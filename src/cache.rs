use std::sync::Arc;
use std::hash::BuildHasher;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;
use bytes::Bytes;
use napi::{Env, JsArrayBuffer, JsBuffer, JsString, JsObject, JsUnknown, Result, ValueType};
use super::algorithms::{CacheImpl, Eviction, lru::LruCache, arc::ArcCache, tinylfu::TinyLfuCache};

/// Expired-entry slots inspected (per shard) after every write while TTLs are in use.
/// Keeps memory held by never-read expired entries bounded without a background thread.
const WRITE_SWEEP_SLOTS: usize = 4;

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
struct GenTable {
    slots: Box<[AtomicU32]>,
}

#[derive(Default)]
struct L1Generations {
    tables: parking_lot::RwLock<Vec<Arc<GenTable>>>,
    count: AtomicUsize,
}

impl L1Generations {
    #[inline]
    fn bump(&self, key: &str) {
        if self.count.load(Ordering::Acquire) == 0 {
            return;
        }
        let slot = l1_slot(key);
        for table in self.tables.read().iter() {
            table.slots[slot].fetch_add(1, Ordering::Release);
        }
    }

    fn bump_all(&self) {
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

type Shard = parking_lot::Mutex<Option<Box<dyn CacheImpl>>>;

#[napi]
pub struct Cache {
    shards: Arc<[Shard]>,
    shard_mask: Option<usize>,
    compression: bool,
    hasher: foldhash::fast::RandomState,
    max_bytes: Option<usize>,
    has_ttl: Arc<AtomicBool>,
    l1_gens: Arc<L1Generations>,
    atomic_hits: Arc<AtomicU64>,
    atomic_misses: Arc<AtomicU64>,
    atomic_sets: Arc<AtomicU64>,
    atomic_deletes: Arc<AtomicU64>,
    atomic_evictions: Arc<AtomicU64>,
    atomic_expirations: Arc<AtomicU64>,
    created_at: Arc<Instant>,
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

/// Default shard count: 4 per CPU (power of two, 8..=256), halved until every shard
/// holds at least 64 entries so small caches are not split into near-empty shards.
fn auto_shards(capacity: usize) -> usize {
    let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let mut shards = (cpus * 4).next_power_of_two().clamp(8, 256);
    while shards > 1 && capacity / shards < 64 {
        shards /= 2;
    }
    shards
}

fn disposed_error() -> napi::Error {
    napi::Error::new(napi::Status::GenericFailure, "Cache has been disposed or poisoned")
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
                    "arc" => Box::new(ArcCache::with_options(shard_capacity, shard_max_bytes, sliding)),
                    "tinylfu" | "w-tinylfu" => Box::new(TinyLfuCache::with_options(shard_capacity, shard_max_bytes, sliding)),
                    _ => Box::new(LruCache::with_options(shard_capacity, shard_max_bytes, sliding)),
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
            self.atomic_expirations.fetch_add(expired, Ordering::Relaxed);
        }
    }

    fn eviction_object(&self, env: Env, key: &str, value: &[u8], reason: &str) -> Result<JsUnknown> {
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

    /// ========================================================================
    /// 📂 SERIALIZATION TAG REGISTRY
    /// ========================================================================
    /// Every entry written off-heap is prefixed with a 1-byte protocol header
    /// (Tag) that determines how the trailing slice payload is deserialized:
    ///
    ///   🏷️ CATEGORY RANGES:
    ///     [1 - 20]   : Core payload datatypes & baseline formats
    ///     [21 - 90]  : Reserved for future structured formats (e.g., MsgPack, Protobuf)
    ///     [91 - 99]  : Reserved for internal testing, diagnostics, & custom overrides
    ///
    ///   📌 TAGS IN USE:
    ///     Tag 1:  Raw Binary Buffer (Stored contiguously in native memory)
    ///     Tag 2:  Raw UTF-8 String (Decoded directly into v8::String)
    ///     Tag 3:  Raw JSON String (Raw text JSON representation of JS objects)
    ///     Tag 4:  Atomic Counter (64-bit signed integer representation)
    ///     Tag 5:  LZ4 Compressed JSON String (Decompressed on demand, self-describing)
    ///     Tag 99: Test Sentinel (Reserved for diagnostic tests, do not use in production)
    ///
    /// The JS wrapper guarantees that a plain string never starts with "\0" (such strings
    /// are sent JSON-encoded), so the "\0J" JSON marker below is unambiguous.
    ///
    /// When expanding types:
    ///   1. Register the tag code here following the range guidelines above.
    ///   2. Handle serialization tag routing in `serialize_value`.
    ///   3. Implement safety catches for new tags in `deserialize_value`.
    /// ========================================================================
    fn serialize_value(&self, _env: Env, value: JsUnknown, force_compression: Option<bool>) -> Result<Bytes> {
        let mut bytes = Vec::new();
        let value_type = value.get_type()?;

        if value.is_buffer()? {
            let buf = JsBuffer::try_from(value)?;
            let data = buf.into_value()?;
            bytes.reserve_exact(1 + data.len());
            bytes.push(1); // Tag: Buffer
            bytes.extend_from_slice(&data);
        } else if value_type == ValueType::String {
            let js_str = JsString::try_from(value)?;
            let utf8 = js_str.into_utf8()?;
            let slice = utf8.as_slice();
            if slice.starts_with(b"\0J") {
                let compress = force_compression.unwrap_or(self.compression);
                if compress {
                    bytes.push(5); // Tag: Compressed JSON
                    let compressed = lz4_flex::compress_prepend_size(&slice[2..]);
                    bytes.extend_from_slice(&compressed);
                } else {
                    bytes.reserve_exact(slice.len() - 1);
                    bytes.push(3); // Tag: JSON (Raw)
                    bytes.extend_from_slice(&slice[2..]);
                }
            } else {
                bytes.reserve_exact(1 + slice.len());
                bytes.push(2); // Tag: String
                bytes.extend_from_slice(slice);
            }
        } else if value_type == ValueType::Number {
            let num = value.coerce_to_number()?;
            let val = num.get_double()?;
            if val.fract() == 0.0 && val >= (i64::MIN as f64) && val <= (i64::MAX as f64) {
                bytes.push(4); // Tag: i64
                bytes.extend_from_slice(&(val as i64).to_ne_bytes());
            } else {
                let s = val.to_string();
                let compress = force_compression.unwrap_or(self.compression);
                if compress {
                    bytes.push(5); // Tag: Compressed JSON
                    let compressed = lz4_flex::compress_prepend_size(s.as_bytes());
                    bytes.extend_from_slice(&compressed);
                } else {
                    bytes.push(3); // Tag: JSON (Raw)
                    bytes.extend_from_slice(s.as_bytes());
                }
            }
        } else {
            return Err(napi::Error::new(napi::Status::InvalidArg, "Complex types must be serialized to JSON in JS wrapper"));
        }
        Ok(Bytes::from(bytes))
    }

    fn deserialize_value(&self, env: Env, bytes: &[u8]) -> Result<JsUnknown> {
        if bytes.is_empty() {
            return Ok(env.get_undefined()?.into_unknown());
        }
        let tag = bytes[0];
        let payload = &bytes[1..];
        match tag {
            1 => {
                let js_buf = env.create_buffer_copy(payload)?;
                Ok(js_buf.into_unknown())
            }
            2 => {
                let s = std::str::from_utf8(payload)
                    .map_err(|e| napi::Error::new(napi::Status::StringExpected, e.to_string()))?;
                let js_str = env.create_string(s)?;
                Ok(js_str.into_unknown())
            }
            3 => {
                let s = std::str::from_utf8(payload)
                    .map_err(|e| napi::Error::new(napi::Status::StringExpected, e.to_string()))?;
                let mut prefixed = String::with_capacity(2 + s.len());
                prefixed.push('\0');
                prefixed.push('J');
                prefixed.push_str(s);
                let js_str = env.create_string(&prefixed)?;
                Ok(js_str.into_unknown())
            }
            4 => {
                if payload.len() == 8 {
                    let val = i64::from_ne_bytes(payload.try_into().unwrap());
                    let js_num = env.create_double(val as f64)?;
                    Ok(js_num.into_unknown())
                } else {
                    Err(napi::Error::new(napi::Status::InvalidArg, "Counter value is corrupted"))
                }
            }
            5 => {
                if payload.len() < 4 {
                    return Err(napi::Error::new(napi::Status::InvalidArg, "Compressed payload is too short"));
                }
                let uncompressed_size = u32::from_le_bytes(payload[0..4].try_into().unwrap()) as usize;

                // Enforce safety limit of min(32 MB, max_bytes * 0.1) to prevent decompression bombs and OOM allocation panics
                let mut limit = 32 * 1024 * 1024;
                if let Some(mb) = self.max_bytes {
                    limit = limit.min((mb as f64 * 0.1) as usize).max(1024);
                }
                if uncompressed_size > limit {
                    return Err(napi::Error::new(
                        napi::Status::InvalidArg,
                        format!("Decompressed size {} exceeds safety limit of {} bytes", uncompressed_size, limit)
                    ));
                }

                // Decompress straight after the "\0J" marker to avoid a second copy.
                let mut prefixed = vec![0u8; 2 + uncompressed_size];
                prefixed[1] = b'J';
                lz4_flex::block::decompress_into(&payload[4..], &mut prefixed[2..])
                    .map_err(|e| napi::Error::new(napi::Status::GenericFailure, format!("LZ4 Decompression failed: {}", e)))?;

                let s = std::str::from_utf8(&prefixed)
                    .map_err(|e| napi::Error::new(napi::Status::StringExpected, e.to_string()))?;
                let js_str = env.create_string(s)?;
                Ok(js_str.into_unknown())
            }
            _ => Err(napi::Error::new(napi::Status::InvalidArg, "Invalid data type tag in cache storage")),
        }
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
            let ttl = if with_ttl && bytes.is_some() { cache.ttl_remaining_ms(hash, &key) } else { None };
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
                let ttl = if with_ttl && bytes.is_some() { cache.ttl_remaining_ms(hash, &key) } else { None };
                (bytes, eviction, ttl)
            })?;

            let val_res = if let Some(b) = bytes {
                hits += 1;
                self.deserialize_value(env, &b).and_then(|v| self.wrap_with_ttl(env, v, ttl))
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
            self.atomic_expirations.fetch_add(expirations, Ordering::Relaxed);
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
        return Err(napi::Error::new(napi::Status::InvalidArg, "Key length exceeds safety limit of 8192 bytes"));
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
        let (bytes, eviction) = self.with_shard(self.shard_for(hash), |cache| cache.peek(hash, &key))?;
        let found = match bytes {
            Some(b) => Some(self.deserialize_value(env, &b)?),
            None => None,
        };
        self.lookup_result(env, found, eviction, || Ok(env.get_undefined()?.into_unknown()))
    }

    #[napi(catch_unwind)]
    pub fn has(&self, env: Env, key: String) -> Result<JsUnknown> {
        validate_key(&key)?;
        let hash = self.hash_key(&key);
        let (has_key, eviction) = self.with_shard(self.shard_for(hash), |cache| cache.has(hash, &key))?;
        let found = if has_key { Some(env.get_boolean(true)?.into_unknown()) } else { None };
        self.lookup_result(env, found, eviction, || Ok(env.get_boolean(false)?.into_unknown()))
    }

    #[napi(catch_unwind)]
    pub fn set(&self, env: Env, key: String, value: JsUnknown, ttl_ms: Option<f64>, force_compression: Option<bool>) -> Result<JsUnknown> {
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
        let (res, eviction) = self.with_shard(self.shard_for(hash), |cache| cache.touch(hash, &key, ttl))?;
        self.l1_gens.bump(&key);
        let found = if res { Some(env.get_boolean(true)?.into_unknown()) } else { None };
        self.lookup_result(env, found, eviction, || Ok(env.get_boolean(false)?.into_unknown()))
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
    pub fn purge_expired(&self, env: Env, max_scan_per_shard: Option<u32>, collect: Option<bool>) -> Result<JsUnknown> {
        let max_scan = max_scan_per_shard.map_or(usize::MAX, |n| n as usize);
        let mut expired = Vec::new();
        for shard in self.shards.iter() {
            self.with_shard(shard, |cache| cache.purge_expired(max_scan, &mut expired))?;
        }
        if !expired.is_empty() {
            self.atomic_expirations.fetch_add(expired.len() as u64, Ordering::Relaxed);
        }
        if !collect.unwrap_or(false) {
            return Ok(env.create_uint32(expired.len() as u32)?.into_unknown());
        }
        let mut js_arr = env.create_array_with_length(expired.len())?;
        for (i, ev) in expired.iter().enumerate() {
            js_arr.set_element(i as u32, self.eviction_object(env, &ev.key, &ev.value, ev.reason)?)?;
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
        let hit_rate = if total > 0 { hits as f64 / total as f64 } else { 0.0 };

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
    pub fn increment(&self, env: Env, key: String, delta: i64, ttl_ms: Option<f64>) -> Result<JsUnknown> {
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
                        4 => {
                            if payload.len() == 8 {
                                current_val = i64::from_ne_bytes(payload.try_into().unwrap());
                            } else {
                                return (is_hit, Err(napi::Error::new(napi::Status::InvalidArg, "Counter value is corrupted")));
                            }
                        }
                        3 => {
                            let parsed = std::str::from_utf8(payload).ok().and_then(|s| s.parse::<i64>().ok());
                            match parsed {
                                Some(val) => current_val = val,
                                None => return (is_hit, Err(napi::Error::new(napi::Status::InvalidArg, "Value is not a valid 64-bit integer"))),
                            }
                        }
                        _ => return (is_hit, Err(napi::Error::new(napi::Status::InvalidArg, "Value is not a numeric counter"))),
                    }
                }
            }

            let new_val = current_val.wrapping_add(delta);
            let mut new_bytes = Vec::with_capacity(9);
            new_bytes.push(4); // Tag 4: i64
            new_bytes.extend_from_slice(&new_val.to_ne_bytes());
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
    pub fn decrement(&self, env: Env, key: String, delta: i64, ttl_ms: Option<f64>) -> Result<JsUnknown> {
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
            let deleted = self.with_shard(self.shard_for(hash), |cache| cache.delete(hash, &key))?;
            self.l1_gens.bump(&key);
            if deleted {
                count += 1;
            }
        }
        if count > 0 {
            self.atomic_deletes.fetch_add(count as u64, Ordering::Relaxed);
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
    use super::*;

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
}
