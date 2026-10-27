use super::algorithms::{arc::ArcCache, lru::LruCache, tinylfu::TinyLfuCache, CacheImpl, Eviction};
use bytes::Bytes;
use napi::{Env, JsArrayBuffer, JsObject, JsUnknown, Result};
use std::hash::BuildHasher;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

pub mod serde;
pub mod types;

use serde::{TAG_I64, TAG_JSON};
pub use types::*;

/// Expired-entry slots inspected (per shard) after every write while TTLs are in use.
/// Keeps memory held by never-read expired entries bounded without a background thread.
const WRITE_SWEEP_SLOTS: usize = 4;

/// Default shard count: 4 per CPU (power of two, 8..=256), halved until every shard
/// holds at least 64 entries so small caches are not split into near-empty shards.
fn auto_shards(capacity: usize) -> usize {
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let mut shards = (cpus * 4).next_power_of_two().clamp(8, 256);
    while shards > 1 && capacity / shards < 64 {
        shards /= 2;
    }
    shards
}

fn disposed_error() -> napi::Error {
    napi::Error::new(
        napi::Status::GenericFailure,
        "Cache has been disposed or poisoned",
    )
}

fn ttl_from_js(ttl_ms: Option<f64>) -> Option<u64> {
    ttl_ms.map(|ms| ms as u64)
}

impl Cache {
    pub fn from_config(config: CacheConfig) -> Self {
        let capacity = config.capacity as usize;
        let shards_count = match config.shards {
            Some(n) => (n as usize).max(1),
            None => auto_shards(capacity),
        };
        let policy_lower = config.policy.to_lowercase();
        let compression = config.compression.unwrap_or(false);
        let sliding = config.sliding_ttl.unwrap_or(false);

        let shard_capacity = (capacity / shards_count).max(1);
        let max_bytes = config.max_bytes.map(|mb| mb as usize);
        let shard_max_bytes = max_bytes.map(|mb| (mb / shards_count).max(1));

        let shards: Arc<[Shard]> = (0..shards_count)
            .map(|_| {
                let inner: Box<dyn CacheImpl> = match policy_lower.as_str() {
                    "arc" => Box::new(ArcCache::with_options(
                        shard_capacity,
                        shard_max_bytes,
                        sliding,
                    )),
                    "tinylfu" | "w-tinylfu" => Box::new(TinyLfuCache::with_options(
                        shard_capacity,
                        shard_max_bytes,
                        sliding,
                    )),
                    _ => Box::new(LruCache::with_options(
                        shard_capacity,
                        shard_max_bytes,
                        sliding,
                    )),
                };
                parking_lot::Mutex::new(Some(inner))
            })
            .collect();

        Self {
            shards,
            shard_mask: shards_count.is_power_of_two().then(|| shards_count - 1),
            compression,
            hasher: foldhash::fast::RandomState::default(),
            max_bytes,
            has_ttl: Arc::new(AtomicBool::new(false)),
            l1_gens: Arc::new(L1Generations::default()),
            atomic_hits: Arc::new(AtomicU64::new(0)),
            atomic_misses: Arc::new(AtomicU64::new(0)),
            atomic_sets: Arc::new(AtomicU64::new(0)),
            atomic_deletes: Arc::new(AtomicU64::new(0)),
            atomic_evictions: Arc::new(AtomicU64::new(0)),
            atomic_expirations: Arc::new(AtomicU64::new(0)),
            created_at: Arc::new(Instant::now()),
        }
    }

    /// The single hash of a key: routes it to a shard, indexes it inside the shard and
    /// feeds the TinyLFU sketch.
    #[inline]
    fn hash_key(&self, key: &str) -> u64 {
        self.hasher.hash_one(key)
    }

    /// Routes with the upper 32 bits; the shard's hash table consumes the low bits (bucket)
    /// and the top 7 bits (control byte), so the two never correlate.
    #[inline]
    fn shard_for(&self, hash: u64) -> &Shard {
        let high = (hash >> 32) as usize;
        let idx = match self.shard_mask {
            Some(mask) => high & mask,
            None => high % self.shards.len(),
        };
        &self.shards[idx]
    }

    /// Runs `f` with exclusive access to a shard. If `f` panics, the shard stays `None` and
    /// every later call reports it as poisoned instead of touching inconsistent state.
    #[inline]
    fn with_shard<R>(&self, shard: &Shard, f: impl FnOnce(&mut dyn CacheImpl) -> R) -> Result<R> {
        let mut lock = shard.lock();
        let mut cache = lock.take().ok_or_else(disposed_error)?;
        let res = f(cache.as_mut());
        *lock = Some(cache);
        Ok(res)
    }

    #[inline]
    fn note_ttl(&self, ttl: Option<u64>) {
        if ttl.is_some() && !self.has_ttl.load(Ordering::Relaxed) {
            self.has_ttl.store(true, Ordering::Relaxed);
        }
    }

