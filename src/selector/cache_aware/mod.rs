mod prefix_cache;

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Mutex, RwLock},
};

use tracing::{info, warn};

use crate::{discovery::DiscoveryEvent, models::CreateChatCompletionRequest};

use super::NodeSelector;
use prefix_cache::{PrefixCache, prefix_key};

#[derive(Debug, Clone, Copy)]
pub struct CacheAwareConfig {
    /// Prompts are cut to this many bytes before going in the cache.
    pub max_prefix_bytes: usize,
    /// How much of a prompt has to match a stored one, as a fraction of the
    /// prompt's (truncated) length, to count as a cache hit.
    pub min_match_ratio: f64,
    /// Once the cache holds more prefixes than this, the least recently used
    /// half is evicted.
    pub max_entries: usize,
    /// Consecutive `Unresponsive` events before a node and its prefixes are
    /// dropped. Health checks send one every interval, so with the default 2s
    /// interval, 5 is about 10s of downtime.
    pub max_unresponsive: u32,
}

impl Default for CacheAwareConfig {
    fn default() -> Self {
        Self {
            max_prefix_bytes: 4096,
            min_match_ratio: 0.5,
            max_entries: 100_000,
            max_unresponsive: 5,
        }
    }
}

struct NodeHealth {
    is_responsive: bool,
    consecutive_unresponsive: u32,
}

/// Sends prompts that share a prefix to the node that served that prefix
/// last, so the node's prefix cache gets hits and skips prefill. Cache misses
/// go to the node with the fewest prefixes, which spreads prompts (and so
/// load) evenly.
pub struct CacheAwareSelector {
    config: CacheAwareConfig,
    nodes: RwLock<HashMap<SocketAddr, NodeHealth>>,
    // `select` holds `nodes` while it inserts here, and removing a node
    // releases `nodes` before purging here, so a removed node's prefixes
    // can't be re-added after the purge.
    prefixes: Mutex<PrefixCache>,
}

impl CacheAwareSelector {
    pub fn new(config: CacheAwareConfig) -> Self {
        Self {
            config,
            nodes: RwLock::default(),
            prefixes: Mutex::new(PrefixCache::new(config.max_entries, config.min_match_ratio)),
        }
    }

    // Both handlers log only real changes, since discoverers send an event
    // after every health check. They release the lock before logging, since
    // writing logs is I/O and `select` shouldn't wait on it.

    /// Adds the node if it's new, and clears its unresponsive streak.
    fn handle_responsive(&self, addr: SocketAddr) {
        let mut nodes = self.nodes.write().unwrap();
        let is_new = !nodes.contains_key(&addr);
        let health = nodes.entry(addr).or_insert(NodeHealth {
            is_responsive: false,
            consecutive_unresponsive: 0,
        });
        health.consecutive_unresponsive = 0;
        let was_responsive = std::mem::replace(&mut health.is_responsive, true);
        if was_responsive {
            return;
        }
        let (responsive, num_nodes) = node_counts(&nodes);
        drop(nodes);

        if is_new {
            info!(%addr, responsive, num_nodes, "node added");
        } else {
            info!(%addr, responsive, num_nodes, "node responsive again");
        }
    }

    /// Extends the node's unresponsive streak, and removes it and its prefixes
    /// once the streak reaches `max_unresponsive`. Unknown nodes are ignored,
    /// so failed health checks on a removed node don't bring it back.
    fn handle_unresponsive(&self, addr: SocketAddr) {
        let mut nodes = self.nodes.write().unwrap();
        let Some(health) = nodes.get_mut(&addr) else {
            return;
        };
        health.consecutive_unresponsive += 1;
        let was_responsive = std::mem::replace(&mut health.is_responsive, false);
        let should_remove = health.consecutive_unresponsive >= self.config.max_unresponsive;
        if !was_responsive && !should_remove {
            return; // still down, already logged
        }
        if should_remove {
            nodes.remove(&addr);
        }
        let (responsive, num_nodes) = node_counts(&nodes);
        drop(nodes);

        if should_remove {
            let purged_prefixes = self.prefixes.lock().unwrap().remove_node(addr);
            warn!(
                %addr,
                purged_prefixes,
                responsive,
                num_nodes,
                "node removed after staying unresponsive"
            );
        } else {
            warn!(%addr, responsive, num_nodes, "node marked unresponsive");
        }
        if responsive == 0 {
            warn!(num_nodes, "no responsive nodes, requests will fail");
        }
    }
}

/// (responsive nodes, all nodes), for logging.
fn node_counts(nodes: &HashMap<SocketAddr, NodeHealth>) -> (usize, usize) {
    let responsive = nodes.values().filter(|health| health.is_responsive);
    (responsive.count(), nodes.len())
}

impl NodeSelector for CacheAwareSelector {
    fn handle_discovery_event(&self, event: DiscoveryEvent) {
        match event {
            DiscoveryEvent::Responsive(addr) => self.handle_responsive(addr),
            DiscoveryEvent::Unresponsive(addr) => self.handle_unresponsive(addr),
        }
    }

