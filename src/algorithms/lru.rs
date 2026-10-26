use bytes::Bytes;
use super::{now_ms, CacheEntry, CacheImpl, CacheStats, Eviction, IndexedNode, KeyIndex};

struct LruNode {
    key: Box<str>,
    hash: u64,
    entry: CacheEntry,
    prev: Option<usize>,
    next: Option<usize>,
    live: bool,
}

impl IndexedNode for LruNode {
    #[inline]
    fn key(&self) -> &str {
        &self.key
    }
    #[inline]
    fn hash(&self) -> u64 {
        self.hash
    }
}

pub struct LruCache {
    capacity: usize,
    max_bytes: Option<usize>,
    bytes_used: usize,
    sliding: bool,
    index: KeyIndex,
    nodes: Vec<LruNode>,
    free_nodes: Vec<usize>,
    head: Option<usize>, // Most Recently Used
    tail: Option<usize>, // Least Recently Used
    sweep_cursor: usize,
    hits: u64,
    misses: u64,
    evictions: u64,
    expirations: u64,
}

impl LruCache {
    pub fn new(capacity: usize) -> Self {
        Self::new_with_max_bytes(capacity, None)
    }

    pub fn new_with_max_bytes(capacity: usize, max_bytes: Option<usize>) -> Self {
        Self::with_options(capacity, max_bytes, false)
    }

