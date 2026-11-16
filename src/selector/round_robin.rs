use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        RwLock,
        atomic::{AtomicUsize, Ordering},
    },
};

use tracing::{info, warn};

use crate::{discovery::DiscoveryEvent, models::CreateChatCompletionRequest};

use super::NodeSelector;

#[derive(Default)]
pub struct RoundRobinSelector {
    /// Each known node and whether it's responsive.
    nodes: RwLock<HashMap<SocketAddr, bool>>,
    next: AtomicUsize,
}

impl RoundRobinSelector {
    // The handlers log only real changes, since discoverers send an event
    // after every health check. They release the lock before logging, since
    // writing logs is I/O and `select` shouldn't wait on it.

    /// Adds the node if it's new, and marks it responsive.
    fn handle_responsive(&self, addr: SocketAddr) {
        let mut nodes = self.nodes.write().unwrap();
        let prev = nodes.insert(addr, true);
        if prev == Some(true) {
            return;
        }
        let (responsive, num_nodes) = node_counts(&nodes);
        drop(nodes);

        if prev.is_none() {
            info!(%addr, responsive, num_nodes, "node added");
        } else {
            info!(%addr, responsive, num_nodes, "node responsive again");
        }
    }

    /// Marks the node unresponsive. Unknown nodes are ignored, so a late
    /// failure report doesn't bring back a removed node.
    fn handle_unresponsive(&self, addr: SocketAddr) {
        let mut nodes = self.nodes.write().unwrap();
        let Some(is_responsive) = nodes.get_mut(&addr) else {
            return;
        };
        if !std::mem::replace(is_responsive, false) {
            return; // still down, already logged
        }
        let (responsive, num_nodes) = node_counts(&nodes);
        drop(nodes);

        warn!(%addr, responsive, num_nodes, "node marked unresponsive");
        if responsive == 0 {
            warn!(num_nodes, "no responsive nodes, requests will fail");
        }
    }

    /// Drops the node. Unknown nodes are ignored.
    fn handle_removed(&self, addr: SocketAddr) {
        let mut nodes = self.nodes.write().unwrap();
        if nodes.remove(&addr).is_none() {
            return;
        }
        let (responsive, num_nodes) = node_counts(&nodes);
        drop(nodes);

        warn!(%addr, responsive, num_nodes, "node removed by discovery");
        if responsive == 0 {
            warn!(num_nodes, "no responsive nodes, requests will fail");
        }
    }
}

/// (responsive nodes, all nodes), for logging.
fn node_counts(nodes: &HashMap<SocketAddr, bool>) -> (usize, usize) {
    let responsive = nodes.values().filter(|&&is_responsive| is_responsive);
    (responsive.count(), nodes.len())
}

impl NodeSelector for RoundRobinSelector {
    fn handle_discovery_event(&self, event: DiscoveryEvent) {
        match event {
            DiscoveryEvent::Responsive(addr) => self.handle_responsive(addr),
            DiscoveryEvent::Unresponsive(addr) => self.handle_unresponsive(addr),
            DiscoveryEvent::Removed(addr) => self.handle_removed(addr),
        }
    }

    /// The map's order is arbitrary but only changes when a node is added, so
    /// picks rotate evenly between additions.
    fn select(
        &self,
        _req: &CreateChatCompletionRequest,
        exclude: &[SocketAddr],
    ) -> Option<SocketAddr> {
        let nodes = self.nodes.read().unwrap();
        let mut candidates = nodes
            .iter()
            .filter(|&(addr, &is_responsive)| is_responsive && !exclude.contains(addr))
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
        let selector = selector_with(&[Responsive(addr(1)), Responsive(addr(2))]);
        assert_alternates(&picks(&selector, 4), addr(1), addr(2));
    }

    #[test]
    fn skips_unresponsive_nodes_until_responsive_again() {
        let selector = selector_with(&[
            Responsive(addr(1)),
            Responsive(addr(2)),
            Unresponsive(addr(1)),
        ]);
        assert_eq!(picks(&selector, 2), [Some(addr(2)), Some(addr(2))]);

        selector.handle_discovery_event(Responsive(addr(1)));
        assert_alternates(&picks(&selector, 4), addr(1), addr(2));
    }

    #[test]
    fn adds_unknown_nodes_once_responsive() {
        let selector = selector_with(&[Unresponsive(addr(1)), Responsive(addr(2))]);
        assert_eq!(picks(&selector, 2), [Some(addr(2)), Some(addr(2))]);
        assert!(!selector.nodes.read().unwrap().contains_key(&addr(1)));

        selector.handle_discovery_event(Responsive(addr(1)));
        assert_alternates(&picks(&selector, 4), addr(1), addr(2));
    }

    #[test]
    fn removed_nodes_stay_removed_until_responsive() {
        let selector = selector_with(&[Responsive(addr(1)), Responsive(addr(2))]);
        selector.handle_discovery_event(Removed(addr(1)));
        selector.handle_discovery_event(Unresponsive(addr(1)));
        assert_eq!(picks(&selector, 2), [Some(addr(2)), Some(addr(2))]);
        assert!(!selector.nodes.read().unwrap().contains_key(&addr(1)));

        selector.handle_discovery_event(Responsive(addr(1)));
        assert_alternates(&picks(&selector, 4), addr(1), addr(2));
    }

    #[test]
    fn honours_exclude() {
        let selector = selector_with(&[Responsive(addr(1)), Responsive(addr(2))]);

        for _ in 0..3 {
            assert_eq!(selector.select(&request(), &[addr(1)]), Some(addr(2)));
        }
        assert_eq!(selector.select(&request(), &[addr(1), addr(2)]), None);
    }
}