    /// Picks the node holding the longest matching prefix, falling back to the
    /// eligible node with the fewest prefixes. Records the prompt against the
    /// pick, so a retry moves the prefix to whichever node is tried next.
    fn select(
        &self,
        req: &CreateChatCompletionRequest,
        exclude: &[SocketAddr],
    ) -> Option<SocketAddr> {
        let key = prefix_key(req, self.config.max_prefix_bytes);
        let nodes = self.nodes.read().unwrap();
        let mut prefixes = self.prefixes.lock().unwrap();

        let is_eligible = |addr: &SocketAddr| {
            nodes.get(addr).is_some_and(|health| health.is_responsive) && !exclude.contains(addr)
        };
        let cached = prefixes.matches(&key).find(is_eligible);
        let node = cached.or_else(|| {
            nodes
                .keys()
                .copied()
                .filter(is_eligible)
                .min_by_key(|&addr| (prefixes.prefix_count(addr), addr))
        })?;

        prefixes.insert(key, node);
        Some(node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Content, Message};
    use DiscoveryEvent::*;

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], port))
    }

    fn request(messages: &[&str]) -> CreateChatCompletionRequest {
        CreateChatCompletionRequest {
            model: String::new(),
            messages: messages
                .iter()
                .map(|text| Message {
                    content: Some(Content::TextContent(text.to_string())),
                })
                .collect(),
        }
    }

    fn two_nodes() -> CacheAwareSelector {
        let selector = CacheAwareSelector::new(CacheAwareConfig::default());
        selector.handle_discovery_event(Responsive(addr(1)));
        selector.handle_discovery_event(Responsive(addr(2)));
        selector
    }

    fn pick(selector: &CacheAwareSelector, messages: &[&str]) -> Option<SocketAddr> {
        selector.select(&request(messages), &[])
    }

    fn is_known(selector: &CacheAwareSelector, node: SocketAddr) -> bool {
        selector.nodes.read().unwrap().contains_key(&node)
    }

    fn cached_prefixes(selector: &CacheAwareSelector) -> usize {
        selector.prefixes.lock().unwrap().len()
    }

    const SYSTEM: &str = "You are a helpful assistant who answers questions about the weather.";

    #[test]
    fn same_prompt_goes_to_same_node() {
        let selector = two_nodes();
        let first = pick(&selector, &[SYSTEM, "hello"]);
        for _ in 0..3 {
            assert_eq!(pick(&selector, &[SYSTEM, "hello"]), first);
        }
    }

    #[test]
    fn shared_prefix_goes_to_same_node() {
        let selector = two_nodes();
        let first = pick(&selector, &[SYSTEM, "what's it like in Paris?"]);
        assert_eq!(pick(&selector, &[SYSTEM, "what's it like in Rome?"]), first);
        // A follow-up turn extends the conversation.
        assert_eq!(
            pick(
                &selector,
                &[SYSTEM, "what's it like in Paris?", "sunny", "thanks"]
            ),
            first
        );
    }

    #[test]
    fn unrelated_prompts_spread_across_nodes() {
        let selector = two_nodes();
        let a = pick(&selector, &["alpha alpha alpha"]);
        let b = pick(&selector, &["omega omega omega"]);
        assert_ne!(a, b);
    }

    #[test]
    fn falls_back_when_cached_node_unavailable() {
        let selector = two_nodes();
        let first = pick(&selector, &[SYSTEM, "hello"]).unwrap();
        let other = if first == addr(1) { addr(2) } else { addr(1) };

        assert_eq!(
            selector.select(&request(&[SYSTEM, "hello"]), &[first]),
            Some(other)
        );

        selector.handle_discovery_event(Unresponsive(other));
        selector.handle_discovery_event(Unresponsive(first));
        assert_eq!(pick(&selector, &[SYSTEM, "hello"]), None);
    }

    #[test]
    fn removes_node_after_max_unresponsive() {
        let config = CacheAwareConfig::default();
        let selector = two_nodes();
        let node = pick(&selector, &[SYSTEM, "hello"]).unwrap();
        assert_eq!(cached_prefixes(&selector), 1);

        for _ in 0..config.max_unresponsive {
            selector.handle_discovery_event(Unresponsive(node));
        }
        assert!(!is_known(&selector, node));
        assert_eq!(cached_prefixes(&selector), 0);

        // Further failed health checks don't bring it back, a passing one does.
        selector.handle_discovery_event(Unresponsive(node));
        assert!(!is_known(&selector, node));
        selector.handle_discovery_event(Responsive(node));
        assert!(is_known(&selector, node));
    }

    #[test]
    fn responsive_resets_unresponsive_count() {
        let config = CacheAwareConfig::default();
        let selector = two_nodes();
        for _ in 0..config.max_unresponsive - 1 {
            selector.handle_discovery_event(Unresponsive(addr(1)));
        }
        selector.handle_discovery_event(Responsive(addr(1)));
        for _ in 0..config.max_unresponsive - 1 {
            selector.handle_discovery_event(Unresponsive(addr(1)));
        }
        assert!(is_known(&selector, addr(1)));
    }
}
