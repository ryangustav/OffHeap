use std::sync::Arc;
use std::collections::hash_map::RandomState;
use std::hash::{Hash, Hasher, BuildHasher};
use napi::{Env, JsUnknown, JsBuffer, JsString, JsObject, Result, ValueType};
use super::algorithms::{CacheImpl, lru::LruCache, arc::ArcCache, tinylfu::TinyLfuCache};

#[napi(object)]
pub struct CacheConfig {
    pub policy: String,
    pub capacity: u32,
    pub shards: Option<u32>,
    pub max_bytes: Option<f64>,
    pub compression: Option<bool>,
}

#[napi(object)]
pub struct CacheStatsJs {
    pub hits: f64,
    pub misses: f64,
    pub capacity: f64,
    pub size: f64,
    pub bytes_used: f64,
}

#[napi]
pub struct Cache {
    shards: Vec<Arc<parking_lot::Mutex<Option<Box<dyn CacheImpl>>>>>,
    compression: bool,
    hasher: Arc<RandomState>,
    max_bytes: Option<usize>,
}

impl Clone for Cache {
    fn clone(&self) -> Self {
        Self {
            shards: self.shards.clone(),
            compression: self.compression,
            hasher: Arc::clone(&self.hasher),
            max_bytes: self.max_bytes,
        }
    }
}

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
            shards.push(Arc::new(parking_lot::Mutex::new(Some(inner))));
        }

        let hasher = Arc::new(RandomState::new());

        Self { shards, compression, hasher, max_bytes }
    }

    fn get_shard(&self, key: &str) -> Result<Arc<parking_lot::Mutex<Option<Box<dyn CacheImpl>>>>> {
        let mut hasher = self.hasher.build_hasher();
        key.hash(&mut hasher);
        let hash = hasher.finish() as usize;
        let idx = hash % self.shards.len();
        Ok(Arc::clone(&self.shards[idx]))
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
    /// When expanding types:
    ///   1. Register the tag code here following the range guidelines above.
    ///   2. Handle serialization tag routing in `serialize_value`.
    ///   3. Implement safety catches for new tags in `deserialize_value`.
    /// ========================================================================
    fn serialize_value(&self, _env: Env, value: JsUnknown, force_compression: Option<bool>) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        let value_type = value.get_type()?;

        if value.is_buffer()? {
            let buf = JsBuffer::try_from(value)?;
            let raw_bytes: Vec<u8> = buf.into_value()?.to_vec();
            bytes.push(1); // Tag: Buffer
            bytes.extend_from_slice(&raw_bytes);
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
                    bytes.push(3); // Tag: JSON (Raw)
                    bytes.extend_from_slice(&slice[2..]);
                }
            } else {
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
        Ok(bytes)
    }

    fn deserialize_value(&self, env: Env, bytes: Vec<u8>) -> Result<JsUnknown> {
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
                
                let mut decompressed = vec![0u8; uncompressed_size];
                lz4_flex::block::decompress_into(&payload[4..], &mut decompressed)
                    .map_err(|e| napi::Error::new(napi::Status::GenericFailure, format!("LZ4 Decompression failed: {}", e)))?;
                
                let s = std::str::from_utf8(&decompressed)
                    .map_err(|e| napi::Error::new(napi::Status::StringExpected, e.to_string()))?;
                let mut prefixed = String::with_capacity(2 + s.len());
                prefixed.push('\0');
                prefixed.push('J');
                prefixed.push_str(s);
                let js_str = env.create_string(&prefixed)?;
                Ok(js_str.into_unknown())
            }
            _ => Err(napi::Error::new(napi::Status::InvalidArg, "Invalid data type tag in cache storage")),
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
        validate_key(&key)?;
        let shard_lock = self.get_shard(&key)?;
        let mut lock = shard_lock.lock();
        let mut cache = lock.take().ok_or_else(|| napi::Error::new(napi::Status::GenericFailure, "Cache has been disposed or poisoned"))?;
        
        let (bytes, eviction) = cache.get(&key);
        let res = if let Some(b) = bytes {
            self.deserialize_value(env, b)
        } else if let Some(ev) = eviction {
            let mut obj = env.create_object()?;
            obj.set_named_property("__expired", env.get_boolean(true)?)?;
            obj.set_named_property("key", env.create_string(&ev.key)?)?;
            let val_js = self.deserialize_value(env, ev.value)?;
            obj.set_named_property("value", val_js)?;
            Ok(obj.into_unknown())
        } else {
            Ok(env.get_undefined()?.into_unknown())
        };
        *lock = Some(cache);
        res
    }

    #[napi(catch_unwind)]
    pub fn peek(&self, env: Env, key: String) -> Result<JsUnknown> {
        validate_key(&key)?;
        let shard_lock = self.get_shard(&key)?;
        let mut lock = shard_lock.lock();
        let mut cache = lock.take().ok_or_else(|| napi::Error::new(napi::Status::GenericFailure, "Cache has been disposed or poisoned"))?;
        
        let (bytes, eviction) = cache.peek(&key);
        let res = if let Some(b) = bytes {
            self.deserialize_value(env, b)
        } else if let Some(ev) = eviction {
            let mut obj = env.create_object()?;
            obj.set_named_property("__expired", env.get_boolean(true)?)?;
            obj.set_named_property("key", env.create_string(&ev.key)?)?;
            let val_js = self.deserialize_value(env, ev.value)?;
            obj.set_named_property("value", val_js)?;
            Ok(obj.into_unknown())
        } else {
            Ok(env.get_undefined()?.into_unknown())
        };
        *lock = Some(cache);
        res
    }

    #[napi(catch_unwind)]
    pub fn has(&self, env: Env, key: String) -> Result<JsUnknown> {
        validate_key(&key)?;
        let shard_lock = self.get_shard(&key)?;
        let mut lock = shard_lock.lock();
        let mut cache = lock.take().ok_or_else(|| napi::Error::new(napi::Status::GenericFailure, "Cache has been disposed or poisoned"))?;
        let (has_key, eviction) = cache.has(&key);
        *lock = Some(cache);
        
        if has_key {
            Ok(env.get_boolean(true)?.into_unknown())
        } else if let Some(ev) = eviction {
            let mut obj = env.create_object()?;
            obj.set_named_property("__expired", env.get_boolean(true)?)?;
            obj.set_named_property("key", env.create_string(&ev.key)?)?;
            let val_js = self.deserialize_value(env, ev.value)?;
            obj.set_named_property("value", val_js)?;
            Ok(obj.into_unknown())
        } else {
            Ok(env.get_boolean(false)?.into_unknown())
        }
    }

    #[napi(catch_unwind)]
    pub fn set(&self, env: Env, key: String, value: JsUnknown, ttl_ms: Option<f64>, force_compression: Option<bool>) -> Result<Vec<JsUnknown>> {
        validate_key(&key)?;
        let bytes = self.serialize_value(env, value, force_compression)?;
        let ttl = ttl_ms.map(|ms| ms as u64);
        
        let shard_lock = self.get_shard(&key)?;
        let mut lock = shard_lock.lock();
        let mut cache = lock.take().ok_or_else(|| napi::Error::new(napi::Status::GenericFailure, "Cache has been disposed or poisoned"))?;
        let (old_val, evs) = cache.set(&key, bytes, ttl);
        *lock = Some(cache);
        
        let mut all_evictions = Vec::new();
        if let Some(old) = old_val {
            let mut ev_obj = env.create_object()?;
            ev_obj.set_named_property("key", env.create_string(&key)?)?;
            let val_js = self.deserialize_value(env, old)?;
            ev_obj.set_named_property("value", val_js)?;
            ev_obj.set_named_property("reason", env.create_string("replaced")?)?;
            all_evictions.push(ev_obj.into_unknown());
        }
        for ev in evs {
            let mut ev_obj = env.create_object()?;
            ev_obj.set_named_property("key", env.create_string(&ev.key)?)?;
            let val_js = self.deserialize_value(env, ev.value)?;
            ev_obj.set_named_property("value", val_js)?;
            ev_obj.set_named_property("reason", env.create_string(&ev.reason)?)?;
            all_evictions.push(ev_obj.into_unknown());
        }
        Ok(all_evictions)
    }

    #[napi(catch_unwind)]
    pub fn touch(&self, env: Env, key: String, ttl_ms: Option<f64>) -> Result<JsUnknown> {
        validate_key(&key)?;
        let ttl = ttl_ms.map(|ms| ms as u64);
        let shard_lock = self.get_shard(&key)?;
        let mut lock = shard_lock.lock();
        let mut cache = lock.take().ok_or_else(|| napi::Error::new(napi::Status::GenericFailure, "Cache has been disposed or poisoned"))?;
        let (res, eviction) = cache.touch(&key, ttl);
        *lock = Some(cache);
        
        if res {
            Ok(env.get_boolean(true)?.into_unknown())
        } else if let Some(ev) = eviction {
            let mut obj = env.create_object()?;
            obj.set_named_property("__expired", env.get_boolean(true)?)?;
            obj.set_named_property("key", env.create_string(&ev.key)?)?;
            let val_js = self.deserialize_value(env, ev.value)?;
            obj.set_named_property("value", val_js)?;
            Ok(obj.into_unknown())
        } else {
            Ok(env.get_boolean(false)?.into_unknown())
        }
    }

    #[napi(catch_unwind)]
    pub fn delete(&self, key: String) -> Result<bool> {
        validate_key(&key)?;
        let shard_lock = self.get_shard(&key)?;
        let mut lock = shard_lock.lock();
        let mut cache = lock.take().ok_or_else(|| napi::Error::new(napi::Status::GenericFailure, "Cache has been disposed or poisoned"))?;
        let res = cache.delete(&key);
        *lock = Some(cache);
        Ok(res)
    }

    #[napi(catch_unwind)]
    pub fn clear(&self) -> Result<()> {
        for shard_lock in &self.shards {
            let mut lock = shard_lock.lock();
            let mut cache = lock.take().ok_or_else(|| napi::Error::new(napi::Status::GenericFailure, "Cache has been disposed or poisoned"))?;
            cache.clear();
            *lock = Some(cache);
        }
        Ok(())
    }

    #[napi(catch_unwind)]
    pub fn stats(&self) -> Result<CacheStatsJs> {
        let mut total_hits = 0.0;
        let mut total_misses = 0.0;
        let mut total_capacity = 0.0;
        let mut total_size = 0.0;
        let mut total_bytes = 0.0;
        
        for shard_lock in &self.shards {
            let lock = shard_lock.lock();
            let cache = lock.as_ref().ok_or_else(|| napi::Error::new(napi::Status::GenericFailure, "Cache has been disposed or poisoned"))?;
            let stats = cache.stats();
            total_hits += stats.hits as f64;
            total_misses += stats.misses as f64;
            total_capacity += stats.capacity as f64;
            total_size += stats.size as f64;
            total_bytes += stats.bytes_used as f64;
        }
        
        Ok(CacheStatsJs {
            hits: total_hits,
            misses: total_misses,
            capacity: total_capacity,
            size: total_size,
            bytes_used: total_bytes,
        })
    }

    #[napi(catch_unwind)]
    pub fn keys(&self) -> Result<Vec<String>> {
        let mut all_keys = Vec::new();
        for shard_lock in &self.shards {
            let lock = shard_lock.lock();
            let cache = lock.as_ref().ok_or_else(|| napi::Error::new(napi::Status::GenericFailure, "Cache has been disposed or poisoned"))?;
            all_keys.extend(cache.keys());
        }
        Ok(all_keys)
    }

    #[napi(catch_unwind)]
    pub fn dispose(&self) -> Result<()> {
        for shard_lock in &self.shards {
            let mut lock = shard_lock.lock();
            *lock = None;
        }
        Ok(())
    }

    #[napi(catch_unwind)]
    pub fn increment(&self, env: Env, key: String, delta: i64, ttl_ms: Option<f64>) -> Result<JsUnknown> {
        validate_key(&key)?;
        let shard_lock = self.get_shard(&key)?;
        let mut lock = shard_lock.lock();
        let mut cache = lock.take().ok_or_else(|| napi::Error::new(napi::Status::GenericFailure, "Cache has been disposed or poisoned"))?;
        
        let mut run = || -> Result<JsUnknown> {
            let mut current_val = 0i64;
            
            let (bytes_opt, _eviction) = cache.get(&key);
            if let Some(bytes) = bytes_opt {
                if !bytes.is_empty() {
                    let tag = bytes[0];
                    let payload = &bytes[1..];
                    match tag {
                        4 => {
                            if payload.len() == 8 {
                                current_val = i64::from_ne_bytes(payload.try_into().unwrap());
                            } else {
                                return Err(napi::Error::new(napi::Status::InvalidArg, "Counter value is corrupted"));
                            }
                        }
                        3 => {
                            let s = std::str::from_utf8(payload)
                                .map_err(|e| napi::Error::new(napi::Status::StringExpected, e.to_string()))?;
                            if let Ok(val) = s.parse::<i64>() {
                                current_val = val;
                            } else {
                                return Err(napi::Error::new(napi::Status::InvalidArg, "Value is not a valid 64-bit integer"));
                            }
                        }
                        _ => return Err(napi::Error::new(napi::Status::InvalidArg, "Value is not a numeric counter")),
                    }
                }
            }
            
            let new_val = current_val.wrapping_add(delta);
            let mut new_bytes = Vec::with_capacity(9);
            new_bytes.push(4); // Tag 4: i64
            new_bytes.extend_from_slice(&new_val.to_ne_bytes());
            
            let ttl = ttl_ms.map(|ms| ms as u64);
            let _ = cache.set(&key, new_bytes, ttl);
            
            let js_num = env.create_double(new_val as f64)?;
            Ok(js_num.into_unknown())
        };

        let res = run();
        *lock = Some(cache);
        res
    }

    #[napi(catch_unwind)]
    pub fn decrement(&self, env: Env, key: String, delta: i64, ttl_ms: Option<f64>) -> Result<JsUnknown> {
        self.increment(env, key, -delta, ttl_ms)
    }

    #[napi(catch_unwind)]
    pub fn mget(&self, env: Env, keys: Vec<String>) -> Result<Vec<JsUnknown>> {
        let mut result = Vec::with_capacity(keys.len());
        for key in keys {
            validate_key(&key)?;
            let shard_lock = self.get_shard(&key)?;
            let mut lock = shard_lock.lock();
            let mut cache = lock.take().ok_or_else(|| napi::Error::new(napi::Status::GenericFailure, "Cache has been disposed or poisoned"))?;
            
            let (bytes, eviction) = cache.get(&key);
            let val_res = if let Some(b) = bytes {
                self.deserialize_value(env, b)
            } else if let Some(ev) = eviction {
                let mut obj = env.create_object()?;
                obj.set_named_property("__expired", env.get_boolean(true)?)?;
                obj.set_named_property("key", env.create_string(&ev.key)?)?;
                let val_js = self.deserialize_value(env, ev.value)?;
                obj.set_named_property("value", val_js)?;
                Ok(obj.into_unknown())
            } else {
                Ok(env.get_undefined()?.into_unknown())
            };
            *lock = Some(cache);
            result.push(val_res?);
        }
        Ok(result)
    }

    #[napi(catch_unwind)]
    pub fn mset(&self, env: Env, entries: JsObject, ttl_ms: Option<f64>) -> Result<Vec<JsUnknown>> {
        let keys_array = entries.get_property_names()?;
        let len = keys_array.get_array_length()?;
        let ttl = ttl_ms.map(|ms| ms as u64);
        let mut all_evictions = Vec::new();
        
        for i in 0..len {
            let key_unknown: JsUnknown = keys_array.get_element(i)?;
            let key_str = key_unknown.coerce_to_string()?;
            let key = key_str.into_utf8()?.into_owned()?;
            validate_key(&key)?;
            
            let val: JsUnknown = entries.get_named_property(&key)?;
            let bytes = self.serialize_value(env, val, None)?;
            
            let shard_lock = self.get_shard(&key)?;
            let mut lock = shard_lock.lock();
            let mut cache = lock.take().ok_or_else(|| napi::Error::new(napi::Status::GenericFailure, "Cache has been disposed or poisoned"))?;
            let (old_val, evs) = cache.set(&key, bytes, ttl);
            *lock = Some(cache);
            
            if let Some(old) = old_val {
                let mut ev_obj = env.create_object()?;
                ev_obj.set_named_property("key", env.create_string(&key)?)?;
                let val_js = self.deserialize_value(env, old)?;
                ev_obj.set_named_property("value", val_js)?;
                ev_obj.set_named_property("reason", env.create_string("replaced")?)?;
                all_evictions.push(ev_obj.into_unknown());
            }
            for ev in evs {
                let mut ev_obj = env.create_object()?;
                ev_obj.set_named_property("key", env.create_string(&ev.key)?)?;
                let val_js = self.deserialize_value(env, ev.value)?;
                ev_obj.set_named_property("value", val_js)?;
                ev_obj.set_named_property("reason", env.create_string(&ev.reason)?)?;
                all_evictions.push(ev_obj.into_unknown());
            }
        }
        Ok(all_evictions)
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
        Ok(count)
    }

    #[napi(catch_unwind)]
    pub fn test_deserialize_raw(&self, env: Env, bytes: Vec<u8>) -> Result<JsUnknown> {
        self.deserialize_value(env, bytes)
    }

    #[napi(catch_unwind)]
    pub fn test_panic(&self, key: String) {
        let shard_lock = self.get_shard(&key).unwrap();
        let mut lock = shard_lock.lock();
        let mut _cache = lock.take().ok_or_else(|| napi::Error::new(napi::Status::GenericFailure, "Cache has been disposed or poisoned")).unwrap();
        panic!("Intended test panic in Rust code!");
    }
}