    /// Amortized active expiration, run inside the write's own shard lock.
    #[inline]
    fn sweep_after_write(&self, cache: &mut dyn CacheImpl, evictions: &mut Option<Vec<Eviction>>) {
        if self.has_ttl.load(Ordering::Relaxed) {
            let mut out = Vec::new();
            cache.purge_expired(WRITE_SWEEP_SLOTS, &mut out);
            if !out.is_empty() {
                evictions.get_or_insert_with(Vec::new).extend(out);
            }
        }
    }

    /// Updates counters for a write's displaced entries ("replaced" counts as an eviction).
    fn count_evictions(&self, replaced: bool, evictions: &Option<Vec<Eviction>>) {
        let mut evicted = replaced as u64;
        let mut expired = 0;
        if let Some(list) = evictions {
            for ev in list {
                if ev.reason == "expired" {
                    expired += 1;
                } else {
                    evicted += 1;
                }
            }
        }
        if evicted > 0 {
            self.atomic_evictions.fetch_add(evicted, Ordering::Relaxed);
        }
        if expired > 0 {
            self.atomic_expirations
                .fetch_add(expired, Ordering::Relaxed);
        }
    }

    fn eviction_object(
        &self,
        env: Env,
        key: &str,
        value: &[u8],
        reason: &str,
    ) -> Result<JsUnknown> {
        let mut ev_obj = env.create_object()?;
        ev_obj.set_named_property("key", env.create_string(key)?)?;
        ev_obj.set_named_property("value", self.deserialize_value(env, value)?)?;
        ev_obj.set_named_property("reason", env.create_string(reason)?)?;
        Ok(ev_obj.into_unknown())
    }

    fn expired_object(&self, env: Env, ev: &Eviction) -> Result<JsUnknown> {
        let mut obj = env.create_object()?;
        obj.set_named_property("__expired", env.get_boolean(true)?)?;
        obj.set_named_property("key", env.create_string(&ev.key)?)?;
        obj.set_named_property("value", self.deserialize_value(env, &ev.value)?)?;
        Ok(obj.into_unknown())
    }

    /// Wraps a hit as `[value, remainingTtlMs]` when the entry has a TTL, so the JS
    /// L1 layer can expire its copy at the same instant as L2. Entries without TTL are
    /// returned unwrapped (no extra allocation). Arrays are never produced by
    /// `deserialize_value` (JS arrays travel as JSON strings), so the shape is unambiguous.
    fn wrap_with_ttl(&self, env: Env, value: JsUnknown, ttl: Option<u64>) -> Result<JsUnknown> {
        match ttl {
            None => Ok(value),
            Some(ms) => {
                let mut arr = env.create_array_with_length(2)?;
                arr.set_element(0, value)?;
                arr.set_element(1, env.create_double(ms as f64)?)?;
                Ok(arr.into_unknown())
            }
        }
    }

    fn get_impl(&self, env: Env, key: String, with_ttl: bool) -> Result<JsUnknown> {
        validate_key(&key)?;
        let hash = self.hash_key(&key);
        let (bytes, eviction, ttl) = self.with_shard(self.shard_for(hash), |cache| {
            let (bytes, eviction) = cache.get(hash, &key);
            let ttl = if with_ttl && bytes.is_some() {
                cache.ttl_remaining_ms(hash, &key)
            } else {
                None
            };
            (bytes, eviction, ttl)
        })?;

        if let Some(b) = bytes {
            self.atomic_hits.fetch_add(1, Ordering::Relaxed);
            let val_js = self.deserialize_value(env, &b)?;
            self.wrap_with_ttl(env, val_js, ttl)
        } else if let Some(ev) = eviction {
            self.atomic_misses.fetch_add(1, Ordering::Relaxed);
            self.atomic_expirations.fetch_add(1, Ordering::Relaxed);
            self.expired_object(env, &ev)
        } else {
            self.atomic_misses.fetch_add(1, Ordering::Relaxed);
            Ok(env.get_undefined()?.into_unknown())
        }
    }

    fn mget_impl(&self, env: Env, keys: Vec<String>, with_ttl: bool) -> Result<Vec<JsUnknown>> {
        let mut result = Vec::with_capacity(keys.len());
        let mut hits = 0;
        let mut misses = 0;
        let mut expirations = 0;

        for key in keys {
            validate_key(&key)?;
            let hash = self.hash_key(&key);
            // The shard is released before any fallible JS conversion below.
            let (bytes, eviction, ttl) = self.with_shard(self.shard_for(hash), |cache| {
                let (bytes, eviction) = cache.get(hash, &key);
                let ttl = if with_ttl && bytes.is_some() {
                    cache.ttl_remaining_ms(hash, &key)
                } else {
                    None
                };
                (bytes, eviction, ttl)
            })?;

            let val_res = if let Some(b) = bytes {
                hits += 1;
                self.deserialize_value(env, &b)
                    .and_then(|v| self.wrap_with_ttl(env, v, ttl))
            } else if let Some(ev) = eviction {
                misses += 1;
                expirations += 1;
                self.expired_object(env, &ev)
            } else {
                misses += 1;
                Ok(env.get_undefined()?.into_unknown())
            };
            result.push(val_res?);
        }

        if hits > 0 {
            self.atomic_hits.fetch_add(hits, Ordering::Relaxed);
        }
        if misses > 0 {
            self.atomic_misses.fetch_add(misses, Ordering::Relaxed);
        }
        if expirations > 0 {
            self.atomic_expirations
                .fetch_add(expirations, Ordering::Relaxed);
        }

        Ok(result)
    }

