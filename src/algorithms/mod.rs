use std::sync::LazyLock;
use std::time::Instant;
use bytes::Bytes;
use hashbrown::HashTable;

static EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);

/// Milliseconds since the first cache operation in this process, offset by 1 so that
/// `0` can be used as "never expires" in `CacheEntry::expires_at`.
#[inline]
pub fn now_ms() -> u64 {
    EPOCH.elapsed().as_millis() as u64 + 1
}

#[derive(Debug, Clone)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub capacity: usize,
    pub size: usize,
    pub bytes_used: usize,
    pub evictions: u64,
    pub expirations: u64,
}

#[derive(Debug, Clone)]
pub struct Eviction {
    pub key: String,
    pub value: Bytes,
    pub reason: &'static str, // "evicted", "expired"
}

impl Eviction {
    pub fn expired(key: String, value: Bytes) -> Self {
        Self { key, value, reason: "expired" }
    }

    pub fn evicted(key: String, value: Bytes) -> Self {
        Self { key, value, reason: "evicted" }
    }
}

/// A stored value. `Bytes` makes handing a value out of the shard lock a refcount bump
/// instead of a full copy of the payload.
#[derive(Debug, Clone, Default)]
pub struct CacheEntry {
    pub value: Bytes,
    /// Absolute deadline in `now_ms()` units, `0` = never expires.
    pub expires_at: u64,
    /// Original TTL, kept so sliding expiration can push the deadline forward on access.
    pub ttl_ms: u64,
}

impl CacheEntry {
    pub fn new(value: Bytes, ttl_ms: Option<u64>) -> Self {
        match ttl_ms {
            Some(ms) => Self { value, expires_at: now_ms().saturating_add(ms), ttl_ms: ms },
            None => Self { value, expires_at: 0, ttl_ms: 0 },
        }
    }

    /// Remaining TTL in milliseconds, or `None` if the entry never expires.
    pub fn remaining_ms(&self) -> Option<u64> {
        if self.expires_at == 0 {
            None
        } else {
            Some(self.expires_at.saturating_sub(now_ms()))
        }
    }

    #[inline]
    pub fn is_expired(&self) -> bool {
        self.expires_at != 0 && now_ms() > self.expires_at
    }

    #[inline]
    pub fn is_expired_at(&self, now: u64) -> bool {
        self.expires_at != 0 && now > self.expires_at
    }

    /// Sliding expiration: restart the TTL window from now.
    #[inline]
    pub fn slide(&mut self) {
        if self.expires_at != 0 {
            self.expires_at = now_ms().saturating_add(self.ttl_ms);
        }
    }

    pub fn with_ttl(&self, ttl_ms: Option<u64>) -> Self {
        Self::new(self.value.clone(), ttl_ms)
    }
}

/// Nodes stored in an algorithm's slab, addressable through a `KeyIndex`.
pub trait IndexedNode {
    fn key(&self) -> &str;
    fn hash(&self) -> u64;
}

/// Hash index from key to slab position. The key lives only once, inside the node;
/// the table stores just the slab index and reuses the hash computed by the shard router.
pub struct KeyIndex {
    table: HashTable<usize>,
}

impl KeyIndex {
    pub fn with_capacity(capacity: usize) -> Self {
        Self { table: HashTable::with_capacity(capacity) }
    }

    #[inline]
    pub fn find<N: IndexedNode>(&self, nodes: &[N], hash: u64, key: &str) -> Option<usize> {
        self.table
            .find(hash, |&idx| nodes[idx].hash() == hash && nodes[idx].key() == key)
            .copied()
    }

    #[inline]
    pub fn insert<N: IndexedNode>(&mut self, nodes: &[N], hash: u64, idx: usize) {
        self.table.insert_unique(hash, idx, |&i| nodes[i].hash());
    }

