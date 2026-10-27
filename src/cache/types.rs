use crate::algorithms::CacheImpl;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Number of generation counters exposed to each JS wrapper for L1 invalidation.
/// Must match `L1_GEN_SLOTS` in index.js.
pub const L1_GEN_SLOTS: usize = 4096;

/// Slot of a key in the L1 generation tables. FNV-1a over UTF-16 code units, so JS can
/// compute the same slot from a string with `charCodeAt` (see `l1Slot` in index.js).
pub fn l1_slot(key: &str) -> usize {
    let mut h: u32 = 0x811c_9dc5;
    for unit in key.encode_utf16() {
        h ^= unit as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    (h as usize) & (L1_GEN_SLOTS - 1)
}

#[napi(object)]
pub struct CacheConfig {
    pub policy: String,
    pub capacity: u32,
    /// Omitted: chosen from the CPU count, capped so each shard keeps a useful capacity.
    pub shards: Option<u32>,
    pub max_bytes: Option<f64>,
    pub compression: Option<bool>,
    /// Sliding expiration: every `get` hit restarts the entry's TTL window.
    pub sliding_ttl: Option<bool>,
}

#[napi(object)]
pub struct ShardStatsJs {
    pub size: f64,
    pub bytes_used: f64,
}

#[napi(object)]
pub struct CacheStatsJs {
    pub hits: f64,
    pub misses: f64,
    pub capacity: f64,
    pub size: f64,
    pub bytes_used: f64,
    pub sets: f64,
    pub deletes: f64,
    pub evictions: f64,
    pub expirations: f64,
    pub hit_rate: f64,
    pub uptime_ms: f64,
    pub shards: Vec<ShardStatsJs>,
}

#[napi(object)]
pub struct CacheCountersJs {
    pub hits: f64,
    pub misses: f64,
    pub sets: f64,
    pub deletes: f64,
    pub evictions: f64,
    pub expirations: f64,
}

/// One table of L1 generation counters, owned by a single JS `Cache` wrapper (one per
/// thread). V8 refuses two ArrayBuffers over the same memory, so every wrapper gets its
/// own table and writers bump the slot in all registered tables.
pub(super) struct GenTable {
    pub(super) slots: Box<[AtomicU32]>,
}

#[derive(Default)]
pub(super) struct L1Generations {
    pub(super) tables: parking_lot::RwLock<Vec<Arc<GenTable>>>,
    pub(super) count: AtomicUsize,
}

impl L1Generations {
    #[inline]
    pub(super) fn bump(&self, key: &str) {
        if self.count.load(Ordering::Acquire) == 0 {
            return;
        }
        let slot = l1_slot(key);
        for table in self.tables.read().iter() {
            table.slots[slot].fetch_add(1, Ordering::Release);
        }
    }

    pub(super) fn bump_all(&self) {
        if self.count.load(Ordering::Acquire) == 0 {
            return;
        }
        for table in self.tables.read().iter() {
            for slot in table.slots.iter() {
                slot.fetch_add(1, Ordering::Release);
            }
        }
    }
}

pub(super) type Shard = parking_lot::Mutex<Option<Box<dyn CacheImpl>>>;

#[napi]
pub struct Cache {
    pub(super) shards: Arc<[Shard]>,
    pub(super) shard_mask: Option<usize>,
    pub(super) compression: bool,
    pub(super) hasher: foldhash::fast::RandomState,
    pub(super) max_bytes: Option<usize>,
    pub(super) has_ttl: Arc<AtomicBool>,
    pub(super) l1_gens: Arc<L1Generations>,
    pub(super) atomic_hits: Arc<AtomicU64>,
    pub(super) atomic_misses: Arc<AtomicU64>,
    pub(super) atomic_sets: Arc<AtomicU64>,
    pub(super) atomic_deletes: Arc<AtomicU64>,
    pub(super) atomic_evictions: Arc<AtomicU64>,
    pub(super) atomic_expirations: Arc<AtomicU64>,
    pub(super) created_at: Arc<Instant>,
}

impl Clone for Cache {
    fn clone(&self) -> Self {
        Self {
            shards: Arc::clone(&self.shards),
            shard_mask: self.shard_mask,
            compression: self.compression,
            hasher: self.hasher,
            max_bytes: self.max_bytes,
            has_ttl: Arc::clone(&self.has_ttl),
            l1_gens: Arc::clone(&self.l1_gens),
            atomic_hits: Arc::clone(&self.atomic_hits),
            atomic_misses: Arc::clone(&self.atomic_misses),
            atomic_sets: Arc::clone(&self.atomic_sets),
            atomic_deletes: Arc::clone(&self.atomic_deletes),
            atomic_evictions: Arc::clone(&self.atomic_evictions),
            atomic_expirations: Arc::clone(&self.atomic_expirations),
            created_at: Arc::clone(&self.created_at),
        }
    }
}
