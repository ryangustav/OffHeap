use std::collections::HashMap;
use super::{CacheImpl, CacheEntry, CacheStats};

struct LruNode {
    key: String,
    entry: CacheEntry,
    prev: Option<usize>,
    next: Option<usize>,
}

pub struct LruCache {
    capacity: usize,
    map: HashMap<String, usize>,
    nodes: Vec<LruNode>,
    free_nodes: Vec<usize>,
    head: Option<usize>, // Most Recently Used
    tail: Option<usize>, // Least Recently Used
    hits: u64,
    misses: u64,
}

impl LruCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            map: HashMap::with_capacity(capacity),
            nodes: Vec::with_capacity(capacity),
            free_nodes: Vec::new(),
            head: None,
            tail: None,
            hits: 0,
            misses: 0,
        }
    }

    fn detach(&mut self, node_idx: usize) {
        let prev = self.nodes[node_idx].prev;
        let next = self.nodes[node_idx].next;

        if let Some(p) = prev {
            self.nodes[p].next = next;
        } else {
            self.head = next;
        }

        if let Some(n) = next {
            self.nodes[n].prev = prev;
        } else {
            self.tail = prev;
        }

        self.nodes[node_idx].prev = None;
        self.nodes[node_idx].next = None;
    }

    fn attach_to_head(&mut self, node_idx: usize) {
        self.nodes[node_idx].prev = None;
        self.nodes[node_idx].next = self.head;

        if let Some(h) = self.head {
            self.nodes[h].prev = Some(node_idx);
        } else {
            self.tail = Some(node_idx);
        }

        self.head = Some(node_idx);
    }

    fn move_to_head(&mut self, node_idx: usize) {
        if self.head == Some(node_idx) {
            return;
        }
        self.detach(node_idx);
        self.attach_to_head(node_idx);
    }

    fn remove_node(&mut self, node_idx: usize) -> (String, CacheEntry) {
        self.detach(node_idx);
        self.free_nodes.push(node_idx);
        let key = std::mem::take(&mut self.nodes[node_idx].key);
        let entry = std::mem::replace(&mut self.nodes[node_idx].entry, CacheEntry::new(Vec::new(), None));
        (key, entry)
    }
}

impl CacheImpl for LruCache {
    fn get(&mut self, key: &str) -> Option<Vec<u8>> {
        if let Some(&node_idx) = self.map.get(key) {
            if self.nodes[node_idx].entry.is_expired() {
                self.remove_node(node_idx);
                self.map.remove(key);
                self.misses += 1;
                None
            } else {
                self.move_to_head(node_idx);
                self.hits += 1;
                Some(self.nodes[node_idx].entry.value.clone())
            }
        } else {
            self.misses += 1;
            None
        }
    }

    fn set(&mut self, key: &str, value: Vec<u8>, ttl_ms: Option<u64>) -> Option<Vec<u8>> {
        if let Some(&node_idx) = self.map.get(key) {
            // Key exists, update it
            let old_value = std::mem::replace(
                &mut self.nodes[node_idx].entry,
                CacheEntry::new(value, ttl_ms),
            ).value;
            self.move_to_head(node_idx);
            Some(old_value)
        } else {
            // If full, evict the LRU (tail)
            if self.map.len() >= self.capacity && self.capacity > 0 {
                if let Some(t_idx) = self.tail {
                    let (evicted_key, _) = self.remove_node(t_idx);
                    self.map.remove(&evicted_key);
                }
            }

            // Insert new entry
            let node_idx = if let Some(idx) = self.free_nodes.pop() {
                self.nodes[idx].key = key.to_string();
                self.nodes[idx].entry = CacheEntry::new(value, ttl_ms);
                idx
            } else {
                let idx = self.nodes.len();
                self.nodes.push(LruNode {
                    key: key.to_string(),
                    entry: CacheEntry::new(value, ttl_ms),
                    prev: None,
                    next: None,
                });
                idx
            };

            self.attach_to_head(node_idx);
            self.map.insert(key.to_string(), node_idx);
            None
        }
    }

    fn delete(&mut self, key: &str) -> bool {
        if let Some(&node_idx) = self.map.get(key) {
            self.remove_node(node_idx);
            self.map.remove(key);
            true
        } else {
            false
        }
    }

    fn clear(&mut self) {
        self.map.clear();
        self.nodes.clear();
        self.free_nodes.clear();
        self.head = None;
        self.tail = None;
        self.hits = 0;
        self.misses = 0;
    }

    fn stats(&self) -> CacheStats {
        CacheStats {
            hits: self.hits,
            misses: self.misses,
            capacity: self.capacity,
            size: self.map.len(),
        }
    }

    fn keys(&self) -> Vec<String> {
        let mut result = Vec::new();
        let mut curr = self.head;
        while let Some(idx) = curr {
            let node = &self.nodes[idx];
            if !node.entry.is_expired() {
                result.push(node.key.clone());
            }
            curr = node.next;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lru_basic() {
        let mut cache = LruCache::new(2);
        cache.set("a", vec![1], None);
        cache.set("b", vec![2], None);
        assert_eq!(cache.get("a"), Some(vec![1]));
        cache.set("c", vec![3], None); // should evict "b"
        assert_eq!(cache.get("b"), None);
        assert_eq!(cache.get("a"), Some(vec![1]));
        assert_eq!(cache.get("c"), Some(vec![3]));
    }

    #[test]
    fn test_lru_expiry() {
        let mut cache = LruCache::new(2);
        cache.set("a", vec![1], Some(10)); // 10ms
        assert_eq!(cache.get("a"), Some(vec![1]));
        std::thread::sleep(std::time::Duration::from_millis(15));
        assert_eq!(cache.get("a"), None);
    }
}