    /// Removes by slab position, so it still works after the node's key was moved out.
    #[inline]
    pub fn remove(&mut self, hash: u64, idx: usize) {
        if let Ok(entry) = self.table.find_entry(hash, |&i| i == idx) {
            entry.remove();
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.table.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.table.is_empty()
    }

    pub fn clear(&mut self) {
        self.table.clear();
    }
}

/// Every method receives the key together with its hash, computed once per operation
/// by `Cache` and shared by shard routing, the index and the TinyLFU sketch.
pub trait CacheImpl: Send + Sync {
    fn get(&mut self, hash: u64, key: &str) -> (Option<Bytes>, Option<Eviction>);
    fn peek(&mut self, hash: u64, key: &str) -> (Option<Bytes>, Option<Eviction>);
    fn has(&mut self, hash: u64, key: &str) -> (bool, Option<Eviction>);
    fn set(&mut self, hash: u64, key: &str, value: Bytes, ttl_ms: Option<u64>) -> (Option<Bytes>, Option<Vec<Eviction>>);
    fn touch(&mut self, hash: u64, key: &str, ttl_ms: Option<u64>) -> (bool, Option<Eviction>);
    fn delete(&mut self, hash: u64, key: &str) -> bool;
    fn clear(&mut self);
    fn stats(&self) -> CacheStats;
    fn keys(&self) -> Vec<String>;
    /// Remaining TTL (ms) of a stored entry. `None` if the key is absent or never expires.
    /// Read-only: does not touch recency/frequency metadata or hit/miss counters.
    fn ttl_remaining_ms(&self, hash: u64, key: &str) -> Option<u64>;
    /// Active expiration: inspects up to `max_scan` slab slots (resuming where the previous
    /// call stopped) and removes the expired entries found, appending them to `out`.
    fn purge_expired(&mut self, max_scan: usize, out: &mut Vec<Eviction>);
}

pub mod lru;
pub mod arc;
pub mod tinylfu;

#[cfg(test)]
pub(crate) fn test_hash(key: &str) -> u64 {
    use std::hash::BuildHasher;
    foldhash::fast::FixedState::with_seed(0).hash_one(key)
}

#[cfg(test)]
mod model_tests {
    use super::*;
    use rand::{rngs::StdRng, Rng, SeedableRng};
    use std::collections::{HashMap, VecDeque};

    fn bytes(v: u8) -> Bytes {
        Bytes::from(vec![v])
    }

    /// Invariants every policy must keep: index, lists, `size`, `bytes_used` and `keys()`
    /// agree, capacity is respected, and every resident key returns its last written value.
    fn check_invariants(cache: &mut dyn CacheImpl, last_written: &HashMap<String, u8>, capacity: usize) {
        let keys = cache.keys();
        let stats = cache.stats();
        assert_eq!(stats.size, keys.len());
        assert!(stats.size <= capacity, "size {} > capacity {}", stats.size, capacity);
        assert_eq!(stats.bytes_used, keys.iter().map(|k| k.len() + 1).sum::<usize>());
        for key in &keys {
            let value = cache.peek(test_hash(key), key).0.expect("listed key must be readable");
            assert_eq!(value.as_ref(), &[last_written[key]]);
        }
    }

    fn run_random_ops(cache: &mut dyn CacheImpl, capacity: usize, seed: u64) {
        let mut rng = StdRng::seed_from_u64(seed);
        let mut last_written: HashMap<String, u8> = HashMap::new();
        for step in 0..5_000 {
            let key = format!("key-{}", rng.gen_range(0..(capacity * 3)));
            let h = test_hash(&key);
            match rng.gen_range(0..10) {
                0..=3 => {
                    let v: u8 = rng.gen();
                    let ttl = if rng.gen_bool(0.2) { Some(3_600_000) } else { None };
                    cache.set(h, &key, bytes(v), ttl);
                    last_written.insert(key, v);
                }
                4..=7 => {
                    if let Some(v) = cache.get(h, &key).0 {
                        assert_eq!(v.as_ref(), &[last_written[&key]]);
                    }
                }
                8 => {
                    cache.delete(h, &key);
                }
                _ => {
                    cache.touch(h, &key, Some(3_600_000));
                }
            }
            if step % 97 == 0 {
                check_invariants(cache, &last_written, capacity);
            }
        }
        check_invariants(cache, &last_written, capacity);
    }

    #[test]
    fn random_ops_keep_invariants_for_every_policy() {
        for seed in 0..8 {
            for capacity in [1, 2, 7, 64] {
                run_random_ops(&mut lru::LruCache::new(capacity), capacity, seed);
                run_random_ops(&mut arc::ArcCache::new(capacity), capacity, seed);
                run_random_ops(&mut tinylfu::TinyLfuCache::new(capacity), capacity, seed);
            }
        }
    }

    #[test]
    fn lru_matches_reference_model() {
        let capacity = 16;
        let mut rng = StdRng::seed_from_u64(42);
        let mut cache = lru::LruCache::new(capacity);
        let mut model: VecDeque<(String, u8)> = VecDeque::new(); // front = most recent

        for _ in 0..20_000 {
            let key = format!("k{}", rng.gen_range(0..40));
            let h = test_hash(&key);
            let pos = model.iter().position(|(k, _)| *k == key);
            if rng.gen_bool(0.5) {
                let v: u8 = rng.gen();
                cache.set(h, &key, bytes(v), None);
                if let Some(p) = pos {
                    model.remove(p);
                } else if model.len() == capacity {
                    model.pop_back();
                }
                model.push_front((key, v));
            } else {
                let got = cache.get(h, &key).0.map(|b| b[0]);
                let expected = pos.map(|p| {
                    let entry = model.remove(p).unwrap();
                    let v = entry.1;
                    model.push_front(entry);
                    v
                });
                assert_eq!(got, expected);
            }
        }
    }
}
