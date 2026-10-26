use bytes::Bytes;
use super::{now_ms, CacheEntry, CacheImpl, CacheStats, Eviction, IndexedNode, KeyIndex};

#[derive(Copy, Clone, PartialEq, Debug)]
enum ArcList {
    T1,
    B1,
    T2,
    B2,
}

struct ArcNode {
    key: Box<str>,
    hash: u64,
    value: Option<CacheEntry>, // Only T1 and T2 store values. B1 and B2 are ghost entries.
    list: ArcList,
    prev: Option<usize>,
    next: Option<usize>,
    live: bool,
}

impl IndexedNode for ArcNode {
    #[inline]
    fn key(&self) -> &str {
        &self.key
    }
    #[inline]
    fn hash(&self) -> u64 {
        self.hash
    }
}

struct ArcListMetadata {
    head: Option<usize>,
    tail: Option<usize>,
    len: usize,
}

impl ArcListMetadata {
    fn new() -> Self {
        Self {
            head: None,
            tail: None,
            len: 0,
        }
    }
}

pub struct ArcCache {
    capacity: usize,
    max_bytes: Option<usize>,
    bytes_used: usize,
    sliding: bool,
    index: KeyIndex,
    nodes: Vec<ArcNode>,
    free_nodes: Vec<usize>,

    t1: ArcListMetadata,
    b1: ArcListMetadata,
    t2: ArcListMetadata,
    b2: ArcListMetadata,

    p: usize, // Target size for T1
    sweep_cursor: usize,
    hits: u64,
    misses: u64,
    evictions: u64,
    expirations: u64,
}

