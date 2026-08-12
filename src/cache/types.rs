use std::sync::Arc;
use std::collections::hash_map::RandomState;
use std::sync::atomic::AtomicU64;
use std::time::Instant;
use crate::algorithms::CacheImpl;

#[napi(object)]
pub struct CacheConfig {
    pub policy: String,
    pub capacity: u32,
    pub shards: Option<u32>,
    pub max_bytes: Option<f64>,
    pub compression: Option<bool>,
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

#[napi]
pub struct Cache {
    pub(super) shards: Vec<Arc<parking_lot::Mutex<Option<Box<dyn CacheImpl>>>>>,
    pub(super) compression: bool,
    pub(super) hasher: Arc<RandomState>,
    pub(super) max_bytes: Option<usize>,
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
            shards: self.shards.clone(),
            compression: self.compression,
            hasher: Arc::clone(&self.hasher),
            max_bytes: self.max_bytes,
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