    /// Single-key lookup that reports an expired entry the same way for peek/has/touch.
    fn lookup_result(
        &self,
        env: Env,
        found: Option<JsUnknown>,
        eviction: Option<Eviction>,
        miss_value: impl FnOnce() -> Result<JsUnknown>,
    ) -> Result<JsUnknown> {
        if let Some(v) = found {
            self.atomic_hits.fetch_add(1, Ordering::Relaxed);
            Ok(v)
        } else if let Some(ev) = eviction {
            self.atomic_misses.fetch_add(1, Ordering::Relaxed);
            self.atomic_expirations.fetch_add(1, Ordering::Relaxed);
            self.expired_object(env, &ev)
        } else {
            self.atomic_misses.fetch_add(1, Ordering::Relaxed);
            miss_value()
        }
    }
}

fn validate_key(key: &str) -> Result<()> {
    if key.len() > 8192 {
        return Err(napi::Error::new(
            napi::Status::InvalidArg,
            "Key length exceeds safety limit of 8192 bytes",
        ));
    }
    Ok(())
}

#[napi]
impl Cache {
    #[napi(catch_unwind)]
    pub fn get(&self, env: Env, key: String) -> Result<JsUnknown> {
        self.get_impl(env, key, false)
    }

    /// Same as `get`, but hits on entries with a TTL come back as `[value, remainingTtlMs]`.
    #[napi(catch_unwind)]
    pub fn get_with_ttl(&self, env: Env, key: String) -> Result<JsUnknown> {
        self.get_impl(env, key, true)
    }

    #[napi(catch_unwind)]
    pub fn peek(&self, env: Env, key: String) -> Result<JsUnknown> {
        validate_key(&key)?;
        let hash = self.hash_key(&key);
        let (bytes, eviction) =
            self.with_shard(self.shard_for(hash), |cache| cache.peek(hash, &key))?;
        let found = match bytes {
            Some(b) => Some(self.deserialize_value(env, &b)?),
            None => None,
        };
        self.lookup_result(env, found, eviction, || {
            Ok(env.get_undefined()?.into_unknown())
        })
    }

    #[napi(catch_unwind)]
    pub fn has(&self, env: Env, key: String) -> Result<JsUnknown> {
        validate_key(&key)?;
        let hash = self.hash_key(&key);
        let (has_key, eviction) =
            self.with_shard(self.shard_for(hash), |cache| cache.has(hash, &key))?;
        let found = if has_key {
            Some(env.get_boolean(true)?.into_unknown())
        } else {
            None
        };
        self.lookup_result(env, found, eviction, || {
            Ok(env.get_boolean(false)?.into_unknown())
        })
    }

    #[napi(catch_unwind)]
    pub fn set(
        &self,
        env: Env,
        key: String,
        value: JsUnknown,
        ttl_ms: Option<f64>,
        force_compression: Option<bool>,
    ) -> Result<JsUnknown> {
        validate_key(&key)?;
        let bytes = self.serialize_value(env, value, force_compression)?;
        let ttl = ttl_from_js(ttl_ms);
        self.note_ttl(ttl);
        let hash = self.hash_key(&key);

        let (old_val, evs) = self.with_shard(self.shard_for(hash), |cache| {
            let (old_val, mut evs) = cache.set(hash, &key, bytes, ttl);
            self.sweep_after_write(cache, &mut evs);
            (old_val, evs)
        })?;
        self.l1_gens.bump(&key);

        self.atomic_sets.fetch_add(1, Ordering::Relaxed);
        self.count_evictions(old_val.is_some(), &evs);

        if old_val.is_none() && evs.is_none() {
            return Ok(env.get_undefined()?.into_unknown());
        }

        let mut all_evictions = Vec::new();
        if let Some(old) = old_val {
            all_evictions.push(self.eviction_object(env, &key, &old, "replaced")?);
        }
        for ev in evs.into_iter().flatten() {
            all_evictions.push(self.eviction_object(env, &ev.key, &ev.value, ev.reason)?);
        }

        let mut js_arr = env.create_array_with_length(all_evictions.len())?;
        for (i, val) in all_evictions.into_iter().enumerate() {
            js_arr.set_element(i as u32, val)?;
        }
        Ok(js_arr.into_unknown())
    }

