use bytes::Bytes;
use super::{now_ms, CacheEntry, CacheImpl, CacheStats, Eviction, IndexedNode, KeyIndex};

// Count-Min Sketch with 4-bit counters and aging.
// Sized dynamically relative to cache capacity.
struct FrequencySketch {
    table: Vec<u8>,
    mask: usize,
    sample_size: usize,
    additions: usize,
}

/// Re-mixes the shared key hash. All keys of a shard agree on the bits used for shard
/// routing, so the sketch must not consume them raw.
#[inline]
fn sketch_hash(hash: u64) -> u64 {
    let x = (hash ^ (hash >> 31)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    x ^ (x >> 29)
}

impl FrequencySketch {
    fn new(capacity: usize) -> Self {
        // Number of counters: next power of 2 of capacity * 4, min 128
        let counters = (capacity * 4).next_power_of_two().max(128);
        let bytes = counters / 2;
        Self {
            table: vec![0; bytes],
            mask: counters - 1,
            sample_size: (capacity * 10).max(1024),
            additions: 0,
        }
    }

    fn increment(&mut self, key_hash: u64) {
        let hash = sketch_hash(key_hash);
        let h1 = (hash & 0xFFFFFFFF) as usize;
        let h2 = (hash >> 32) as usize;

        let mut added = false;
        for i in 0..4 {
            let idx = (h1.wrapping_add(i * h2)) & self.mask;
            if self.increment_at(idx) {
                added = true;
            }
        }

        if added {
            self.additions += 1;
            if self.additions >= self.sample_size {
                self.decay();
            }
        }
    }

    fn increment_at(&mut self, idx: usize) -> bool {
        let byte_idx = idx / 2;
        let is_high = idx % 2 == 1;
        let val = self.table[byte_idx];

        if is_high {
            let count = val >> 4;
            if count < 15 {
                self.table[byte_idx] = (val & 0x0F) | ((count + 1) << 4);
                return true;
            }
        } else {
            let count = val & 0x0F;
            if count < 15 {
                self.table[byte_idx] = (val & 0xF0) | (count + 1);
                return true;
            }
        }
        false
    }

    fn estimate(&self, key_hash: u64) -> u8 {
        let hash = sketch_hash(key_hash);
        let h1 = (hash & 0xFFFFFFFF) as usize;
        let h2 = (hash >> 32) as usize;

        let mut min_val = 15;
        for i in 0..4 {
            let idx = (h1.wrapping_add(i * h2)) & self.mask;
            let val = self.counter_at(idx);
            if val < min_val {
                min_val = val;
            }
        }
        min_val
    }

    fn counter_at(&self, idx: usize) -> u8 {
        let byte_idx = idx / 2;
        let is_high = idx % 2 == 1;
        let val = self.table[byte_idx];
        if is_high {
            val >> 4
        } else {
            val & 0x0F
        }
    }

    fn decay(&mut self) {
        for val in self.table.iter_mut() {
            let high = (*val >> 4) >> 1;
            let low = (*val & 0x0F) >> 1;
            *val = (high << 4) | low;
        }
        self.additions = 0;
    }
}

#[derive(Copy, Clone, PartialEq, Debug)]
enum TinyLfuList {
    Window,
    Probation,
    Protected,
}

struct TinyLfuNode {
    key: Box<str>,
    hash: u64,
    entry: CacheEntry,
    list: TinyLfuList,
    prev: Option<usize>,
    next: Option<usize>,
    live: bool,
}

impl IndexedNode for TinyLfuNode {
    #[inline]
    fn key(&self) -> &str {
        &self.key
    }
    #[inline]
    fn hash(&self) -> u64 {
        self.hash
    }
}

struct TinyLfuListMetadata {
    head: Option<usize>,
    tail: Option<usize>,
    len: usize,
    capacity: usize,
}

impl TinyLfuListMetadata {
    fn new(capacity: usize) -> Self {
        Self {
            head: None,
            tail: None,
            len: 0,
            capacity,
        }
    }

    fn reset(&mut self) {
        self.head = None;
        self.tail = None;
        self.len = 0;
    }
}

pub struct TinyLfuCache {
    capacity: usize,
    max_bytes: Option<usize>,
    bytes_used: usize,
    sliding: bool,
    index: KeyIndex,
    nodes: Vec<TinyLfuNode>,
    free_nodes: Vec<usize>,

    window: TinyLfuListMetadata,
    probation: TinyLfuListMetadata,
    protected: TinyLfuListMetadata,

    sketch: FrequencySketch,
    sweep_cursor: usize,
    hits: u64,
    misses: u64,
    evictions: u64,
    expirations: u64,
}

impl TinyLfuCache {
    pub fn new(capacity: usize) -> Self {
        Self::new_with_max_bytes(capacity, None)
    }

    pub fn new_with_max_bytes(capacity: usize, max_bytes: Option<usize>) -> Self {
        Self::with_options(capacity, max_bytes, false)
    }

    pub fn with_options(capacity: usize, max_bytes: Option<usize>, sliding: bool) -> Self {
        let window_cap = (capacity / 100).max(1);
        let main_cap = capacity.saturating_sub(window_cap);

        let (probation_cap, protected_cap) = if main_cap > 0 {
            let prob = (main_cap * 20 / 100).max(1);
            let prot = main_cap.saturating_sub(prob);
            (prob, prot)
        } else {
            (0, 0)
        };

        Self {
            capacity,
            max_bytes,
            bytes_used: 0,
            sliding,
            index: KeyIndex::with_capacity(capacity),
            nodes: Vec::with_capacity(capacity),
            free_nodes: Vec::new(),
            window: TinyLfuListMetadata::new(window_cap),
            probation: TinyLfuListMetadata::new(probation_cap),
            protected: TinyLfuListMetadata::new(protected_cap),
            sketch: FrequencySketch::new(capacity),
            sweep_cursor: 0,
            hits: 0,
            misses: 0,
            evictions: 0,
            expirations: 0,
        }
    }

    fn node_bytes(&self, idx: usize) -> usize {
        let node = &self.nodes[idx];
        node.key.len() + node.entry.value.len()
    }

    fn detach(&mut self, idx: usize) {
        let prev = self.nodes[idx].prev;
        let next = self.nodes[idx].next;
        let list = self.nodes[idx].list;

        let meta = match list {
            TinyLfuList::Window => &mut self.window,
            TinyLfuList::Probation => &mut self.probation,
            TinyLfuList::Protected => &mut self.protected,
        };

        if let Some(p) = prev {
            self.nodes[p].next = next;
        } else {
            meta.head = next;
        }

        if let Some(n) = next {
            self.nodes[n].prev = prev;
        } else {
            meta.tail = prev;
        }

        meta.len -= 1;
        self.nodes[idx].prev = None;
        self.nodes[idx].next = None;
    }

    fn attach_to_head(&mut self, idx: usize, list: TinyLfuList) {
        self.nodes[idx].list = list;
        let meta = match list {
            TinyLfuList::Window => &mut self.window,
            TinyLfuList::Probation => &mut self.probation,
            TinyLfuList::Protected => &mut self.protected,
        };

        self.nodes[idx].prev = None;
        self.nodes[idx].next = meta.head;

        if let Some(h) = meta.head {
            self.nodes[h].prev = Some(idx);
        } else {
            meta.tail = Some(idx);
        }

        meta.head = Some(idx);
        meta.len += 1;
    }

    /// Unlinks a node from its list (unless already detached) and the index, frees its slot.
    fn remove_completely(&mut self, idx: usize, already_detached: bool) -> (String, CacheEntry) {
        let bytes = self.node_bytes(idx);
        self.bytes_used -= bytes;
        if !already_detached {
            self.detach(idx);
        }
        self.index.remove(self.nodes[idx].hash, idx);
        self.free_nodes.push(idx);
        let node = &mut self.nodes[idx];
        node.live = false;
        let key = std::mem::take(&mut node.key);
        let entry = std::mem::take(&mut node.entry);
        (key.into_string(), entry)
    }

    fn expire_node(&mut self, idx: usize) -> Eviction {
        let (key, entry) = self.remove_completely(idx, false);
        self.expirations += 1;
        Eviction::expired(key, entry.value)
    }

    fn evict_node(&mut self, idx: usize, already_detached: bool) -> Eviction {
        let (key, entry) = self.remove_completely(idx, already_detached);
        self.evictions += 1;
        Eviction::evicted(key, entry.value)
    }

    /// Recency/frequency promotion shared by reads and in-place updates.
    fn promote(&mut self, idx: usize) {
        match self.nodes[idx].list {
            TinyLfuList::Window => {
                self.detach(idx);
                self.attach_to_head(idx, TinyLfuList::Window);
            }
            TinyLfuList::Probation => {
                self.detach(idx);
                self.attach_to_head(idx, TinyLfuList::Protected);

                if self.protected.len > self.protected.capacity {
                    if let Some(prot_tail) = self.protected.tail {
                        self.detach(prot_tail);
                        self.attach_to_head(prot_tail, TinyLfuList::Probation);
                    }
                }
            }
            TinyLfuList::Protected => {
                self.detach(idx);
                self.attach_to_head(idx, TinyLfuList::Protected);
            }
        }
    }
}

impl CacheImpl for TinyLfuCache {
    fn get(&mut self, hash: u64, key: &str) -> (Option<Bytes>, Option<Eviction>) {
        self.sketch.increment(hash);

        if let Some(idx) = self.index.find(&self.nodes, hash, key) {
            if self.nodes[idx].entry.is_expired() {
                self.misses += 1;
                (None, Some(self.expire_node(idx)))
            } else {
                self.promote(idx);
                if self.sliding {
                    self.nodes[idx].entry.slide();
                }
                self.hits += 1;
                (Some(self.nodes[idx].entry.value.clone()), None)
            }
        } else {
            self.misses += 1;
            (None, None)
        }
    }

    fn peek(&mut self, hash: u64, key: &str) -> (Option<Bytes>, Option<Eviction>) {
        if let Some(idx) = self.index.find(&self.nodes, hash, key) {
            if self.nodes[idx].entry.is_expired() {
                self.misses += 1;
                (None, Some(self.expire_node(idx)))
            } else {
                self.hits += 1;
                (Some(self.nodes[idx].entry.value.clone()), None)
            }
        } else {
            self.misses += 1;
            (None, None)
        }
    }

    fn has(&mut self, hash: u64, key: &str) -> (bool, Option<Eviction>) {
        if let Some(idx) = self.index.find(&self.nodes, hash, key) {
            if self.nodes[idx].entry.is_expired() {
                (false, Some(self.expire_node(idx)))
            } else {
                (true, None)
            }
        } else {
            (false, None)
        }
    }

    fn set(&mut self, hash: u64, key: &str, value: Bytes, ttl_ms: Option<u64>) -> (Option<Bytes>, Option<Vec<Eviction>>) {
        self.sketch.increment(hash);

        let new_entry = CacheEntry::new(value, ttl_ms);
        let new_bytes = key.len() + new_entry.value.len();
        let mut old_value = None;
        let mut evictions: Option<Vec<Eviction>> = None;

        if let Some(idx) = self.index.find(&self.nodes, hash, key) {
            let old_bytes = self.node_bytes(idx);
            old_value = Some(std::mem::replace(&mut self.nodes[idx].entry, new_entry).value);
            self.bytes_used = self.bytes_used + new_bytes - old_bytes;
            self.promote(idx);
        } else {
            if self.capacity == 0 {
                return (None, None);
            }

            let node = TinyLfuNode {
                key: key.into(),
                hash,
                entry: new_entry,
                list: TinyLfuList::Window,
                prev: None,
                next: None,
                live: true,
            };
            let new_idx = if let Some(free_idx) = self.free_nodes.pop() {
                self.nodes[free_idx] = node;
                free_idx
            } else {
                self.nodes.push(node);
                self.nodes.len() - 1
            };

            self.bytes_used += new_bytes;
            self.attach_to_head(new_idx, TinyLfuList::Window);
            self.index.insert(&self.nodes, hash, new_idx);

            if self.window.len > self.window.capacity {
                if let Some(win_tail) = self.window.tail {
                    let candidate_idx = win_tail;
                    self.detach(candidate_idx);

                    let main_has_capacity = self.probation.capacity > 0;
                    if main_has_capacity {
                        let total_size = self.window.len + self.probation.len + self.protected.len;
                        if total_size + 1 <= self.capacity {
                            self.attach_to_head(candidate_idx, TinyLfuList::Probation);
                        } else if let Some(victim_idx) = self.probation.tail {
                            let candidate_freq = self.sketch.estimate(self.nodes[candidate_idx].hash);
                            let victim_freq = self.sketch.estimate(self.nodes[victim_idx].hash);

                            if candidate_freq > victim_freq {
                                let ev = self.evict_node(victim_idx, false);
                                self.attach_to_head(candidate_idx, TinyLfuList::Probation);
                                evictions.get_or_insert_with(Vec::new).push(ev);
                            } else {
                                let ev = self.evict_node(candidate_idx, true);
                                evictions.get_or_insert_with(Vec::new).push(ev);
                            }
                        } else {
                            self.attach_to_head(candidate_idx, TinyLfuList::Probation);
                        }
                    } else {
                        let ev = self.evict_node(candidate_idx, true);
                        evictions.get_or_insert_with(Vec::new).push(ev);
                    }
                }
            }
        }

        if let Some(max) = self.max_bytes {
            while self.bytes_used > max && (self.probation.len > 0 || self.window.len > 0) {
                let victim = self.probation.tail.or(self.window.tail);
                match victim {
                    Some(idx) => {
                        let ev = self.evict_node(idx, false);
                        evictions.get_or_insert_with(Vec::new).push(ev);
                    }
                    None => break,
                }
            }
        }

        (old_value, evictions)
    }

    fn touch(&mut self, hash: u64, key: &str, ttl_ms: Option<u64>) -> (bool, Option<Eviction>) {
        if let Some(idx) = self.index.find(&self.nodes, hash, key) {
            if self.nodes[idx].entry.is_expired() {
                (false, Some(self.expire_node(idx)))
            } else {
                self.nodes[idx].entry = self.nodes[idx].entry.with_ttl(ttl_ms);
                let list = self.nodes[idx].list;
                self.detach(idx);
                self.attach_to_head(idx, list);
                (true, None)
            }
        } else {
            (false, None)
        }
    }

    fn delete(&mut self, hash: u64, key: &str) -> bool {
        if let Some(idx) = self.index.find(&self.nodes, hash, key) {
            self.remove_completely(idx, false);
            true
        } else {
            false
        }
    }

    fn clear(&mut self) {
        self.index.clear();
        self.nodes.clear();
        self.free_nodes.clear();
        self.window.reset();
        self.probation.reset();
        self.protected.reset();
        self.sweep_cursor = 0;
        self.hits = 0;
        self.misses = 0;
        self.evictions = 0;
        self.expirations = 0;
        self.sketch.table.fill(0);
        self.sketch.additions = 0;
        self.bytes_used = 0;
    }

    fn stats(&self) -> CacheStats {
        CacheStats {
            hits: self.hits,
            misses: self.misses,
            capacity: self.capacity,
            size: self.window.len + self.probation.len + self.protected.len,
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
        for head in [self.window.head, self.probation.head, self.protected.head] {
            let mut curr = head;
            while let Some(idx) = curr {
                let node = &self.nodes[idx];
                if !node.entry.is_expired() {
                    result.push(node.key.to_string());
                }
                curr = node.next;
            }
        }
        result
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use super::super::test_hash as h;

    fn set(cache: &mut TinyLfuCache, key: &str, value: u8, ttl: Option<u64>) {
        cache.set(h(key), key, Bytes::from(vec![value]), ttl);
    }

    fn get(cache: &mut TinyLfuCache, key: &str) -> Option<Vec<u8>> {
        cache.get(h(key), key).0.map(|b| b.to_vec())
    }

    #[test]
    fn test_tinylfu_basic() {
        let mut cache = TinyLfuCache::new(5);

        // Fill cache
        for i in 0..5 {
            set(&mut cache, &format!("key-{}", i), i, None);
        }
        assert_eq!(cache.stats().size, 5);

        // Increase frequency of key-0 and key-1
        for _ in 0..3 {
            let _ = get(&mut cache, "key-0");
            let _ = get(&mut cache, "key-1");
        }

        // Add a new key. TinyLFU will compare frequency of candidate vs probation victim.
        // It will evict a low-frequency key.
        set(&mut cache, "key-5", 5, None);

        // key-0 and key-1 must remain because of high frequency.
        assert!(get(&mut cache, "key-0").is_some());
        assert!(get(&mut cache, "key-1").is_some());
    }

    #[test]
    fn test_tinylfu_churn_keeps_accounting() {
        let mut cache = TinyLfuCache::new(50);
        for i in 0..2000u32 {
            let key = format!("k{}", i % 300);
            set(&mut cache, &key, (i % 251) as u8, if i % 3 == 0 { Some(1_000) } else { None });
            if i % 7 == 0 {
                cache.delete(h(&key), &key);
            }
        }
        let keys = cache.keys();
        assert_eq!(cache.stats().size, keys.len());
        assert!(keys.len() <= 50);
        assert_eq!(cache.bytes_used, keys.iter().map(|k| k.len() + 1).sum::<usize>());
        for k in &keys {
            assert!(get(&mut cache, k).is_some());
        }
    }

    #[test]
    fn test_tinylfu_purge_expired() {
        let mut cache = TinyLfuCache::new(100);
        for i in 0..20 {
            set(&mut cache, &format!("t{}", i), i, Some(5));
            set(&mut cache, &format!("p{}", i), i, None);
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
        let mut out = Vec::new();
        cache.purge_expired(1000, &mut out);
        assert_eq!(out.len(), 20);
        assert!(out.iter().all(|e| e.reason == "expired" && e.key.starts_with('t')));
        assert_eq!(cache.stats().size, 20);
    }
}
