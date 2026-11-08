use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        RwLock,
        atomic::{AtomicUsize, Ordering},
    },
};

use crate::{discovery::DiscoveryEvent, models::CreateChatCompletionRequest};

use super::NodeSelector;

#[derive(Default)]
pub struct RoundRobinSelector {
    /// Each known node and whether it's responsive.
    nodes: RwLock<HashMap<SocketAddr, bool>>,
    next: AtomicUsize,
}

impl NodeSelector for RoundRobinSelector {
    fn handle_discovery_event(&self, event: DiscoveryEvent) {
        let mut nodes = self.nodes.write().unwrap();
        match event {
            DiscoveryEvent::Added(addr) => {
                nodes.insert(addr, true);
            }
            DiscoveryEvent::Responsive(addr) => {
                if let Some(responsive) = nodes.get_mut(&addr) {
                    *responsive = true;
                }
            }
            DiscoveryEvent::Unresponsive(addr) => {
                if let Some(responsive) = nodes.get_mut(&addr) {
                    *responsive = false;
                }
            }
            DiscoveryEvent::Removed(addr) => {
                nodes.remove(&addr);
            }
        }
    }

    /// The map's order is arbitrary but only changes when nodes are added or
    /// removed, so picks rotate evenly between those changes.
    fn select(
        &self,
        _req: &CreateChatCompletionRequest,
        exclude: &[SocketAddr],
    ) -> Option<SocketAddr> {
        let nodes = self.nodes.read().unwrap();
        let mut candidates = nodes
            .iter()
            .filter(|&(addr, &responsive)| responsive && !exclude.contains(addr))
            .map(|(&addr, _)| addr);

        let count = candidates.clone().count();
        if count == 0 {
            return None;
        }

        candidates.nth(self.next.fetch_add(1, Ordering::Relaxed) % count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use DiscoveryEvent::*;

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], port))
    }

    fn request() -> CreateChatCompletionRequest {
        CreateChatCompletionRequest {
            model: String::new(),
            messages: Vec::new(),
        }
    }

    fn selector_with(events: &[DiscoveryEvent]) -> RoundRobinSelector {
        let selector = RoundRobinSelector::default();
        for &event in events {
            selector.handle_discovery_event(event);
        }
        selector
    }

    fn picks(selector: &RoundRobinSelector, n: usize) -> Vec<Option<SocketAddr>> {
        (0..n).map(|_| selector.select(&request(), &[])).collect()
    }

    /// Two nodes alternate, in whichever order the map holds them.
    fn assert_alternates(picks: &[Option<SocketAddr>], a: SocketAddr, b: SocketAddr) {
        let (first, second) = if picks[0] == Some(a) { (a, b) } else { (b, a) };
        for (i, pick) in picks.iter().enumerate() {
            let expected = if i % 2 == 0 { first } else { second };
            assert_eq!(*pick, Some(expected), "picks were {picks:?}");
        }
    }

    #[test]
    fn rotates_through_responsive_nodes() {
        let selector = selector_with(&[Added(addr(1)), Added(addr(2))]);
        assert_alternates(&picks(&selector, 4), addr(1), addr(2));
    }

    #[test]
    fn skips_unresponsive_nodes_until_responsive_again() {
        let selector = selector_with(&[Added(addr(1)), Added(addr(2)), Unresponsive(addr(1))]);
        assert_eq!(picks(&selector, 2), [Some(addr(2)), Some(addr(2))]);

        selector.handle_discovery_event(Responsive(addr(1)));
        assert_alternates(&picks(&selector, 4), addr(1), addr(2));
    }

    #[test]
    fn ignores_events_for_unknown_nodes() {
        let selector = selector_with(&[Responsive(addr(1)), Unresponsive(addr(2))]);
        assert_eq!(selector.select(&request(), &[]), None);
    }

    #[test]
    fn removed_nodes_stay_removed() {
        let selector = selector_with(&[Added(addr(1)), Removed(addr(1)), Responsive(addr(1))]);
        assert_eq!(selector.select(&request(), &[]), None);
    }

    #[test]
    fn honours_exclude() {
        let selector = selector_with(&[Added(addr(1)), Added(addr(2))]);

        for _ in 0..3 {
            assert_eq!(selector.select(&request(), &[addr(1)]), Some(addr(2)));
        }
        assert_eq!(selector.select(&request(), &[addr(1), addr(2)]), None);
    }
}