    #[napi(catch_unwind)]
    pub fn touch(&self, env: Env, key: String, ttl_ms: Option<f64>) -> Result<JsUnknown> {
        validate_key(&key)?;
        let ttl = ttl_from_js(ttl_ms);
        self.note_ttl(ttl);
        let hash = self.hash_key(&key);
        let (res, eviction) =
            self.with_shard(self.shard_for(hash), |cache| cache.touch(hash, &key, ttl))?;
        self.l1_gens.bump(&key);
        let found = if res {
            Some(env.get_boolean(true)?.into_unknown())
        } else {
            None
        };
        self.lookup_result(env, found, eviction, || {
            Ok(env.get_boolean(false)?.into_unknown())
        })
    }

    #[napi(catch_unwind)]
    pub fn delete(&self, key: String) -> Result<bool> {
        validate_key(&key)?;
        let hash = self.hash_key(&key);
        let res = self.with_shard(self.shard_for(hash), |cache| cache.delete(hash, &key))?;
        self.l1_gens.bump(&key);
        if res {
            self.atomic_deletes.fetch_add(1, Ordering::Relaxed);
        }
        Ok(res)
    }

    #[napi(catch_unwind)]
    pub fn clear(&self) -> Result<()> {
        for shard in self.shards.iter() {
            self.with_shard(shard, |cache| cache.clear())?;
        }
        self.l1_gens.bump_all();
        Ok(())
    }

    /// Active expiration over every shard. Scans up to `maxScanPerShard` slots per shard
    /// (default: all of them). Returns the expired entries as eviction objects when
    /// `collect` is true (for `onExpire` hooks), otherwise just their count.
    #[napi(catch_unwind)]
    pub fn purge_expired(
        &self,
        env: Env,
        max_scan_per_shard: Option<u32>,
        collect: Option<bool>,
    ) -> Result<JsUnknown> {
        let max_scan = max_scan_per_shard.map_or(usize::MAX, |n| n as usize);
        let mut expired = Vec::new();
        for shard in self.shards.iter() {
            self.with_shard(shard, |cache| cache.purge_expired(max_scan, &mut expired))?;
        }
        if !expired.is_empty() {
            self.atomic_expirations
                .fetch_add(expired.len() as u64, Ordering::Relaxed);
        }
        if !collect.unwrap_or(false) {
            return Ok(env.create_uint32(expired.len() as u32)?.into_unknown());
        }
        let mut js_arr = env.create_array_with_length(expired.len())?;
        for (i, ev) in expired.iter().enumerate() {
            js_arr.set_element(
                i as u32,
                self.eviction_object(env, &ev.key, &ev.value, ev.reason)?,
            )?;
        }
        Ok(js_arr.into_unknown())
    }

    /// Registers a new table of L1 generation counters for the calling JS wrapper and
    /// returns it as an ArrayBuffer of `L1_GEN_SLOTS` u32 values. Every write to a key
    /// bumps `table[l1Slot(key)]` in all registered tables, letting each thread's L1 drop
    /// entries modified by other threads. The table is unregistered when the buffer is GC'd.
    #[napi(catch_unwind)]
    pub fn l1_generations(&self, env: Env) -> Result<JsArrayBuffer> {
        let table = Arc::new(GenTable {
            slots: (0..L1_GEN_SLOTS).map(|_| AtomicU32::new(0)).collect(),
        });
        {
            let mut tables = self.l1_gens.tables.write();
            tables.push(Arc::clone(&table));
            self.l1_gens.count.store(tables.len(), Ordering::Release);
        }
        let data = table.slots.as_ptr() as *mut u8;
        let registry = Arc::clone(&self.l1_gens);
        // SAFETY: `data` points into `table`, which is moved into the finalizer hint and so
        // outlives the ArrayBuffer. AtomicU32 has the same layout as u32.
        let buffer = unsafe {
            env.create_arraybuffer_with_borrowed_data(
                data,
                L1_GEN_SLOTS * std::mem::size_of::<AtomicU32>(),
                table,
                move |table: Arc<GenTable>, _env| {
                    let mut tables = registry.tables.write();
                    tables.retain(|t| !Arc::ptr_eq(t, &table));
                    registry.count.store(tables.len(), Ordering::Release);
                },
            )
        }?;
        Ok(buffer.into_raw())
    }

    #[napi(catch_unwind)]
    pub fn stats(&self) -> Result<CacheStatsJs> {
        let hits = self.atomic_hits.load(Ordering::Relaxed);
        let misses = self.atomic_misses.load(Ordering::Relaxed);
        let sets = self.atomic_sets.load(Ordering::Relaxed);
        let deletes = self.atomic_deletes.load(Ordering::Relaxed);
        let evictions = self.atomic_evictions.load(Ordering::Relaxed);
        let expirations = self.atomic_expirations.load(Ordering::Relaxed);

        let total = hits + misses;
        let hit_rate = if total > 0 {
            hits as f64 / total as f64
        } else {
            0.0
        };

        let mut total_capacity = 0.0;
        let mut total_size = 0.0;
        let mut total_bytes = 0.0;
        let mut shards_stats = Vec::with_capacity(self.shards.len());

        for shard in self.shards.iter() {
            let lock = shard.lock();
            let cache = lock.as_ref().ok_or_else(disposed_error)?;
            let stats = cache.stats();
            total_capacity += stats.capacity as f64;
            total_size += stats.size as f64;
            total_bytes += stats.bytes_used as f64;

            shards_stats.push(ShardStatsJs {
                size: stats.size as f64,
                bytes_used: stats.bytes_used as f64,
            });
        }

        let uptime_ms = self.created_at.elapsed().as_millis() as f64;

        Ok(CacheStatsJs {
            hits: hits as f64,
            misses: misses as f64,
            capacity: total_capacity,
            size: total_size,
            bytes_used: total_bytes,
            sets: sets as f64,
            deletes: deletes as f64,
            evictions: evictions as f64,
            expirations: expirations as f64,
            hit_rate,
            uptime_ms,
            shards: shards_stats,
        })
    }