    pub fn with_options(capacity: usize, max_bytes: Option<usize>, sliding: bool) -> Self {
        Self {
            capacity,
            max_bytes,
            bytes_used: 0,
            sliding,
            index: KeyIndex::with_capacity(capacity.min(1024)),
            nodes: Vec::with_capacity(capacity.min(1024)),
            free_nodes: Vec::new(),
            head: None,
            tail: None,
            sweep_cursor: 0,
            hits: 0,
            misses: 0,
            evictions: 0,
            expirations: 0,
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

    /// Unlinks a node from the list and the index, releases its slot and its bytes.
    fn remove_node(&mut self, node_idx: usize) -> (String, CacheEntry) {
        self.detach(node_idx);
        self.index.remove(self.nodes[node_idx].hash, node_idx);
        self.free_nodes.push(node_idx);
        let node = &mut self.nodes[node_idx];
        node.live = false;
        let key = std::mem::take(&mut node.key);
        let entry = std::mem::take(&mut node.entry);
        self.bytes_used -= key.len() + entry.value.len();
        (key.into_string(), entry)
    }

    fn expire_node(&mut self, node_idx: usize) -> Eviction {
        let (key, entry) = self.remove_node(node_idx);
        self.expirations += 1;
        Eviction::expired(key, entry.value)
    }

    fn evict_tail(&mut self) -> Option<Eviction> {
        let t_idx = self.tail?;
        let (key, entry) = self.remove_node(t_idx);
        self.evictions += 1;
        Some(Eviction::evicted(key, entry.value))
    }

    fn insert_new(&mut self, hash: u64, key: &str, entry: CacheEntry) {
        self.bytes_used += key.len() + entry.value.len();
        let node = LruNode { key: key.into(), hash, entry, prev: None, next: None, live: true };
        let idx = if let Some(idx) = self.free_nodes.pop() {
            self.nodes[idx] = node;
            idx
        } else {
            self.nodes.push(node);
            self.nodes.len() - 1
        };
        self.attach_to_head(idx);
        self.index.insert(&self.nodes, hash, idx);
    }
}

impl CacheImpl for LruCache {
    fn get(&mut self, hash: u64, key: &str) -> (Option<Bytes>, Option<Eviction>) {
        if let Some(node_idx) = self.index.find(&self.nodes, hash, key) {
            if self.nodes[node_idx].entry.is_expired() {
                self.misses += 1;
                (None, Some(self.expire_node(node_idx)))
            } else {
                self.move_to_head(node_idx);
                if self.sliding {
                    self.nodes[node_idx].entry.slide();
                }
                self.hits += 1;
                (Some(self.nodes[node_idx].entry.value.clone()), None)
            }
        } else {
            self.misses += 1;
            (None, None)
        }
    }

    fn peek(&mut self, hash: u64, key: &str) -> (Option<Bytes>, Option<Eviction>) {
        if let Some(node_idx) = self.index.find(&self.nodes, hash, key) {
            if self.nodes[node_idx].entry.is_expired() {
                self.misses += 1;
                (None, Some(self.expire_node(node_idx)))
            } else {
                self.hits += 1;
                (Some(self.nodes[node_idx].entry.value.clone()), None)
            }
        } else {
            self.misses += 1;
            (None, None)
        }
    }

    fn has(&mut self, hash: u64, key: &str) -> (bool, Option<Eviction>) {
        if let Some(node_idx) = self.index.find(&self.nodes, hash, key) {
            if self.nodes[node_idx].entry.is_expired() {
                (false, Some(self.expire_node(node_idx)))
            } else {
                (true, None)
            }
        } else {
            (false, None)
        }
    }

    fn set(&mut self, hash: u64, key: &str, value: Bytes, ttl_ms: Option<u64>) -> (Option<Bytes>, Option<Vec<Eviction>>) {
        let new_entry = CacheEntry::new(value, ttl_ms);
        let mut old_value = None;
        let mut evictions: Option<Vec<Eviction>> = None;

        let existing = self.index.find(&self.nodes, hash, key);
        match existing {
            Some(node_idx) if !self.nodes[node_idx].entry.is_expired() => {
                let new_bytes = new_entry.value.len();
                let old_entry = std::mem::replace(&mut self.nodes[node_idx].entry, new_entry);
                self.bytes_used = self.bytes_used + new_bytes - old_entry.value.len();
                self.move_to_head(node_idx);
                old_value = Some(old_entry.value);
            }
            _ => {
                if let Some(node_idx) = existing {
                    let ev = self.expire_node(node_idx);
                    evictions.get_or_insert_with(Vec::new).push(ev);
                }
                if self.index.len() >= self.capacity && self.capacity > 0 {
                    if let Some(ev) = self.evict_tail() {
                        evictions.get_or_insert_with(Vec::new).push(ev);
                    }
                }
                self.insert_new(hash, key, new_entry);
            }
        }

        if let Some(max) = self.max_bytes {
            while self.bytes_used > max && !self.index.is_empty() {
                match self.evict_tail() {
                    Some(ev) => evictions.get_or_insert_with(Vec::new).push(ev),
                    None => break,
                }
            }
        }

        (old_value, evictions)
    }

    fn touch(&mut self, hash: u64, key: &str, ttl_ms: Option<u64>) -> (bool, Option<Eviction>) {
        if let Some(node_idx) = self.index.find(&self.nodes, hash, key) {
            if self.nodes[node_idx].entry.is_expired() {
                (false, Some(self.expire_node(node_idx)))
            } else {
                self.nodes[node_idx].entry = self.nodes[node_idx].entry.with_ttl(ttl_ms);
                self.move_to_head(node_idx);
                (true, None)
            }
        } else {
            (false, None)
        }
    }

    fn delete(&mut self, hash: u64, key: &str) -> bool {
        if let Some(node_idx) = self.index.find(&self.nodes, hash, key) {
            self.remove_node(node_idx);
            true
        } else {
            false
        }
    }

    fn clear(&mut self) {
        self.index.clear();
        self.nodes.clear();
        self.free_nodes.clear();
        self.head = None;
        self.tail = None;
        self.sweep_cursor = 0;
        self.hits = 0;
        self.misses = 0;
        self.evictions = 0;
        self.expirations = 0;
        self.bytes_used = 0;
    }

    fn stats(&self) -> CacheStats {
        CacheStats {
            hits: self.hits,
            misses: self.misses,
            capacity: self.capacity,
            size: self.index.len(),
            bytes_used: self.bytes_used,
            evictions: self.evictions,
            expirations: self.expirations,
        }
    }

    fn ttl_remaining_ms(&self, hash: u64, key: &str) -> Option<u64> {
        self.index
            .find(&self.nodes, hash, key)
            .and_then(|idx| self.nodes[idx].entry.remaining_ms())
    }

    fn purge_expired(&mut self, max_scan: usize, out: &mut Vec<Eviction>) {
        let len = self.nodes.len();
        if len == 0 {
            return;
        }
        let now = now_ms();
        for _ in 0..max_scan.min(len) {
            let idx = self.sweep_cursor % len;
            self.sweep_cursor = idx + 1;
            let node = &self.nodes[idx];
            if node.live && node.entry.is_expired_at(now) {
                let ev = self.expire_node(idx);
                out.push(ev);
            }
        }
    }

    fn keys(&self) -> Vec<String> {
        let mut result = Vec::new();
        let mut curr = self.head;
        while let Some(idx) = curr {
            let node = &self.nodes[idx];
            if !node.entry.is_expired() {
                result.push(node.key.to_string());
            }
            curr = node.next;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::test_hash as h;

    fn set(cache: &mut LruCache, key: &str, value: u8, ttl: Option<u64>) {
        cache.set(h(key), key, Bytes::from(vec![value]), ttl);
    }

    fn get(cache: &mut LruCache, key: &str) -> Option<Vec<u8>> {
        cache.get(h(key), key).0.map(|b| b.to_vec())
    }

    #[test]
    fn test_lru_basic() {
        let mut cache = LruCache::new(2);
        set(&mut cache, "a", 1, None);
        set(&mut cache, "b", 2, None);
        assert_eq!(get(&mut cache, "a"), Some(vec![1]));
        set(&mut cache, "c", 3, None); // should evict "b"
        assert_eq!(get(&mut cache, "b"), None);
        assert_eq!(get(&mut cache, "a"), Some(vec![1]));
        assert_eq!(get(&mut cache, "c"), Some(vec![3]));
    }

    #[test]
    fn test_lru_expiry() {
        let mut cache = LruCache::new(2);
        set(&mut cache, "a", 1, Some(10)); // 10ms
        assert_eq!(get(&mut cache, "a"), Some(vec![1]));
        std::thread::sleep(std::time::Duration::from_millis(15));
        assert_eq!(get(&mut cache, "a"), None);
    }

    #[test]
    fn test_lru_slot_reuse_keeps_index_consistent() {
        let mut cache = LruCache::new(3);
        for round in 0..50u8 {
            let key = format!("k{}", round);
            set(&mut cache, &key, round, None);
            if round % 2 == 0 {
                assert!(cache.delete(h(&key), &key));
            }
        }
        assert_eq!(cache.stats().size, cache.keys().len());
        assert_eq!(cache.bytes_used, cache.keys().iter().map(|k| k.len() + 1).sum::<usize>());
    }

    #[test]
    fn test_lru_purge_expired() {
        let mut cache = LruCache::new(10);
        set(&mut cache, "a", 1, Some(5));
        set(&mut cache, "b", 2, None);
        set(&mut cache, "c", 3, Some(5));
        std::thread::sleep(std::time::Duration::from_millis(15));
        let mut out = Vec::new();
        cache.purge_expired(100, &mut out);
        let mut keys: Vec<_> = out.iter().map(|e| e.key.clone()).collect();
        keys.sort();
        assert_eq!(keys, vec!["a", "c"]);
        assert_eq!(cache.stats().size, 1);
        assert_eq!(cache.bytes_used, 2);
    }

    #[test]
    fn test_lru_sliding() {
        let mut cache = LruCache::with_options(2, None, true);
        set(&mut cache, "a", 1, Some(30));
        for _ in 0..4 {
            std::thread::sleep(std::time::Duration::from_millis(15));
            assert_eq!(get(&mut cache, "a"), Some(vec![1]));
        }
    }
}