impl ArcCache {
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
            index: KeyIndex::with_capacity(capacity * 2),
            nodes: Vec::with_capacity(capacity * 2),
            free_nodes: Vec::new(),
            t1: ArcListMetadata::new(),
            b1: ArcListMetadata::new(),
            t2: ArcListMetadata::new(),
            b2: ArcListMetadata::new(),
            p: 0,
            sweep_cursor: 0,
            hits: 0,
            misses: 0,
            evictions: 0,
            expirations: 0,
        }
    }

    fn node_bytes(&self, idx: usize) -> usize {
        let node = &self.nodes[idx];
        if let Some(ref entry) = node.value {
            node.key.len() + entry.value.len()
        } else {
            0
        }
    }

    fn is_resident_expired(&self, idx: usize) -> bool {
        self.nodes[idx].value.as_ref().map_or(true, |e| e.is_expired())
    }

    fn detach(&mut self, idx: usize) {
        let prev = self.nodes[idx].prev;
        let next = self.nodes[idx].next;
        let list = self.nodes[idx].list;

        let meta = match list {
            ArcList::T1 => &mut self.t1,
            ArcList::B1 => &mut self.b1,
            ArcList::T2 => &mut self.t2,
            ArcList::B2 => &mut self.b2,
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

    fn attach_to_head(&mut self, idx: usize, list: ArcList) {
        self.nodes[idx].list = list;
        let meta = match list {
            ArcList::T1 => &mut self.t1,
            ArcList::B1 => &mut self.b1,
            ArcList::T2 => &mut self.t2,
            ArcList::B2 => &mut self.b2,
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

    /// Unlinks a node (resident or ghost) from its list and the index and frees its slot.
    fn remove_completely(&mut self, idx: usize) -> (String, Option<CacheEntry>) {
        let bytes = self.node_bytes(idx);
        self.bytes_used -= bytes;
        self.detach(idx);
        self.index.remove(self.nodes[idx].hash, idx);
        self.free_nodes.push(idx);
        let node = &mut self.nodes[idx];
        node.live = false;
        let key = std::mem::take(&mut node.key);
        let val = node.value.take();
        (key.into_string(), val)
    }

    fn expire_node(&mut self, idx: usize) -> Eviction {
        let (key, entry) = self.remove_completely(idx);
        self.expirations += 1;
        Eviction::expired(key, entry.map(|e| e.value).unwrap_or_default())
    }

    fn replace(&mut self, key_in_b2: bool) -> Option<Eviction> {
        let (tail, ghost_list) = if self.t1.len > 0 && (self.t1.len > self.p || (key_in_b2 && self.t1.len == self.p)) {
            (self.t1.tail, ArcList::B1)
        } else {
            (self.t2.tail, ArcList::B2)
        };
        let idx = tail?;
        let bytes = self.node_bytes(idx);
        self.bytes_used -= bytes;
        self.detach(idx);
        let entry = self.nodes[idx].value.take(); // Evict value from memory, keep the ghost key
        self.attach_to_head(idx, ghost_list);
        entry.map(|e| {
            self.evictions += 1;
            Eviction::evicted(self.nodes[idx].key.to_string(), e.value)
        })
    }
}

impl CacheImpl for ArcCache {
    fn get(&mut self, hash: u64, key: &str) -> (Option<Bytes>, Option<Eviction>) {
        if let Some(idx) = self.index.find(&self.nodes, hash, key) {
            match self.nodes[idx].list {
                ArcList::T1 | ArcList::T2 => {
                    if self.is_resident_expired(idx) {
                        self.misses += 1;
                        (None, Some(self.expire_node(idx)))
                    } else {
                        self.detach(idx);
                        self.attach_to_head(idx, ArcList::T2);
                        self.hits += 1;
                        if self.sliding {
                            if let Some(e) = self.nodes[idx].value.as_mut() {
                                e.slide();
                            }
                        }
                        (self.nodes[idx].value.as_ref().map(|e| e.value.clone()), None)
                    }
                }
                ArcList::B1 | ArcList::B2 => {
                    self.misses += 1;
                    (None, None)
                }
            }
        } else {
            self.misses += 1;
            (None, None)
        }
    }

    fn peek(&mut self, hash: u64, key: &str) -> (Option<Bytes>, Option<Eviction>) {
        if let Some(idx) = self.index.find(&self.nodes, hash, key) {
            match self.nodes[idx].list {
                ArcList::T1 | ArcList::T2 => {
                    if self.is_resident_expired(idx) {
                        self.misses += 1;
                        (None, Some(self.expire_node(idx)))
                    } else {
                        self.hits += 1;
                        (self.nodes[idx].value.as_ref().map(|e| e.value.clone()), None)
                    }
                }
                _ => {
                    self.misses += 1;
                    (None, None)
                }
            }
        } else {
            self.misses += 1;
            (None, None)
        }
    }

    fn has(&mut self, hash: u64, key: &str) -> (bool, Option<Eviction>) {
        if let Some(idx) = self.index.find(&self.nodes, hash, key) {
            match self.nodes[idx].list {
                ArcList::T1 | ArcList::T2 => {
                    if self.is_resident_expired(idx) {
                        (false, Some(self.expire_node(idx)))
                    } else {
                        (true, None)
                    }
                }
                _ => (false, None)
            }
        } else {
            (false, None)
        }
    }

    fn set(&mut self, hash: u64, key: &str, value: Bytes, ttl_ms: Option<u64>) -> (Option<Bytes>, Option<Vec<Eviction>>) {
        if self.capacity == 0 {
            return (None, None);
        }

        let new_entry = CacheEntry::new(value, ttl_ms);
        let new_bytes = key.len() + new_entry.value.len();
        let mut old_value = None;
        let mut evictions: Option<Vec<Eviction>> = None;
        let mut stored = false;

        if let Some(idx) = self.index.find(&self.nodes, hash, key) {
            match self.nodes[idx].list {
                ArcList::T1 | ArcList::T2 => {
                    if self.is_resident_expired(idx) {
                        let ev = self.expire_node(idx);
                        evictions.get_or_insert_with(Vec::new).push(ev);
                    } else {
                        let old_bytes = self.node_bytes(idx);
                        old_value = self.nodes[idx].value.replace(new_entry.clone()).map(|e| e.value);
                        self.bytes_used = self.bytes_used + new_bytes - old_bytes;
                        self.detach(idx);
                        self.attach_to_head(idx, ArcList::T2);
                        stored = true;
                    }
                }
                ArcList::B1 => {
                    let b1_len = if self.b1.len == 0 { 1 } else { self.b1.len };
                    let delta = std::cmp::max(1, self.b2.len / b1_len);
                    self.p = std::cmp::min(self.p + delta, self.capacity);

                    if let Some(ev) = self.replace(false) {
                        evictions.get_or_insert_with(Vec::new).push(ev);
                    }

                    self.detach(idx);
                    self.nodes[idx].value = Some(new_entry.clone());
                    self.bytes_used += new_bytes;
                    self.attach_to_head(idx, ArcList::T2);
                    stored = true;
                }
                ArcList::B2 => {
                    let b2_len = if self.b2.len == 0 { 1 } else { self.b2.len };
                    let delta = std::cmp::max(1, self.b1.len / b2_len);
                    self.p = self.p.saturating_sub(delta);

                    if let Some(ev) = self.replace(true) {
                        evictions.get_or_insert_with(Vec::new).push(ev);
                    }

                    self.detach(idx);
                    self.nodes[idx].value = Some(new_entry.clone());
                    self.bytes_used += new_bytes;
                    self.attach_to_head(idx, ArcList::T2);
                    stored = true;
                }
            }
        }

        if !stored {
            let l1_len = self.t1.len + self.b1.len;
            let l2_len = self.t2.len + self.b2.len;

            if l1_len == self.capacity {
                if self.t1.len < self.capacity {
                    if let Some(b1_tail) = self.b1.tail {
                        self.remove_completely(b1_tail);
                    }
                    if let Some(ev) = self.replace(false) {
                        evictions.get_or_insert_with(Vec::new).push(ev);
                    }
                } else if let Some(t1_tail) = self.t1.tail {
                    let (ev_key, entry) = self.remove_completely(t1_tail);
                    if let Some(e) = entry {
                        self.evictions += 1;
                        evictions.get_or_insert_with(Vec::new).push(Eviction::evicted(ev_key, e.value));
                    }
                }
            } else if l1_len < self.capacity && l1_len + l2_len >= self.capacity {
                if l1_len + l2_len == 2 * self.capacity {
                    if let Some(b2_tail) = self.b2.tail {
                        self.remove_completely(b2_tail);
                    }
                }
                if let Some(ev) = self.replace(false) {
                    evictions.get_or_insert_with(Vec::new).push(ev);
                }
            }

            let node = ArcNode {
                key: key.into(),
                hash,
                value: Some(new_entry),
                list: ArcList::T1,
                prev: None,
                next: None,
                live: true,
            };
            let idx = if let Some(free_idx) = self.free_nodes.pop() {
                self.nodes[free_idx] = node;
                free_idx
            } else {
                self.nodes.push(node);
                self.nodes.len() - 1
            };

            self.bytes_used += new_bytes;
            self.attach_to_head(idx, ArcList::T1);
            self.index.insert(&self.nodes, hash, idx);
        }

        if let Some(max) = self.max_bytes {
            while self.bytes_used > max && (self.t1.len > 0 || self.t2.len > 0) {
                if let Some(ev) = self.replace(false) {
                    evictions.get_or_insert_with(Vec::new).push(ev);
                }
            }
        }

        (old_value, evictions)
    }

    fn touch(&mut self, hash: u64, key: &str, ttl_ms: Option<u64>) -> (bool, Option<Eviction>) {
        if let Some(idx) = self.index.find(&self.nodes, hash, key) {
            match self.nodes[idx].list {
                ArcList::T1 | ArcList::T2 => {
                    if self.is_resident_expired(idx) {
                        (false, Some(self.expire_node(idx)))
                    } else {
                        let refreshed = self.nodes[idx].value.as_ref().map(|e| e.with_ttl(ttl_ms));
                        self.nodes[idx].value = refreshed;
                        self.detach(idx);
                        self.attach_to_head(idx, ArcList::T2);
                        (true, None)
                    }
                }
                _ => (false, None)
            }
        } else {
            (false, None)
        }
    }

    fn delete(&mut self, hash: u64, key: &str) -> bool {
        if let Some(idx) = self.index.find(&self.nodes, hash, key) {
            self.remove_completely(idx);
            true
        } else {
            false
        }
    }

    fn clear(&mut self) {
        self.index.clear();
        self.nodes.clear();
        self.free_nodes.clear();
        self.t1 = ArcListMetadata::new();
        self.b1 = ArcListMetadata::new();
        self.t2 = ArcListMetadata::new();
        self.b2 = ArcListMetadata::new();
        self.p = 0;
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
            size: self.t1.len + self.t2.len,
            bytes_used: self.bytes_used,
            evictions: self.evictions,
            expirations: self.expirations,
        }
    }

    fn ttl_remaining_ms(&self, hash: u64, key: &str) -> Option<u64> {
        let idx = self.index.find(&self.nodes, hash, key)?;
        match self.nodes[idx].list {
            ArcList::T1 | ArcList::T2 => self.nodes[idx].value.as_ref().and_then(|e| e.remaining_ms()),
            _ => None,
        }
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
            let expired = node.live && node.value.as_ref().map_or(false, |e| e.is_expired_at(now));
            if expired {
                let ev = self.expire_node(idx);
                out.push(ev);
            }
        }
    }

    fn keys(&self) -> Vec<String> {
        let mut result = Vec::new();
        for head in [self.t1.head, self.t2.head] {
            let mut curr = head;
            while let Some(idx) = curr {
                let node = &self.nodes[idx];
                let expired = node.value.as_ref().map_or(true, |e| e.is_expired());
                if !expired {
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

    fn set(cache: &mut ArcCache, key: &str, value: u8, ttl: Option<u64>) {
        cache.set(h(key), key, Bytes::from(vec![value]), ttl);
    }

    fn get(cache: &mut ArcCache, key: &str) -> Option<Vec<u8>> {
        cache.get(h(key), key).0.map(|b| b.to_vec())
    }

    #[test]
    fn test_arc_basic() {
        let mut cache = ArcCache::new(3);
        set(&mut cache, "a", 1, None);
        set(&mut cache, "b", 2, None);
        set(&mut cache, "c", 3, None);

        // Hits to move to T2
        assert_eq!(get(&mut cache, "a"), Some(vec![1]));
        assert_eq!(get(&mut cache, "b"), Some(vec![2]));

        // Set new key, should evict c (which was in T1 and never hit)
        set(&mut cache, "d", 4, None);
        assert_eq!(get(&mut cache, "c"), None);
        assert_eq!(get(&mut cache, "a"), Some(vec![1]));
        assert_eq!(get(&mut cache, "b"), Some(vec![2]));
        assert_eq!(get(&mut cache, "d"), Some(vec![4]));
    }

    #[test]
    fn test_arc_ghost_hit_readmits() {
        let mut cache = ArcCache::new(2);
        set(&mut cache, "a", 1, None);
        get(&mut cache, "a"); // "a" -> T2
        set(&mut cache, "b", 2, None);
        set(&mut cache, "c", 3, None); // "b" becomes a ghost in B1
        assert_eq!(get(&mut cache, "b"), None);
        set(&mut cache, "b", 9, None); // ghost hit -> back into T2
        assert_eq!(get(&mut cache, "b"), Some(vec![9]));
        assert_eq!(cache.stats().size, cache.keys().len());
    }

    #[test]
    fn test_arc_purge_skips_ghosts() {
        let mut cache = ArcCache::new(2);
        set(&mut cache, "a", 1, None);
        get(&mut cache, "a"); // "a" -> T2
        set(&mut cache, "b", 2, Some(5));
        set(&mut cache, "c", 3, None); // "b" demoted to ghost (no value)
        std::thread::sleep(std::time::Duration::from_millis(15));
        let mut out = Vec::new();
        cache.purge_expired(100, &mut out);
        assert!(out.is_empty());
        assert_eq!(cache.stats().size, 2);
    }
}
