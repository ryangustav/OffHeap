use std::collections::HashMap;
use super::{CacheImpl, CacheEntry, CacheStats};

// Count-Min Sketch with 4-bit counters and aging.
// Sized dynamically relative to cache capacity.
struct FrequencySketch {
    table: Vec<u8>,
    mask: usize,
    sample_size: usize,
    additions: usize,
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

    fn increment(&mut self, key: &str) {
        let hash = seahash::hash(key.as_bytes());
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

    fn estimate(&self, key: &str) -> u8 {
        let hash = seahash::hash(key.as_bytes());
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
    key: String,
    entry: CacheEntry,
    list: TinyLfuList,
    prev: Option<usize>,
    next: Option<usize>,
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
}

pub struct TinyLfuCache {
    capacity: usize,
    max_bytes: Option<usize>,
    bytes_used: usize,
    map: HashMap<String, usize>,
    nodes: Vec<TinyLfuNode>,
    free_nodes: Vec<usize>,

    window: TinyLfuListMetadata,
    probation: TinyLfuListMetadata,
    protected: TinyLfuListMetadata,

    sketch: FrequencySketch,
    hits: u64,
    misses: u64,
}

impl TinyLfuCache {
    pub fn new(capacity: usize) -> Self {
        Self::new_with_max_bytes(capacity, None)
    }

    pub fn new_with_max_bytes(capacity: usize, max_bytes: Option<usize>) -> Self {
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
            map: HashMap::with_capacity(capacity),
            nodes: Vec::with_capacity(capacity),
            free_nodes: Vec::new(),
            window: TinyLfuListMetadata::new(window_cap),
            probation: TinyLfuListMetadata::new(probation_cap),
            protected: TinyLfuListMetadata::new(protected_cap),
            sketch: FrequencySketch::new(capacity),
            hits: 0,
            misses: 0,
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

    fn remove_completely(&mut self, idx: usize) -> String {
        let bytes = self.node_bytes(idx);
        self.bytes_used -= bytes;
        self.detach(idx);
        self.free_nodes.push(idx);
        let key = std::mem::take(&mut self.nodes[idx].key);
        self.nodes[idx].entry = CacheEntry::new(Vec::new(), None);
        key
    }
}

impl CacheImpl for TinyLfuCache {
    fn get(&mut self, key: &str) -> Option<Vec<u8>> {
        self.sketch.increment(key);

        if let Some(&idx) = self.map.get(key) {
            if self.nodes[idx].entry.is_expired() {
                let evicted_key = self.remove_completely(idx);
                self.map.remove(&evicted_key);
                self.misses += 1;
                None
            } else {
                let list = self.nodes[idx].list;
                match list {
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
                self.hits += 1;
                Some(self.nodes[idx].entry.value.clone())
            }
        } else {
            self.misses += 1;
            None
        }
    }

    fn peek(&mut self, key: &str) -> Option<Vec<u8>> {
        if let Some(&idx) = self.map.get(key) {
            if self.nodes[idx].entry.is_expired() {
                let evicted_key = self.remove_completely(idx);
                self.map.remove(&evicted_key);
                self.misses += 1;
                None
            } else {
                self.hits += 1;
                Some(self.nodes[idx].entry.value.clone())
            }
        } else {
            self.misses += 1;
            None
        }
    }

    fn has(&mut self, key: &str) -> bool {
        if let Some(&idx) = self.map.get(key) {
            if self.nodes[idx].entry.is_expired() {
                let evicted_key = self.remove_completely(idx);
                self.map.remove(&evicted_key);
                false
            } else {
                true
            }
        } else {
            false
        }
    }

    fn set(&mut self, key: &str, value: Vec<u8>, ttl_ms: Option<u64>) -> Option<Vec<u8>> {
        self.sketch.increment(key);

        let new_entry = CacheEntry::new(value, ttl_ms);
        let new_bytes = key.len() + new_entry.value.len();
        let mut old_value = None;

        if let Some(&idx) = self.map.get(key) {
            let old_bytes = self.node_bytes(idx);
            old_value = Some(std::mem::replace(&mut self.nodes[idx].entry, new_entry).value);
            self.bytes_used = self.bytes_used + new_bytes - old_bytes;

            let list = self.nodes[idx].list;
            match list {
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
        } else {
            if self.capacity == 0 {
                return None;
            }

            let new_idx = if let Some(free_idx) = self.free_nodes.pop() {
                self.nodes[free_idx].key = key.to_string();
                self.nodes[free_idx].entry = new_entry;
                free_idx
            } else {
                let free_idx = self.nodes.len();
                self.nodes.push(TinyLfuNode {
                    key: key.to_string(),
                    entry: new_entry,
                    list: TinyLfuList::Window,
                    prev: None,
                    next: None,
                });
                free_idx
            };

            self.bytes_used += new_bytes;
            self.attach_to_head(new_idx, TinyLfuList::Window);
            self.map.insert(key.to_string(), new_idx);

            if self.window.len > self.window.capacity {
                if let Some(win_tail) = self.window.tail {
                    let candidate_idx = win_tail;
                    self.detach(candidate_idx);

                    let main_has_capacity = self.probation.capacity > 0;
                    if main_has_capacity {
                        let total_size = self.window.len + self.probation.len + self.protected.len;
                        if total_size + 1 <= self.capacity {
                            self.attach_to_head(candidate_idx, TinyLfuList::Probation);
                        } else {
                            if let Some(prob_tail) = self.probation.tail {
                                let victim_idx = prob_tail;

                                let candidate_key = &self.nodes[candidate_idx].key;
                                let victim_key = &self.nodes[victim_idx].key;

                                let candidate_freq = self.sketch.estimate(candidate_key);
                                let victim_freq = self.sketch.estimate(victim_key);

                                if candidate_freq > victim_freq {
                                    let evicted_key = self.remove_completely(victim_idx);
                                    self.map.remove(&evicted_key);
                                    self.attach_to_head(candidate_idx, TinyLfuList::Probation);
                                } else {
                                    let evicted_key = self.remove_completely(candidate_idx);
                                    self.map.remove(&evicted_key);
                                }
                            } else {
                                self.attach_to_head(candidate_idx, TinyLfuList::Probation);
                            }
                        }
                    } else {
                        let evicted_key = self.remove_completely(candidate_idx);
                        self.map.remove(&evicted_key);
                    }
                }
            }
        }

        if let Some(max) = self.max_bytes {
            while self.bytes_used > max && (self.probation.len > 0 || self.window.len > 0) {
                if let Some(prob_tail) = self.probation.tail {
                    let evicted_key = self.remove_completely(prob_tail);
                    self.map.remove(&evicted_key);
                } else if let Some(win_tail) = self.window.tail {
                    let evicted_key = self.remove_completely(win_tail);
                    self.map.remove(&evicted_key);
                } else {
                    break;
                }
            }
        }

        old_value
    }

    fn touch(&mut self, key: &str, ttl_ms: Option<u64>) -> bool {
        if let Some(&idx) = self.map.get(key) {
            if self.nodes[idx].entry.is_expired() {
                let evicted_key = self.remove_completely(idx);
                self.map.remove(&evicted_key);
                false
            } else {
                self.nodes[idx].entry = CacheEntry::new(
                    self.nodes[idx].entry.value.clone(),
                    ttl_ms,
                );
                let list = self.nodes[idx].list;
                self.detach(idx);
                self.attach_to_head(idx, list);
                true
            }
        } else {
            false
        }
    }

    fn delete(&mut self, key: &str) -> bool {
        if let Some(&idx) = self.map.get(key) {
            self.remove_completely(idx);
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
        self.window.head = None;
        self.window.tail = None;
        self.window.len = 0;
        self.probation.head = None;
        self.probation.tail = None;
        self.probation.len = 0;
        self.protected.head = None;
        self.protected.tail = None;
        self.protected.len = 0;
        self.hits = 0;
        self.misses = 0;
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
        }
    }

    fn keys(&self) -> Vec<String> {
        let mut result = Vec::new();
        let mut curr = self.window.head;
        while let Some(idx) = curr {
            let node = &self.nodes[idx];
            if !node.entry.is_expired() {
                result.push(node.key.clone());
            }
            curr = node.next;
        }
        let mut curr = self.probation.head;
        while let Some(idx) = curr {
            let node = &self.nodes[idx];
            if !node.entry.is_expired() {
                result.push(node.key.clone());
            }
            curr = node.next;
        }
        let mut curr = self.protected.head;
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
    fn test_tinylfu_basic() {
        let mut cache = TinyLfuCache::new(5);
        
        // Fill cache
        for i in 0..5 {
            cache.set(&format!("key-{}", i), vec![i], None);
        }
        assert_eq!(cache.stats().size, 5);

        // Increase frequency of key-0 and key-1
        for _ in 0..3 {
            cache.get("key-0");
            cache.get("key-1");
        }

        // Add a new key. TinyLFU will compare frequency of candidate vs probation victim.
        // It will evict a low-frequency key.
        cache.set("key-5", vec![5], None);
        
        // key-0 and key-1 must remain because of high frequency.
        assert!(cache.get("key-0").is_some());
        assert!(cache.get("key-1").is_some());
    }
}
