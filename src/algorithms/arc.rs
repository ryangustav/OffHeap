use std::collections::HashMap;
use super::{CacheImpl, CacheEntry, CacheStats, Eviction};

#[derive(Copy, Clone, PartialEq, Debug)]
enum ArcList {
    T1,
    B1,
    T2,
    B2,
}

struct ArcNode {
    key: String,
    value: Option<CacheEntry>, // Only T1 and T2 store values. B1 and B2 are ghost entries.
    list: ArcList,
    prev: Option<usize>,
    next: Option<usize>,
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
    map: HashMap<String, usize>,
    nodes: Vec<ArcNode>,
    free_nodes: Vec<usize>,
    
    t1: ArcListMetadata,
    b1: ArcListMetadata,
    t2: ArcListMetadata,
    b2: ArcListMetadata,
    
    p: usize, // Target size for T1
    hits: u64,
    misses: u64,
}

impl ArcCache {
    pub fn new(capacity: usize) -> Self {
        Self::new_with_max_bytes(capacity, None)
    }

    pub fn new_with_max_bytes(capacity: usize, max_bytes: Option<usize>) -> Self {
        Self {
            capacity,
            max_bytes,
            bytes_used: 0,
            map: HashMap::with_capacity(capacity * 2),
            nodes: Vec::with_capacity(capacity * 2),
            free_nodes: Vec::new(),
            t1: ArcListMetadata::new(),
            b1: ArcListMetadata::new(),
            t2: ArcListMetadata::new(),
            b2: ArcListMetadata::new(),
            p: 0,
            hits: 0,
            misses: 0,
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

    fn remove_completely(&mut self, idx: usize) -> (String, Option<CacheEntry>) {
        let bytes = self.node_bytes(idx);
        self.bytes_used -= bytes;
        self.detach(idx);
        self.free_nodes.push(idx);
        let key = std::mem::take(&mut self.nodes[idx].key);
        let val = self.nodes[idx].value.take();
        (key, val)
    }

    fn replace(&mut self, key_in_b2: bool) -> Option<Eviction> {
        if self.t1.len > 0 && (self.t1.len > self.p || (key_in_b2 && self.t1.len == self.p)) {
            if let Some(t1_tail) = self.t1.tail {
                let bytes = self.node_bytes(t1_tail);
                self.bytes_used -= bytes;
                self.detach(t1_tail);
                let entry = self.nodes[t1_tail].value.take(); // Evict value from memory
                self.attach_to_head(t1_tail, ArcList::B1);
                if let Some(e) = entry {
                    return Some(Eviction {
                        key: self.nodes[t1_tail].key.clone(),
                        value: e.value,
                        reason: "evicted".to_string(),
                    });
                }
            }
        } else {
            if let Some(t2_tail) = self.t2.tail {
                let bytes = self.node_bytes(t2_tail);
                self.bytes_used -= bytes;
                self.detach(t2_tail);
                let entry = self.nodes[t2_tail].value.take(); // Evict value from memory
                self.attach_to_head(t2_tail, ArcList::B2);
                if let Some(e) = entry {
                    return Some(Eviction {
                        key: self.nodes[t2_tail].key.clone(),
                        value: e.value,
                        reason: "evicted".to_string(),
                    });
                }
            }
        }
        None
    }
}

impl CacheImpl for ArcCache {
    fn get(&mut self, key: &str) -> (Option<Vec<u8>>, Option<Eviction>) {
        if let Some(&idx) = self.map.get(key) {
            let list = self.nodes[idx].list;
            match list {
                ArcList::T1 | ArcList::T2 => {
                    let expired = self.nodes[idx].value.as_ref().map_or(true, |e| e.is_expired());
                    if expired {
                        let (ev_key, entry) = self.remove_completely(idx);
                        self.map.remove(&ev_key);
                        self.misses += 1;
                        let value = entry.map(|e| e.value).unwrap_or_default();
                        (None, Some(Eviction {
                            key: ev_key,
                            value,
                            reason: "expired".to_string(),
                        }))
                    } else {
                        self.detach(idx);
                        self.attach_to_head(idx, ArcList::T2);
                        self.hits += 1;
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

    fn peek(&mut self, key: &str) -> (Option<Vec<u8>>, Option<Eviction>) {
        if let Some(&idx) = self.map.get(key) {
            let list = self.nodes[idx].list;
            match list {
                ArcList::T1 | ArcList::T2 => {
                    let expired = self.nodes[idx].value.as_ref().map_or(true, |e| e.is_expired());
                    if expired {
                        let (ev_key, entry) = self.remove_completely(idx);
                        self.map.remove(&ev_key);
                        self.misses += 1;
                        let value = entry.map(|e| e.value).unwrap_or_default();
                        (None, Some(Eviction {
                            key: ev_key,
                            value,
                            reason: "expired".to_string(),
                        }))
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

    fn has(&mut self, key: &str) -> (bool, Option<Eviction>) {
        if let Some(&idx) = self.map.get(key) {
            let list = self.nodes[idx].list;
            match list {
                ArcList::T1 | ArcList::T2 => {
                    let expired = self.nodes[idx].value.as_ref().map_or(true, |e| e.is_expired());
                    if expired {
                        let (ev_key, entry) = self.remove_completely(idx);
                        self.map.remove(&ev_key);
                        let value = entry.map(|e| e.value).unwrap_or_default();
                        (false, Some(Eviction {
                            key: ev_key,
                            value,
                            reason: "expired".to_string(),
                        }))
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

    fn set(&mut self, key: &str, value: Vec<u8>, ttl_ms: Option<u64>) -> (Option<Vec<u8>>, Option<Vec<Eviction>>) {
        if self.capacity == 0 {
            return (None, None);
        }

        let new_entry = CacheEntry::new(value, ttl_ms);
        let new_bytes = key.len() + new_entry.value.len();
        let mut old_value = None;
        let mut evictions: Option<Vec<Eviction>> = None;

        if let Some(&idx) = self.map.get(key) {
            let list = self.nodes[idx].list;
            match list {
                ArcList::T1 | ArcList::T2 => {
                    let expired = self.nodes[idx].value.as_ref().map_or(true, |e| e.is_expired());
                    if expired {
                        let (ev_key, entry) = self.remove_completely(idx);
                        self.map.remove(&ev_key);
                        let value = entry.map(|e| e.value).unwrap_or_default();
                        evictions.get_or_insert_with(Vec::new).push(Eviction {
                            key: ev_key,
                            value,
                            reason: "expired".to_string(),
                        });
                    } else {
                        let old_bytes = self.node_bytes(idx);
                        old_value = self.nodes[idx].value.replace(new_entry.clone()).map(|e| e.value);
                        self.bytes_used = self.bytes_used + new_bytes - old_bytes;
                        self.detach(idx);
                        self.attach_to_head(idx, ArcList::T2);
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
                }
            }
        }

        if !self.map.contains_key(key) {
            let l1_len = self.t1.len + self.b1.len;
            let l2_len = self.t2.len + self.b2.len;

            if l1_len == self.capacity {
                if self.t1.len < self.capacity {
                    if let Some(b1_tail) = self.b1.tail {
                        let ev_key = self.remove_completely(b1_tail).0;
                        self.map.remove(&ev_key);
                    }
                    if let Some(ev) = self.replace(false) {
                        evictions.get_or_insert_with(Vec::new).push(ev);
                    }
                } else {
                    if let Some(t1_tail) = self.t1.tail {
                        let (ev_key, entry) = self.remove_completely(t1_tail);
                        self.map.remove(&ev_key);
                        if let Some(e) = entry {
                            evictions.get_or_insert_with(Vec::new).push(Eviction {
                                key: ev_key,
                                value: e.value,
                                reason: "evicted".to_string(),
                            });
                        }
                    }
                }
            } else if l1_len < self.capacity && l1_len + l2_len >= self.capacity {
                if l1_len + l2_len == 2 * self.capacity {
                    if let Some(b2_tail) = self.b2.tail {
                        let ev_key = self.remove_completely(b2_tail).0;
                        self.map.remove(&ev_key);
                    }
                }
                if let Some(ev) = self.replace(false) {
                    evictions.get_or_insert_with(Vec::new).push(ev);
                }
            }

            let idx = if let Some(free_idx) = self.free_nodes.pop() {
                self.nodes[free_idx].key = key.to_string();
                self.nodes[free_idx].value = Some(new_entry);
                free_idx
            } else {
                let free_idx = self.nodes.len();
                self.nodes.push(ArcNode {
                    key: key.to_string(),
                    value: Some(new_entry),
                    list: ArcList::T1,
                    prev: None,
                    next: None,
                });
                free_idx
            };

            self.bytes_used += new_bytes;
            self.attach_to_head(idx, ArcList::T1);
            self.map.insert(key.to_string(), idx);
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

    fn touch(&mut self, key: &str, ttl_ms: Option<u64>) -> (bool, Option<Eviction>) {
        if let Some(&idx) = self.map.get(key) {
            let list = self.nodes[idx].list;
            match list {
                ArcList::T1 | ArcList::T2 => {
                    let expired = self.nodes[idx].value.as_ref().map_or(true, |e| e.is_expired());
                    if expired {
                        let (ev_key, entry) = self.remove_completely(idx);
                        self.map.remove(key);
                        let value = entry.map(|e| e.value).unwrap_or_default();
                        (false, Some(Eviction {
                            key: ev_key,
                            value,
                            reason: "expired".to_string(),
                        }))
                    } else {
                        let val = self.nodes[idx].value.as_ref().unwrap().value.clone();
                        self.nodes[idx].value = Some(CacheEntry::new(val, ttl_ms));
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
        self.t1 = ArcListMetadata::new();
        self.b1 = ArcListMetadata::new();
        self.t2 = ArcListMetadata::new();
        self.b2 = ArcListMetadata::new();
        self.p = 0;
        self.hits = 0;
        self.misses = 0;
        self.bytes_used = 0;
    }

    fn stats(&self) -> CacheStats {
        CacheStats {
            hits: self.hits,
            misses: self.misses,
            capacity: self.capacity,
            size: self.t1.len + self.t2.len,
            bytes_used: self.bytes_used,
        }
    }

    fn keys(&self) -> Vec<String> {
        let mut result = Vec::new();
        let mut curr = self.t1.head;
        while let Some(idx) = curr {
            let node = &self.nodes[idx];
            let expired = node.value.as_ref().map_or(true, |e| e.is_expired());
            if !expired {
                result.push(node.key.clone());
            }
            curr = node.next;
        }
        let mut curr = self.t2.head;
        while let Some(idx) = curr {
            let node = &self.nodes[idx];
            let expired = node.value.as_ref().map_or(true, |e| e.is_expired());
            if !expired {
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
    fn test_arc_basic() {
        let mut cache = ArcCache::new(3);
        cache.set("a", vec![1], None);
        cache.set("b", vec![2], None);
        cache.set("c", vec![3], None);
        
        // Hits to move to T2
        assert_eq!(cache.get("a").0, Some(vec![1]));
        assert_eq!(cache.get("b").0, Some(vec![2]));
        
        // Set new key, should evict c (which was in T1 and never hit)
        cache.set("d", vec![4], None);
        assert_eq!(cache.get("c").0, None);
        assert_eq!(cache.get("a").0, Some(vec![1]));
        assert_eq!(cache.get("b").0, Some(vec![2]));
        assert_eq!(cache.get("d").0, Some(vec![4]));
    }
}
