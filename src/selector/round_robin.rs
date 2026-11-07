use std::{
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
    nodes: RwLock<Vec<SocketAddr>>,
    next: AtomicUsize,
}

impl NodeSelector for RoundRobinSelector {
    fn handle_discovery_event(&self, event: DiscoveryEvent) {
        let mut nodes = self.nodes.write().unwrap();
        match event {
            DiscoveryEvent::Added(addr) => {
                if !nodes.contains(&addr) {
                    nodes.push(addr);
                }
            }
            DiscoveryEvent::Removed(addr) => {
                nodes.retain(|&node| node != addr);
            }
        }
    }

    fn select(&self, _req: &CreateChatCompletionRequest) -> Option<SocketAddr> {
        let nodes = self.nodes.read().unwrap();
        if nodes.is_empty() {
            return None;
        }

        let index = self.next.fetch_add(1, Ordering::Relaxed) % nodes.len();
        Some(nodes[index])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], port))
    }

    fn request() -> CreateChatCompletionRequest {
        CreateChatCompletionRequest {
            model: String::new(),
            messages: Vec::new(),
        }
    }

    #[test]
    fn rotates_through_nodes() {
        let selector = RoundRobinSelector::default();
        selector.handle_discovery_event(DiscoveryEvent::Added(addr(1)));
        selector.handle_discovery_event(DiscoveryEvent::Added(addr(2)));

        let picks: Vec<_> = (0..4).map(|_| selector.select(&request())).collect();

        assert_eq!(
            picks,
            [addr(1), addr(2), addr(1), addr(2)].map(Some).to_vec()
        );
    }

    #[test]
    fn ignores_duplicate_nodes_and_skips_removed_ones() {
        let selector = RoundRobinSelector::default();
        selector.handle_discovery_event(DiscoveryEvent::Added(addr(1)));
        selector.handle_discovery_event(DiscoveryEvent::Added(addr(1)));
        selector.handle_discovery_event(DiscoveryEvent::Added(addr(2)));
        selector.handle_discovery_event(DiscoveryEvent::Removed(addr(1)));

        assert_eq!(selector.select(&request()), Some(addr(2)));
        assert_eq!(selector.select(&request()), Some(addr(2)));
    }

    #[test]
    fn returns_none_without_nodes() {
        assert_eq!(RoundRobinSelector::default().select(&request()), None);
    }
}