    #[napi(catch_unwind)]
    pub fn stats_counters(&self) -> Result<CacheCountersJs> {
        Ok(CacheCountersJs {
            hits: self.atomic_hits.load(Ordering::Relaxed) as f64,
            misses: self.atomic_misses.load(Ordering::Relaxed) as f64,
            sets: self.atomic_sets.load(Ordering::Relaxed) as f64,
            deletes: self.atomic_deletes.load(Ordering::Relaxed) as f64,
            evictions: self.atomic_evictions.load(Ordering::Relaxed) as f64,
            expirations: self.atomic_expirations.load(Ordering::Relaxed) as f64,
        })
    }

    #[napi(catch_unwind)]
    pub fn keys(&self) -> Result<Vec<String>> {
        let mut all_keys = Vec::new();
        for shard in self.shards.iter() {
            let lock = shard.lock();
            let cache = lock.as_ref().ok_or_else(disposed_error)?;
            all_keys.extend(cache.keys());
        }
        Ok(all_keys)
    }

    #[napi(catch_unwind)]
    pub fn dispose(&self) -> Result<()> {
        for shard in self.shards.iter() {
            let mut lock = shard.lock();
            *lock = None;
        }
        self.l1_gens.bump_all();
        Ok(())
    }

    #[napi(catch_unwind)]
    pub fn increment(
        &self,
        env: Env,
        key: String,
        delta: i64,
        ttl_ms: Option<f64>,
    ) -> Result<JsUnknown> {
        validate_key(&key)?;
        let ttl = ttl_from_js(ttl_ms);
        self.note_ttl(ttl);
        let hash = self.hash_key(&key);

        let (is_hit, res) = self.with_shard(self.shard_for(hash), |cache| {
            let mut current_val = 0i64;
            let (bytes_opt, _eviction) = cache.get(hash, &key);
            let is_hit = bytes_opt.is_some();
            if let Some(bytes) = bytes_opt {
                if !bytes.is_empty() {
                    let tag = bytes[0];
                    let payload = &bytes[1..];
                    match tag {
                        TAG_I64 => {
                            if payload.len() == 8 {
                                current_val = i64::from_le_bytes(payload.try_into().unwrap());
                            } else {
                                return (
                                    is_hit,
                                    Err(napi::Error::new(
                                        napi::Status::InvalidArg,
                                        "Counter value is corrupted",
                                    )),
                                );
                            }
                        }
                        TAG_JSON => {
                            let parsed = std::str::from_utf8(payload)
                                .ok()
                                .and_then(|s| s.parse::<i64>().ok());
                            match parsed {
                                Some(val) => current_val = val,
                                None => {
                                    return (
                                        is_hit,
                                        Err(napi::Error::new(
                                            napi::Status::InvalidArg,
                                            "Value is not a valid 64-bit integer",
                                        )),
                                    )
                                }
                            }
                        }
                        _ => {
                            return (
                                is_hit,
                                Err(napi::Error::new(
                                    napi::Status::InvalidArg,
                                    "Value is not a numeric counter",
                                )),
                            )
                        }
                    }
                }
            }

            let new_val = current_val.wrapping_add(delta);
            let mut new_bytes = Vec::with_capacity(9);
            new_bytes.push(TAG_I64);
            new_bytes.extend_from_slice(&new_val.to_le_bytes());
            let _ = cache.set(hash, &key, Bytes::from(new_bytes), ttl);
            (is_hit, Ok(new_val))
        })?;

        let new_val = res?;
        self.l1_gens.bump(&key);
        if is_hit {
            self.atomic_hits.fetch_add(1, Ordering::Relaxed);
        } else {
            self.atomic_misses.fetch_add(1, Ordering::Relaxed);
        }
        self.atomic_sets.fetch_add(1, Ordering::Relaxed);
        Ok(env.create_double(new_val as f64)?.into_unknown())
    }

    #[napi(catch_unwind)]
    pub fn decrement(
        &self,
        env: Env,
        key: String,
        delta: i64,
        ttl_ms: Option<f64>,
    ) -> Result<JsUnknown> {
        self.increment(env, key, -delta, ttl_ms)
    }

