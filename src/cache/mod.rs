pub mod types;
pub mod serde;

use std::sync::Arc;
use std::hash::{Hash, Hasher, BuildHasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use napi::{Env, JsUnknown, JsString, Result};
use parking_lot::Mutex;

use crate::algorithms::{CacheImpl, lru::LruCache, arc::ArcCache, tinylfu::TinyLfuCache};
use crate::algorithms::Eviction;

pub use types::*;
use serde::{serialize_value, deserialize_value, TAG_I64};
#[cfg(test)]
use serde::{TAG_BUFFER, TAG_STRING, TAG_JSON, TAG_LZ4_JSON};

impl Cache {
    pub fn from_config(config: CacheConfig) -> Self {
        let capacity = config.capacity as usize;
        let shards_count = config.shards.unwrap_or(8).max(1) as usize;
        let policy_lower = config.policy.to_lowercase();
        let compression = config.compression.unwrap_or(false);
        
        let shard_capacity = (capacity / shards_count).max(1);
        let max_bytes = config.max_bytes.map(|mb| mb as usize);
        let shard_max_bytes = max_bytes.map(|mb| (mb / shards_count).max(1));

        let mut shards = Vec::with_capacity(shards_count);
        for _ in 0..shards_count {
            let inner: Box<dyn CacheImpl> = match policy_lower.as_str() {
                "arc" => Box::new(ArcCache::new_with_max_bytes(shard_capacity, shard_max_bytes)),
                "tinylfu" | "w-tinylfu" => Box::new(TinyLfuCache::new_with_max_bytes(shard_capacity, shard_max_bytes)),
                _ => Box::new(LruCache::new_with_max_bytes(shard_capacity, shard_max_bytes)),
            };
            shards.push(Arc::new(Mutex::new(Some(inner))));
        }

        let hasher = Arc::new(std::collections::hash_map::RandomState::new());

        Self {
            shards,
            compression,
            hasher,
            max_bytes,
            atomic_hits: Arc::new(AtomicU64::new(0)),
            atomic_misses: Arc::new(AtomicU64::new(0)),
            atomic_sets: Arc::new(AtomicU64::new(0)),
            atomic_deletes: Arc::new(AtomicU64::new(0)),
            atomic_evictions: Arc::new(AtomicU64::new(0)),
            atomic_expirations: Arc::new(AtomicU64::new(0)),
            created_at: Arc::new(Instant::now()),
        }
    }

    fn get_shard(&self, key: &str) -> Result<Arc<Mutex<Option<Box<dyn CacheImpl>>>>> {
        let mut hasher = self.hasher.build_hasher();
        key.hash(&mut hasher);
        let hash = hasher.finish() as usize;
        let idx = hash % self.shards.len();
        Ok(Arc::clone(&self.shards[idx]))
    }

    fn make_expired_obj(&self, env: Env, ev: Eviction) -> Result<napi::JsObject> {
        let mut obj = env.create_object()?;
        obj.set_named_property("__expired", env.get_boolean(true)?)?;
        obj.set_named_property("key", env.create_string(&ev.key)?)?;
        let val_js = deserialize_value(self.max_bytes, env, ev.value)?;
        obj.set_named_property("value", val_js)?;
        Ok(obj)
    }

    fn with_shard<T, F>(&self, key: &str, f: F) -> Result<T>
    where
        F: FnOnce(&mut dyn CacheImpl) -> Result<T>,
    {
        let shard = self.get_shard(key)?;
        let mut lock = shard.lock();
        let mut cache = lock.take().ok_or_else(|| napi::Error::new(napi::Status::GenericFailure, "Cache has been disposed or poisoned"))?;
        let result = f(cache.as_mut());
        *lock = Some(cache);
        result
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
        validate_key(&key)?;
        let (bytes, eviction) = self.with_shard(&key, |cache| Ok(cache.get(&key)))?;
        if let Some(b) = bytes {
            self.atomic_hits.fetch_add(1, Ordering::Relaxed);
            deserialize_value(self.max_bytes, env, b)
        } else if let Some(ev) = eviction {
            self.atomic_misses.fetch_add(1, Ordering::Relaxed);
            self.atomic_expirations.fetch_add(1, Ordering::Relaxed);
            Ok(self.make_expired_obj(env, ev)?.into_unknown())
        } else {
            self.atomic_misses.fetch_add(1, Ordering::Relaxed);
            Ok(env.get_undefined()?.into_unknown())
        }
    }

    #[napi(catch_unwind)]
    pub fn peek(&self, env: Env, key: String) -> Result<JsUnknown> {
        validate_key(&key)?;
        let (bytes, eviction) = self.with_shard(&key, |cache| Ok(cache.peek(&key)))?;
        if let Some(b) = bytes {
            self.atomic_hits.fetch_add(1, Ordering::Relaxed);
            deserialize_value(self.max_bytes, env, b)
        } else if let Some(ev) = eviction {
            self.atomic_misses.fetch_add(1, Ordering::Relaxed);
            self.atomic_expirations.fetch_add(1, Ordering::Relaxed);
            Ok(self.make_expired_obj(env, ev)?.into_unknown())
        } else {
            self.atomic_misses.fetch_add(1, Ordering::Relaxed);
            Ok(env.get_undefined()?.into_unknown())
        }
    }

    #[napi(catch_unwind)]
    pub fn has(&self, env: Env, key: String) -> Result<JsUnknown> {
        validate_key(&key)?;
        let (has_key, eviction) = self.with_shard(&key, |cache| Ok(cache.has(&key)))?;
        if has_key {
            self.atomic_hits.fetch_add(1, Ordering::Relaxed);
            Ok(env.get_boolean(true)?.into_unknown())
        } else if let Some(ev) = eviction {
            self.atomic_misses.fetch_add(1, Ordering::Relaxed);
            self.atomic_expirations.fetch_add(1, Ordering::Relaxed);
            Ok(self.make_expired_obj(env, ev)?.into_unknown())
        } else {
            self.atomic_misses.fetch_add(1, Ordering::Relaxed);
            Ok(env.get_boolean(false)?.into_unknown())
        }
    }

    #[napi(catch_unwind)]
    pub fn set(&self, env: Env, key: String, value: JsUnknown, ttl_ms: Option<f64>, force_compression: Option<bool>) -> Result<JsUnknown> {
        validate_key(&key)?;
        let bytes = serialize_value(self.compression, env, value, force_compression)?;
        let ttl = ttl_ms.map(|ms| ms as u64);
        
        let shard_lock = self.get_shard(&key)?;
        let mut lock = shard_lock.lock();
        let mut cache = lock.take().ok_or_else(|| napi::Error::new(napi::Status::GenericFailure, "Cache has been disposed or poisoned"))?;
        let (old_val, evs) = cache.set(&key, bytes, ttl);
        *lock = Some(cache);
        
        self.atomic_sets.fetch_add(1, Ordering::Relaxed);
        
        let mut replaced_count = 0;
        let mut evicted_count = 0;
        let mut expired_count = 0;
        
        if old_val.is_some() {
            replaced_count += 1;
        }
        if let Some(ref evs_list) = evs {
            for ev in evs_list {
                if ev.reason == "expired" {
                    expired_count += 1;
                } else {
                    evicted_count += 1;
                }
            }
        }
        
        if replaced_count > 0 {
            self.atomic_evictions.fetch_add(replaced_count, Ordering::Relaxed);
        }
        if evicted_count > 0 {
            self.atomic_evictions.fetch_add(evicted_count, Ordering::Relaxed);
        }
        if expired_count > 0 {
            self.atomic_expirations.fetch_add(expired_count, Ordering::Relaxed);
        }
        
        if old_val.is_none() && evs.is_none() {
            return Ok(env.get_undefined()?.into_unknown());
        }

        let mut all_evictions = Vec::new();
        if let Some(old) = old_val {
            let mut ev_obj = env.create_object()?;
            ev_obj.set_named_property("key", env.create_string(&key)?)?;
            let val_js = deserialize_value(self.max_bytes, env, old)?;
            ev_obj.set_named_property("value", val_js)?;
            ev_obj.set_named_property("reason", env.create_string("replaced")?)?;
            all_evictions.push(ev_obj.into_unknown());
        }
        
        if let Some(evs_list) = evs {
            for ev in evs_list {
                let mut ev_obj = env.create_object()?;
                ev_obj.set_named_property("key", env.create_string(&ev.key)?)?;
                let val_js = deserialize_value(self.max_bytes, env, ev.value)?;
                ev_obj.set_named_property("value", val_js)?;
                ev_obj.set_named_property("reason", env.create_string(&ev.reason)?)?;
                all_evictions.push(ev_obj.into_unknown());
            }
        }
        
        if all_evictions.is_empty() {
            Ok(env.get_undefined()?.into_unknown())
        } else {
            let mut js_arr = env.create_array_with_length(all_evictions.len())?;
            for (i, ev_js) in all_evictions.into_iter().enumerate() {
                js_arr.set_element(i as u32, ev_js)?;
            }
            Ok(js_arr.into_unknown())
        }
    }

    #[napi(catch_unwind)]
    pub fn touch(&self, env: Env, key: String, ttl_ms: Option<f64>) -> Result<JsUnknown> {
        validate_key(&key)?;
        let ttl = ttl_ms.map(|ms| ms as u64);
        let (res, eviction) = self.with_shard(&key, |cache| Ok(cache.touch(&key, ttl)))?;
        if res {
            self.atomic_hits.fetch_add(1, Ordering::Relaxed);
            Ok(env.get_boolean(true)?.into_unknown())
        } else if let Some(ev) = eviction {
            self.atomic_misses.fetch_add(1, Ordering::Relaxed);
            self.atomic_expirations.fetch_add(1, Ordering::Relaxed);
            Ok(self.make_expired_obj(env, ev)?.into_unknown())
        } else {
            self.atomic_misses.fetch_add(1, Ordering::Relaxed);
            Ok(env.get_boolean(false)?.into_unknown())
        }
    }

    #[napi(catch_unwind)]
    pub fn delete(&self, key: String) -> Result<bool> {
        validate_key(&key)?;
        let res = self.with_shard(&key, |cache| Ok(cache.delete(&key)))?;
        if res {
            self.atomic_deletes.fetch_add(1, Ordering::Relaxed);
        }
        Ok(res)
    }

    #[napi(catch_unwind)]
    pub fn clear(&self) -> Result<()> {
        for shard in &self.shards {
            let mut lock = shard.lock();
            if let Some(ref mut cache) = *lock {
                cache.clear();
            }
        }
        Ok(())
    }

    #[napi(catch_unwind)]
    pub fn dispose(&self) -> Result<()> {
        for shard in &self.shards {
            let mut lock = shard.lock();
            // take() removes the CacheImpl from the shard, poisoning it for future use.
            let _ = lock.take();
        }
        Ok(())
    }

    #[napi(catch_unwind)]
    pub fn stats(&self, _env: Env) -> Result<CacheStatsJs> {
        let mut total_size = 0.0;
        let mut total_bytes = 0.0;
        let mut total_capacity = 0.0;
        let mut shard_stats = Vec::with_capacity(self.shards.len());

        for shard in &self.shards {
            let mut lock = shard.lock();
            if let Some(ref mut cache) = *lock {
                let st = cache.stats();
                total_size += st.size as f64;
                total_bytes += st.bytes_used as f64;
                total_capacity += st.capacity as f64;
                shard_stats.push(ShardStatsJs {
                    size: st.size as f64,
                    bytes_used: st.bytes_used as f64,
                });
            } else {
                shard_stats.push(ShardStatsJs { size: 0.0, bytes_used: 0.0 });
            }
        }

        let hits = self.atomic_hits.load(Ordering::Relaxed) as f64;
        let misses = self.atomic_misses.load(Ordering::Relaxed) as f64;
        let sets = self.atomic_sets.load(Ordering::Relaxed) as f64;
        let deletes = self.atomic_deletes.load(Ordering::Relaxed) as f64;
        let evictions = self.atomic_evictions.load(Ordering::Relaxed) as f64;
        let expirations = self.atomic_expirations.load(Ordering::Relaxed) as f64;

        let total_requests = hits + misses;
        let hit_rate = if total_requests > 0.0 {
            hits / total_requests
        } else {
            0.0
        };
        
        let uptime_ms = self.created_at.elapsed().as_millis() as f64;

        Ok(CacheStatsJs {
            hits,
            misses,
            capacity: total_capacity,
            size: total_size,
            bytes_used: total_bytes,
            sets,
            deletes,
            evictions,
            expirations,
            hit_rate,
            uptime_ms,
            shards: shard_stats,
        })
    }

    #[napi(catch_unwind)]
    pub fn counters(&self) -> Result<CacheCountersJs> {
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
        for shard in &self.shards {
            let mut lock = shard.lock();
            if let Some(ref mut cache) = *lock {
                all_keys.extend(cache.keys());
            }
        }
        Ok(all_keys)
    }

    #[napi(catch_unwind)]
    pub fn increment(&self, env: Env, key: String, delta: Option<f64>, ttl_ms: Option<f64>) -> Result<JsUnknown> {
        validate_key(&key)?;
        let d = delta.unwrap_or(1.0) as i64;
        let ttl = ttl_ms.map(|ms| ms as u64);
        
        let shard_lock = self.get_shard(&key)?;
        let mut lock = shard_lock.lock();
        let mut cache = lock.take().ok_or_else(|| napi::Error::new(napi::Status::GenericFailure, "Cache has been disposed or poisoned"))?;
        
        let (bytes, ev_opt) = cache.get(&key);
        let current_val: i64;
        
        if let Some(payload) = bytes {
            if payload.is_empty() {
                current_val = 0;
            } else if payload[0] == TAG_I64 && payload.len() == 9 {
                let bytes: [u8; 8] = payload[1..].try_into()
                    .map_err(|_| napi::Error::new(napi::Status::InvalidArg, "Counter value is corrupted"))?;
                current_val = i64::from_le_bytes(bytes);
            } else {
                *lock = Some(cache);
                return Err(napi::Error::new(napi::Status::InvalidArg, "Cannot increment a non-integer value"));
            }
        } else {
            current_val = 0;
        }
        
        let new_val = current_val.wrapping_add(d);
        let mut new_bytes = Vec::with_capacity(9);
        new_bytes.push(TAG_I64);
        new_bytes.extend_from_slice(&new_val.to_le_bytes());
        
        let (_, _evs) = cache.set(&key, new_bytes, ttl);
        *lock = Some(cache);
        
        self.atomic_sets.fetch_add(1, Ordering::Relaxed);
        
        Ok(env.create_double(new_val as f64)?.into_unknown())
    }

    #[napi(catch_unwind)]
    pub fn decrement(&self, env: Env, key: String, delta: Option<f64>, ttl_ms: Option<f64>) -> Result<JsUnknown> {
        validate_key(&key)?;
        let d = delta.unwrap_or(1.0) as i64;
        let ttl = ttl_ms.map(|ms| ms as u64);
        
        let shard_lock = self.get_shard(&key)?;
        let mut lock = shard_lock.lock();
        let mut cache = lock.take().ok_or_else(|| napi::Error::new(napi::Status::GenericFailure, "Cache has been disposed or poisoned"))?;
        
        let (bytes, _ev_opt) = cache.get(&key);
        let current_val: i64;
        
        if let Some(payload) = bytes {
            if payload.is_empty() {
                current_val = 0;
            } else if payload[0] == TAG_I64 && payload.len() == 9 {
                let bytes: [u8; 8] = payload[1..].try_into()
                    .map_err(|_| napi::Error::new(napi::Status::InvalidArg, "Counter value is corrupted"))?;
                current_val = i64::from_le_bytes(bytes);
            } else {
                *lock = Some(cache);
                return Err(napi::Error::new(napi::Status::InvalidArg, "Cannot decrement a non-integer value"));
            }
        } else {
            current_val = 0;
        }
        
        let new_val = current_val.wrapping_sub(d);
        let mut new_bytes = Vec::with_capacity(9);
        new_bytes.push(TAG_I64);
        new_bytes.extend_from_slice(&new_val.to_le_bytes());
        
        let (_, _evs) = cache.set(&key, new_bytes, ttl);
        *lock = Some(cache);
        
        self.atomic_sets.fetch_add(1, Ordering::Relaxed);
        
        Ok(env.create_double(new_val as f64)?.into_unknown())
    }

    #[napi(catch_unwind)]
    pub fn mget(&self, env: Env, keys: Vec<String>) -> Result<Vec<JsUnknown>> {
        let mut result = Vec::with_capacity(keys.len());
        let mut hits = 0;
        let mut misses = 0;
        let mut expirations = 0;
        
        for key in keys {
            validate_key(&key)?;
            let (bytes, eviction) = self.with_shard(&key, |cache| Ok(cache.get(&key)))?;
            
            let val_res = if let Some(b) = bytes {
                hits += 1;
                deserialize_value(self.max_bytes, env, b)?
            } else if let Some(ev) = eviction {
                misses += 1;
                expirations += 1;
                self.make_expired_obj(env, ev)?.into_unknown()
            } else {
                misses += 1;
                env.get_undefined()?.into_unknown()
            };
            result.push(val_res);
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

    #[napi(catch_unwind)]
    pub fn mset(&self, env: Env, entries: napi::JsObject, ttl_ms: Option<f64>) -> Result<JsUnknown> {
        let keys = napi::JsObject::get_property_names(&entries)?;
        let len = keys.get_array_length()?;
        let mut all_evictions = Vec::new();
        
        for i in 0..len {
            let key_unknown: JsUnknown = keys.get_element(i)?;
            let key_str = key_unknown.coerce_to_string()?;
            let key = key_str.into_utf8()?.into_owned()?;
            validate_key(&key)?;
            
            let val_js: JsUnknown = entries.get_named_property(&key)?;
            let bytes = serialize_value(self.compression, env, val_js, None)?;
            let ttl = ttl_ms.map(|ms| ms as u64);
            
            let shard_lock = self.get_shard(&key)?;
            let mut lock = shard_lock.lock();
            let mut cache = lock.take().ok_or_else(|| napi::Error::new(napi::Status::GenericFailure, "Cache has been disposed or poisoned"))?;
            
            let (old_val, evs) = cache.set(&key, bytes, ttl);
            *lock = Some(cache);
            self.atomic_sets.fetch_add(1, Ordering::Relaxed);
            
            if let Some(old) = old_val {
                self.atomic_evictions.fetch_add(1, Ordering::Relaxed);
                let mut ev_obj = env.create_object()?;
                ev_obj.set_named_property("key", env.create_string(&key)?)?;
                let v = deserialize_value(self.max_bytes, env, old)?;
                ev_obj.set_named_property("value", v)?;
                ev_obj.set_named_property("reason", env.create_string("replaced")?)?;
                all_evictions.push(ev_obj.into_unknown());
            }
            if let Some(evs_list) = evs {
                for ev in evs_list {
                    if ev.reason == "expired" {
                        self.atomic_expirations.fetch_add(1, Ordering::Relaxed);
                    } else {
                        self.atomic_evictions.fetch_add(1, Ordering::Relaxed);
                    }
                    let mut ev_obj = env.create_object()?;
                    ev_obj.set_named_property("key", env.create_string(&ev.key)?)?;
                    let v = deserialize_value(self.max_bytes, env, ev.value)?;
                    ev_obj.set_named_property("value", v)?;
                    ev_obj.set_named_property("reason", env.create_string(&ev.reason)?)?;
                    all_evictions.push(ev_obj.into_unknown());
                }
            }
        }
        
        if all_evictions.is_empty() {
            Ok(env.get_undefined()?.into_unknown())
        } else {
            let mut js_arr = env.create_array_with_length(all_evictions.len())?;
            for (i, ev_js) in all_evictions.into_iter().enumerate() {
                js_arr.set_element(i as u32, ev_js)?;
            }
            Ok(js_arr.into_unknown())
        }
    }

    #[napi(catch_unwind)]
    pub fn mdelete(&self, keys: Vec<String>) -> Result<u32> {
        let mut count = 0;
        for key in keys {
            validate_key(&key)?;
            let shard_lock = self.get_shard(&key)?;
            let mut lock = shard_lock.lock();
            let mut cache = lock.take().ok_or_else(|| napi::Error::new(napi::Status::GenericFailure, "Cache has been disposed or poisoned"))?;
            let deleted = cache.delete(&key);
            *lock = Some(cache);
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
        deserialize_value(self.max_bytes, env, bytes)
    }

    #[napi(catch_unwind)]
    pub fn test_panic(&self, key: String) {
        let shard_lock = self.get_shard(&key).unwrap();
        let mut lock = shard_lock.lock();
        let mut _cache = lock.take().ok_or_else(|| napi::Error::new(napi::Status::GenericFailure, "Cache has been disposed or poisoned")).unwrap();
        panic!("Intended test panic in Rust code!");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Unit tests
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use super::super::algorithms::lru::LruCache;

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
        assert!(err.reason.contains("8192"), "Error message must mention the limit");
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
        cache.set("key1", value.clone(), None);
        let (result, eviction) = cache.get("key1");
        assert!(eviction.is_none());
        assert_eq!(result.unwrap(), value);
    }

    #[test]
    fn lru_evicts_lru_on_capacity_exceeded() {
        let mut cache = LruCache::new(2);
        cache.set("a", encode_string("v_a"), None);
        cache.set("b", encode_string("v_b"), None);
        cache.get("a");
        cache.set("c", encode_string("v_c"), None);
        let (b_val, _) = cache.get("b");
        assert!(b_val.is_none(), "b should have been evicted");
        let (a_val, _) = cache.get("a");
        assert!(a_val.is_some(), "a should still be in cache");
    }

    #[test]
    fn lru_delete_removes_key() {
        let mut cache = LruCache::new(5);
        cache.set("key", encode_i64(100), None);
        assert!(cache.delete("key"));
        assert!(!cache.delete("key"));
        let (val, _) = cache.get("key");
        assert!(val.is_none());
    }

    #[test]
    fn lru_ttl_expiry_returns_eviction() {
        let mut cache = LruCache::new(5);
        cache.set("temp", encode_string("value"), Some(1));
        std::thread::sleep(std::time::Duration::from_millis(5));
        let (val, eviction) = cache.get("temp");
        assert!(val.is_none(), "expired key must not return a value");
        assert!(eviction.is_some(), "expired key must return an eviction record");
        let ev = eviction.unwrap();
        assert_eq!(ev.key, "temp");
        assert_eq!(ev.reason, "expired");
    }

    #[test]
    fn lru_peek_does_not_promote_key() {
        let mut cache = LruCache::new(2);
        cache.set("a", encode_string("v_a"), None);
        cache.set("b", encode_string("v_b"), None);
        let (peeked, _) = cache.peek("a");
        assert!(peeked.is_some());
        cache.set("c", encode_string("v_c"), None);
        let (a_val, _) = cache.get("a");
        assert!(a_val.is_none(), "peek must not prevent eviction of a");
    }

    #[test]
    fn lru_clear_empties_cache() {
        let mut cache = LruCache::new(5);
        for i in 0..5 {
            cache.set(&format!("k{i}"), encode_i64(i as i64), None);
        }
        assert_eq!(cache.stats().size, 5);
        cache.clear();
        assert_eq!(cache.stats().size, 0);
        let (val, _) = cache.get("k0");
        assert!(val.is_none());
    }

    #[test]
    fn lru_max_bytes_triggers_eviction() {
        let mut cache = LruCache::new_with_max_bytes(100, Some(5));
        cache.set("a", encode_string("v"), None);
        cache.set("b", encode_string("v"), None);
        cache.set("c", encode_string("v"), None);
        let stats = cache.stats();
        assert!(stats.bytes_used <= 5, "bytes_used {} must not exceed maxBytes 5", stats.bytes_used);
    }

    // ── Counter increment/decrement encoding ──────────────────────────────────

    #[test]
    fn i64_counter_encoding_survives_lru_roundtrip() {
        let mut cache = LruCache::new(5);
        let initial: i64 = 0;
        let delta: i64 = 7;
        let new_val = initial + delta;

        let encoded = encode_i64(new_val);
        cache.set("counter", encoded, None);

        let (bytes, _) = cache.get("counter");
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
