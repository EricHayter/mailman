use std::{collections::HashMap, net::SocketAddr};

use patricia_tree::PatriciaMap;

use crate::models::{Content, CreateChatCompletionRequest};

/// Remembers which node last served each prompt prefix, so prompts that share
/// a prefix can go back to it and hit its prefix cache.
pub struct PrefixCache {
    /// Prompt prefix -> node that most recently served it.
    tree: PatriciaMap<MapEntry>,
    /// Entries in the tree pointing at each node.
    counts: HashMap<SocketAddr, usize>,
    /// Bumped on every insert, to order entries for eviction.
    clock: u64,
    max_entries: usize,
    min_match_ratio: f64,
}

struct MapEntry {
    node: SocketAddr,
    /// `clock` when this prefix was last inserted.
    last_used: u64,
}

impl PrefixCache {
    /// Once the tree holds more than `max_entries`, the least recently used
    /// half is evicted. A lookup only counts as a hit if it matches at least
    /// `min_match_ratio` of the key.
    pub fn new(max_entries: usize, min_match_ratio: f64) -> Self {
        Self {
            tree: PatriciaMap::new(),
            counts: HashMap::new(),
            clock: 0,
            max_entries,
            min_match_ratio,
        }
    }

    /// Nodes holding the longest stored match for `key`, or none if the match
    /// is too short to count as a hit.
    pub fn matches(&self, key: &[u8]) -> impl Iterator<Item = SocketAddr> {
        let len = self.tree.longest_common_prefix_len(key);
        let is_hit = !key.is_empty() && len as f64 / key.len() as f64 >= self.min_match_ratio;
        is_hit
            .then(|| {
                self.tree
                    .iter_prefix(&key[..len])
                    .map(|(_, entry)| entry.node)
            })
            .into_iter()
            .flatten()
    }

    /// Records that `node` served `key`, replacing whichever node served it
    /// before.
    pub fn insert(&mut self, key: Vec<u8>, node: SocketAddr) {
        self.clock += 1;
        let entry = MapEntry {
            node,
            last_used: self.clock,
        };
        if let Some(old) = self.tree.insert(key, entry) {
            self.uncount(old.node);
        }
        *self.counts.entry(node).or_default() += 1;

        if self.tree.len() > self.max_entries {
            self.evict_oldest_half();
        }
    }

    pub fn prefix_count(&self, node: SocketAddr) -> usize {
        self.counts.get(&node).copied().unwrap_or(0)
    }

    /// Drops every prefix pointing at `node`. Returns how many there were.
    pub fn remove_node(&mut self, node: SocketAddr) -> usize {
        let keys: Vec<_> = self
            .tree
            .iter()
            .filter(|(_, entry)| entry.node == node)
            .map(|(key, _)| key)
            .collect();
        for key in &keys {
            self.tree.remove(key);
        }
        self.counts.remove(&node);
        keys.len()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.tree.len()
    }

    fn uncount(&mut self, node: SocketAddr) {
        if let Some(count) = self.counts.get_mut(&node) {
            *count -= 1;
            if *count == 0 {
                self.counts.remove(&node);
            }
        }
    }

    fn evict_oldest_half(&mut self) {
        let mut entries: Vec<_> = self
            .tree
            .iter()
            .map(|(key, entry)| (entry.last_used, key))
            .collect();
        entries.sort_unstable_by_key(|&(last_used, _)| last_used);
        let evict = entries.len() / 2;
        for (_, key) in entries.into_iter().take(evict) {
            if let Some(entry) = self.tree.remove(key) {
                self.uncount(entry.node);
            }
        }
    }
}

/// The cache key for a request: each message's text followed by a `\0`, so
/// message boundaries can't make two different conversations look the same,
/// cut to `max` bytes. Past a few KB, two prompts that match that far almost
/// certainly share the rest of their prefix too, and it bounds the memory each
/// entry takes.
pub fn prefix_key(req: &CreateChatCompletionRequest, max: usize) -> Vec<u8> {
    let mut key = Vec::new();
    for message in &req.messages {
        match &message.content {
            Some(Content::TextContent(text)) => key.extend_from_slice(text.as_bytes()),
            Some(Content::ArrayOfContentParts(parts)) => {
                for part in parts {
                    key.extend_from_slice(part.text.as_bytes());
                }
            }
            None => {}
        }
        key.push(0);
        if key.len() >= max {
            break;
        }
    }
    key.truncate(max);
    key
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Message;

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], port))
    }

    fn matches(cache: &PrefixCache, key: &[u8]) -> Vec<SocketAddr> {
        cache.matches(key).collect()
    }

    #[test]
    fn matches_longest_shared_prefix() {
        let mut cache = PrefixCache::new(100, 0.5);
        cache.insert(b"system prompt\0paris".to_vec(), addr(1));
        cache.insert(b"other\0".to_vec(), addr(2));

        assert_eq!(matches(&cache, b"system prompt\0rome"), [addr(1)]);
        assert_eq!(matches(&cache, b"nothing alike"), []);
    }

    #[test]
    fn insert_moves_prefix_to_new_node() {
        let mut cache = PrefixCache::new(100, 0.5);
        cache.insert(b"key".to_vec(), addr(1));
        cache.insert(b"key".to_vec(), addr(2));

        assert_eq!(matches(&cache, b"key"), [addr(2)]);
        assert_eq!(cache.prefix_count(addr(1)), 0);
        assert_eq!(cache.prefix_count(addr(2)), 1);
    }

    #[test]
    fn remove_node_drops_its_prefixes() {
        let mut cache = PrefixCache::new(100, 0.5);
        cache.insert(b"a".to_vec(), addr(1));
        cache.insert(b"b".to_vec(), addr(1));
        cache.insert(b"c".to_vec(), addr(2));

        assert_eq!(cache.remove_node(addr(1)), 2);
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.prefix_count(addr(1)), 0);
    }

    #[test]
    fn evicts_oldest_entries() {
        let mut cache = PrefixCache::new(4, 0.5);
        for key in ["a", "b", "c", "d", "e"] {
            cache.insert(key.as_bytes().to_vec(), addr(1));
        }

        assert!(cache.len() <= 4);
        assert_eq!(matches(&cache, b"e"), [addr(1)]);
        assert_eq!(matches(&cache, b"a"), []);
        assert_eq!(cache.prefix_count(addr(1)), cache.len());
    }

    #[test]
    fn truncates_key() {
        let request = |messages: &[&str]| CreateChatCompletionRequest {
            model: String::new(),
            messages: messages
                .iter()
                .map(|text| Message {
                    content: Some(Content::TextContent(text.to_string())),
                })
                .collect(),
        };
        let long = "x".repeat(100);
        assert_eq!(prefix_key(&request(&[&long, "more"]), 10), b"xxxxxxxxxx");
        assert_eq!(prefix_key(&request(&["ab", "c"]), 100), b"ab\0c\0");
    }
}