    #[napi(catch_unwind)]
    pub fn mget(&self, env: Env, keys: Vec<String>) -> Result<Vec<JsUnknown>> {
        self.mget_impl(env, keys, false)
    }

    /// Same as `mget`, but hits on entries with a TTL come back as `[value, remainingTtlMs]`.
    #[napi(catch_unwind)]
    pub fn mget_with_ttl(&self, env: Env, keys: Vec<String>) -> Result<Vec<JsUnknown>> {
        self.mget_impl(env, keys, true)
    }

    #[napi(catch_unwind)]
    pub fn mset(&self, env: Env, entries: JsObject, ttl_ms: Option<f64>) -> Result<JsUnknown> {
        let keys_array = entries.get_property_names()?;
        let len = keys_array.get_array_length()?;
        let ttl = ttl_from_js(ttl_ms);
        self.note_ttl(ttl);
        let mut collected_evictions: Vec<(String, Bytes, &'static str)> = Vec::new();
        let mut sets = 0;

        for i in 0..len {
            let key_unknown: JsUnknown = keys_array.get_element(i)?;
            let key_str = key_unknown.coerce_to_string()?;
            let key = key_str.into_utf8()?.into_owned()?;
            validate_key(&key)?;

            let val: JsUnknown = entries.get_named_property(&key)?;
            let bytes = self.serialize_value(env, val, None)?;
            let hash = self.hash_key(&key);

            let (old_val, evs) = self.with_shard(self.shard_for(hash), |cache| {
                let (old_val, mut evs) = cache.set(hash, &key, bytes, ttl);
                self.sweep_after_write(cache, &mut evs);
                (old_val, evs)
            })?;
            self.l1_gens.bump(&key);

            sets += 1;
            self.count_evictions(old_val.is_some(), &evs);
            if let Some(old) = old_val {
                collected_evictions.push((key, old, "replaced"));
            }
            for ev in evs.into_iter().flatten() {
                collected_evictions.push((ev.key, ev.value, ev.reason));
            }
        }

        if sets > 0 {
            self.atomic_sets.fetch_add(sets, Ordering::Relaxed);
        }

        if collected_evictions.is_empty() {
            return Ok(env.get_undefined()?.into_unknown());
        }

        let mut js_arr = env.create_array_with_length(collected_evictions.len())?;
        for (i, (key, value, reason)) in collected_evictions.iter().enumerate() {
            js_arr.set_element(i as u32, self.eviction_object(env, key, value, reason)?)?;
        }
        Ok(js_arr.into_unknown())
    }

    #[napi(catch_unwind)]
    pub fn mdelete(&self, keys: Vec<String>) -> Result<u32> {
        let mut count = 0;
        for key in keys {
            validate_key(&key)?;
            let hash = self.hash_key(&key);
            let deleted =
                self.with_shard(self.shard_for(hash), |cache| cache.delete(hash, &key))?;
            self.l1_gens.bump(&key);
            if deleted {
                count += 1;
            }
        }
        if count > 0 {
            self.atomic_deletes
                .fetch_add(count as u64, Ordering::Relaxed);
        }
        Ok(count)
    }

    #[napi(catch_unwind)]
    pub fn test_deserialize_raw(&self, env: Env, bytes: Vec<u8>) -> Result<JsUnknown> {
        self.deserialize_value(env, &bytes)
    }

    #[napi(catch_unwind)]
    pub fn test_panic(&self, key: String) {
        let hash = self.hash_key(&key);
        let mut lock = self.shard_for(hash).lock();
        let mut _cache = lock.take().ok_or_else(disposed_error).unwrap();
        panic!("Intended test panic in Rust code!");
    }
}

#[cfg(test)]
mod tests {
    use super::serde::{TAG_BUFFER, TAG_LZ4_JSON, TAG_STRING};
    use super::*;
    use crate::algorithms::test_hash;

    #[test]
    fn l1_slot_matches_js_fnv1a_over_utf16() {
        // Same algorithm as l1Slot() in index.js: FNV-1a 32-bit over UTF-16 code units.
        assert_eq!(l1_slot(""), 0x811c_9dc5 & (L1_GEN_SLOTS - 1));
        let manual = |s: &str| {
            let mut h: u32 = 0x811c_9dc5;
            for u in s.encode_utf16() {
                h = (h ^ u as u32).wrapping_mul(16_777_619);
            }
            (h as usize) & (L1_GEN_SLOTS - 1)
        };
        for key in ["a", "user:42", "çãõ", "😀 emoji"] {
            assert_eq!(l1_slot(key), manual(key));
        }
    }

    #[test]
    fn auto_shards_keeps_shards_useful() {
        assert_eq!(auto_shards(10), 1);
        assert!(10_000 / auto_shards(10_000) >= 64);
        assert!(auto_shards(10_000_000).is_power_of_two());
    }

    // ── Helpers ──────────────────────────────────────────────────────────────
    fn encode_i64(val: i64) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(9);
        bytes.push(TAG_I64);
        bytes.extend_from_slice(&val.to_le_bytes());
        bytes
    }

    fn encode_string(s: &str) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(1 + s.len());
        bytes.push(TAG_STRING);
        bytes.extend_from_slice(s.as_bytes());
        bytes
    }

    fn encode_json(json: &str) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(1 + json.len());
        bytes.push(TAG_JSON);
        bytes.extend_from_slice(json.as_bytes());
        bytes
    }

    fn encode_buffer(data: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(1 + data.len());
        bytes.push(TAG_BUFFER);
        bytes.extend_from_slice(data);
        bytes
    }

    // ── TAG constants ─────────────────────────────────────────────────────────

    #[test]
    fn tag_constants_are_unique_and_nonzero() {
        let tags = [TAG_BUFFER, TAG_STRING, TAG_JSON, TAG_I64, TAG_LZ4_JSON];
        for &t in &tags {
            assert_ne!(t, 0, "Tag {t} must not be zero");
        }
        let mut seen = std::collections::HashSet::new();
        for &t in &tags {
            assert!(seen.insert(t), "Duplicate tag value: {t}");
        }
    }

    // ── Serialization encoding ────────────────────────────────────────────────

    #[test]
    fn encode_i64_uses_little_endian() {
        let encoded = encode_i64(1);
        assert_eq!(encoded[0], TAG_I64);
        assert_eq!(&encoded[1..], &[1u8, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn encode_i64_negative_value() {
        let val: i64 = -1;
        let encoded = encode_i64(val);
        assert_eq!(encoded[0], TAG_I64);
        let recovered = i64::from_le_bytes(encoded[1..].try_into().unwrap());
        assert_eq!(recovered, val);
    }

    #[test]
    fn encode_i64_min_max_roundtrip() {
        for val in [i64::MIN, i64::MAX, 0i64, 42, -42] {
            let encoded = encode_i64(val);
            let recovered = i64::from_le_bytes(encoded[1..].try_into().unwrap());
            assert_eq!(recovered, val, "Roundtrip failed for {val}");
        }
    }

    #[test]
    fn encode_string_tag_and_content() {
        let s = "hello";
        let encoded = encode_string(s);
        assert_eq!(encoded[0], TAG_STRING);
        assert_eq!(&encoded[1..], s.as_bytes());
    }

    #[test]
    fn encode_json_tag_and_content() {
        let json = r#"{"x":1}"#;
        let encoded = encode_json(json);
        assert_eq!(encoded[0], TAG_JSON);
        assert_eq!(&encoded[1..], json.as_bytes());
    }

    #[test]
    fn encode_buffer_tag_and_bytes() {
        let data: &[u8] = &[0xDE, 0xAD, 0xBE, 0xEF];
        let encoded = encode_buffer(data);
        assert_eq!(encoded[0], TAG_BUFFER);
        assert_eq!(&encoded[1..], data);
    }

    // ── Deserialization decoding (pure byte parsing, no NAPI) ─────────────────

    #[test]
    fn decode_i64_little_endian_correctness() {
        let raw: [u8; 8] = 42i64.to_le_bytes();
        let decoded = i64::from_le_bytes(raw);
        assert_eq!(decoded, 42);
    }

    #[test]
    fn decode_i64_endianness_stability() {
        let encoded = encode_i64(256);
        assert_eq!(encoded[1], 0x00, "byte 0 of 256");
        assert_eq!(encoded[2], 0x01, "byte 1 of 256");
        for &b in &encoded[3..] {
            assert_eq!(b, 0x00);
        }
    }

    #[test]
    fn invalid_tag_zero_is_unrecognized() {
        let unknown_tag: u8 = 0;
        assert_ne!(unknown_tag, TAG_BUFFER);
        assert_ne!(unknown_tag, TAG_STRING);
        assert_ne!(unknown_tag, TAG_JSON);
        assert_ne!(unknown_tag, TAG_I64);
        assert_ne!(unknown_tag, TAG_LZ4_JSON);
    }

    #[test]
    fn lz4_decompression_size_prefix_is_four_bytes_le() {
        let original = b"hello world from offheap";
        let compressed = lz4_flex::compress_prepend_size(original);
        let size_bytes: [u8; 4] = compressed[0..4].try_into().unwrap();
        let decoded_size = u32::from_le_bytes(size_bytes) as usize;
        assert_eq!(decoded_size, original.len());
    }

    // ── validate_key ──────────────────────────────────────────────────────────

    #[test]
    fn validate_key_accepts_normal_keys() {
        assert!(validate_key("hello").is_ok());
        assert!(validate_key("").is_ok());
        assert!(validate_key("unicode_日本語").is_ok());
    }

    #[test]
    fn validate_key_rejects_oversized_key() {
        let long_key = "a".repeat(8193);
        let result = validate_key(&long_key);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.reason.contains("8192"),
            "Error message must mention the limit"
        );
    }

    #[test]
    fn validate_key_accepts_exactly_8192_bytes() {
        let max_key = "a".repeat(8192);
        assert!(validate_key(&max_key).is_ok());
    }

    // ── LruCache via CacheImpl (integration without NAPI) ────────────────────

    #[test]
    fn lru_set_and_get_roundtrip() {
        let mut cache = LruCache::new(10);
        let value = encode_string("test_value");
        cache.set(test_hash("key1"), "key1", Bytes::from(value.clone()), None);
        let (result, eviction) = cache.get(test_hash("key1"), "key1");
        assert!(eviction.is_none());
        assert_eq!(result.unwrap(), value);
    }

    #[test]
    fn lru_evicts_lru_on_capacity_exceeded() {
        let mut cache = LruCache::new(2);
        cache.set(test_hash("a"), "a", Bytes::from(encode_string("v_a")), None);
        cache.set(test_hash("b"), "b", Bytes::from(encode_string("v_b")), None);
        cache.get(test_hash("a"), "a");
        cache.set(test_hash("c"), "c", Bytes::from(encode_string("v_c")), None);
        let (b_val, _) = cache.get(test_hash("b"), "b");
        assert!(b_val.is_none(), "b should have been evicted");
        let (a_val, _) = cache.get(test_hash("a"), "a");
        assert!(a_val.is_some(), "a should still be in cache");
    }

    #[test]
    fn lru_delete_removes_key() {
        let mut cache = LruCache::new(5);
        cache.set(test_hash("key"), "key", Bytes::from(encode_i64(100)), None);
        assert!(cache.delete(test_hash("key"), "key"));
        assert!(!cache.delete(test_hash("key"), "key"));
        let (val, _) = cache.get(test_hash("key"), "key");
        assert!(val.is_none());
    }

    #[test]
    fn lru_ttl_expiry_returns_eviction() {
        let mut cache = LruCache::new(5);
        cache.set(
            test_hash("temp"),
            "temp",
            Bytes::from(encode_string("value")),
            Some(1),
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
        let (val, eviction) = cache.get(test_hash("temp"), "temp");
        assert!(val.is_none(), "expired key must not return a value");
        assert!(
            eviction.is_some(),
            "expired key must return an eviction record"
        );
        let ev = eviction.unwrap();
        assert_eq!(ev.key, "temp");
        assert_eq!(ev.reason, "expired");
    }

    #[test]
    fn lru_peek_does_not_promote_key() {
        let mut cache = LruCache::new(2);
        cache.set(test_hash("a"), "a", Bytes::from(encode_string("v_a")), None);
        cache.set(test_hash("b"), "b", Bytes::from(encode_string("v_b")), None);
        let (peeked, _) = cache.peek(test_hash("a"), "a");
        assert!(peeked.is_some());
        cache.set(test_hash("c"), "c", Bytes::from(encode_string("v_c")), None);
        let (a_val, _) = cache.get(test_hash("a"), "a");
        assert!(a_val.is_none(), "peek must not prevent eviction of a");
    }

    #[test]
    fn lru_clear_empties_cache() {
        let mut cache = LruCache::new(5);
        for i in 0..5 {
            cache.set(
                test_hash(&format!("k{i}")),
                &format!("k{i}"),
                Bytes::from(encode_i64(i as i64)),
                None,
            );
        }
        assert_eq!(cache.stats().size, 5);
        cache.clear();
        assert_eq!(cache.stats().size, 0);
        let (val, _) = cache.get(test_hash("k0"), "k0");
        assert!(val.is_none());
    }

    #[test]
    fn lru_max_bytes_triggers_eviction() {
        let mut cache = LruCache::new_with_max_bytes(100, Some(5));
        cache.set(test_hash("a"), "a", Bytes::from(encode_string("v")), None);
        cache.set(test_hash("b"), "b", Bytes::from(encode_string("v")), None);
        cache.set(test_hash("c"), "c", Bytes::from(encode_string("v")), None);
        let stats = cache.stats();
        assert!(
            stats.bytes_used <= 5,
            "bytes_used {} must not exceed maxBytes 5",
            stats.bytes_used
        );
    }

    // ── Counter increment/decrement encoding ──────────────────────────────────

    #[test]
    fn i64_counter_encoding_survives_lru_roundtrip() {
        let mut cache = LruCache::new(5);
        let initial: i64 = 0;
        let delta: i64 = 7;
        let new_val = initial + delta;

        let encoded = encode_i64(new_val);
        cache.set(test_hash("counter"), "counter", Bytes::from(encoded), None);

        let (bytes, _) = cache.get(test_hash("counter"), "counter");
        let bytes = bytes.expect("counter must exist");
        assert_eq!(bytes[0], TAG_I64, "tag must be TAG_I64");
        let recovered = i64::from_le_bytes(bytes[1..].try_into().unwrap());
        assert_eq!(recovered, 7);
    }

    #[test]
    fn i64_counter_negative_encoding() {
        let val: i64 = -999;
        let encoded = encode_i64(val);
        let recovered = i64::from_le_bytes(encoded[1..].try_into().unwrap());
        assert_eq!(recovered, val);
    }
}
