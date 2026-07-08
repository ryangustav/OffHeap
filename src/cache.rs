use std::sync::Arc;
use napi::{Env, JsUnknown, JsBuffer, JsString, JsObject, Result, ValueType};
use super::algorithms::{CacheImpl, lru::LruCache, arc::ArcCache, tinylfu::TinyLfuCache};

#[napi(object)]
pub struct CacheConfig {
    pub policy: String,
    pub capacity: u32,
    pub shards: Option<u32>,
    pub max_bytes: Option<f64>,
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
}

impl Clone for Cache {
    fn clone(&self) -> Self {
        Self {
            shards: self.shards.clone(),
        }
    }
}

impl Cache {
    pub fn from_config(config: CacheConfig) -> Self {
        let capacity = config.capacity as usize;
        let shards_count = config.shards.unwrap_or(8).max(1) as usize;
        let policy_lower = config.policy.to_lowercase();
        
        let shard_capacity = (capacity / shards_count).max(1);
        let shard_max_bytes = config.max_bytes.map(|mb| (mb as usize / shards_count).max(1));

        let mut shards = Vec::with_capacity(shards_count);
        for _ in 0..shards_count {
            let inner: Box<dyn CacheImpl> = match policy_lower.as_str() {
                "arc" => Box::new(ArcCache::new_with_max_bytes(shard_capacity, shard_max_bytes)),
                "tinylfu" | "w-tinylfu" => Box::new(TinyLfuCache::new_with_max_bytes(shard_capacity, shard_max_bytes)),
                _ => Box::new(LruCache::new_with_max_bytes(shard_capacity, shard_max_bytes)),
            };
            shards.push(Arc::new(parking_lot::Mutex::new(Some(inner))));
        }

        Self { shards }
    }

    fn get_shard(&self, key: &str) -> Result<Arc<parking_lot::Mutex<Option<Box<dyn CacheImpl>>>>> {
        let hash = seahash::hash(key.as_bytes()) as usize;
        let idx = hash % self.shards.len();
        Ok(Arc::clone(&self.shards[idx]))
    }

    fn serialize_value(&self, _env: Env, value: JsUnknown) -> Result<Vec<u8>> {
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
                bytes.push(3); // Tag: JSON
                bytes.extend_from_slice(&slice[2..]);
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
                bytes.push(3); // Tag: JSON (float represented as string)
                bytes.extend_from_slice(s.as_bytes());
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
                // Prepend \0J prefix so the JS wrapper knows to JSON.parse it
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
            _ => Err(napi::Error::new(napi::Status::InvalidArg, "Invalid data type tag in cache storage")),
        }
    }
}

#[napi]
impl Cache {
    #[napi]
    pub fn get(&self, env: Env, key: String) -> Result<JsUnknown> {
        let shard_lock = self.get_shard(&key)?;
        let mut lock = shard_lock.lock();
        let cache = lock.as_mut().ok_or_else(|| napi::Error::new(napi::Status::GenericFailure, "Cache has been disposed"))?;
        
        if let Some(bytes) = cache.get(&key) {
            self.deserialize_value(env, bytes)
        } else {
            Ok(env.get_undefined()?.into_unknown())
        }
    }

    #[napi]
    pub fn peek(&self, env: Env, key: String) -> Result<JsUnknown> {
        let shard_lock = self.get_shard(&key)?;
        let mut lock = shard_lock.lock();
        let cache = lock.as_mut().ok_or_else(|| napi::Error::new(napi::Status::GenericFailure, "Cache has been disposed"))?;
        
        if let Some(bytes) = cache.peek(&key) {
            self.deserialize_value(env, bytes)
        } else {
            Ok(env.get_undefined()?.into_unknown())
        }
    }

    #[napi]
    pub fn has(&self, key: String) -> Result<bool> {
        let shard_lock = self.get_shard(&key)?;
        let mut lock = shard_lock.lock();
        let cache = lock.as_mut().ok_or_else(|| napi::Error::new(napi::Status::GenericFailure, "Cache has been disposed"))?;
        Ok(cache.has(&key))
    }

    #[napi]
    pub fn set(&self, env: Env, key: String, value: JsUnknown, ttl_ms: Option<f64>) -> Result<()> {
        let bytes = self.serialize_value(env, value)?;
        let ttl = ttl_ms.map(|ms| ms as u64);
        
        let shard_lock = self.get_shard(&key)?;
        let mut lock = shard_lock.lock();
        let cache = lock.as_mut().ok_or_else(|| napi::Error::new(napi::Status::GenericFailure, "Cache has been disposed"))?;
        
        cache.set(&key, bytes, ttl);
        Ok(())
    }

    #[napi]
    pub fn touch(&self, key: String, ttl_ms: Option<f64>) -> Result<bool> {
        let ttl = ttl_ms.map(|ms| ms as u64);
        let shard_lock = self.get_shard(&key)?;
        let mut lock = shard_lock.lock();
        let cache = lock.as_mut().ok_or_else(|| napi::Error::new(napi::Status::GenericFailure, "Cache has been disposed"))?;
        Ok(cache.touch(&key, ttl))
    }

    #[napi]
    pub fn delete(&self, key: String) -> Result<bool> {
        let shard_lock = self.get_shard(&key)?;
        let mut lock = shard_lock.lock();
        let cache = lock.as_mut().ok_or_else(|| napi::Error::new(napi::Status::GenericFailure, "Cache has been disposed"))?;
        Ok(cache.delete(&key))
    }

    #[napi]
    pub fn clear(&self) -> Result<()> {
        for shard_lock in &self.shards {
            let mut lock = shard_lock.lock();
            let cache = lock.as_mut().ok_or_else(|| napi::Error::new(napi::Status::GenericFailure, "Cache has been disposed"))?;
            cache.clear();
        }
        Ok(())
    }

    #[napi]
    pub fn stats(&self) -> Result<CacheStatsJs> {
        let mut total_hits = 0.0;
        let mut total_misses = 0.0;
        let mut total_capacity = 0.0;
        let mut total_size = 0.0;
        let mut total_bytes = 0.0;
        
        for shard_lock in &self.shards {
            let lock = shard_lock.lock();
            let cache = lock.as_ref().ok_or_else(|| napi::Error::new(napi::Status::GenericFailure, "Cache has been disposed"))?;
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

    #[napi]
    pub fn keys(&self) -> Result<Vec<String>> {
        let mut all_keys = Vec::new();
        for shard_lock in &self.shards {
            let lock = shard_lock.lock();
            let cache = lock.as_ref().ok_or_else(|| napi::Error::new(napi::Status::GenericFailure, "Cache has been disposed"))?;
            all_keys.extend(cache.keys());
        }
        Ok(all_keys)
    }

    #[napi]
    pub fn dispose(&self) -> Result<()> {
        for shard_lock in &self.shards {
            let mut lock = shard_lock.lock();
            *lock = None;
        }
        Ok(())
    }

    #[napi]
    pub fn increment(&self, env: Env, key: String, delta: i64, ttl_ms: Option<f64>) -> Result<JsUnknown> {
        let shard_lock = self.get_shard(&key)?;
        let mut lock = shard_lock.lock();
        let cache = lock.as_mut().ok_or_else(|| napi::Error::new(napi::Status::GenericFailure, "Cache has been disposed"))?;
        
        let mut current_val = 0i64;
        
        if let Some(bytes) = cache.get(&key) {
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
        cache.set(&key, new_bytes, ttl);
        
        let js_num = env.create_double(new_val as f64)?;
        Ok(js_num.into_unknown())
    }

    #[napi]
    pub fn decrement(&self, env: Env, key: String, delta: i64, ttl_ms: Option<f64>) -> Result<JsUnknown> {
        self.increment(env, key, -delta, ttl_ms)
    }

    #[napi]
    pub fn mget(&self, env: Env, keys: Vec<String>) -> Result<Vec<JsUnknown>> {
        let mut result = Vec::with_capacity(keys.len());
        for key in keys {
            let shard_lock = self.get_shard(&key)?;
            let mut lock = shard_lock.lock();
            let cache = lock.as_mut().ok_or_else(|| napi::Error::new(napi::Status::GenericFailure, "Cache has been disposed"))?;
            
            if let Some(bytes) = cache.get(&key) {
                let val = self.deserialize_value(env, bytes)?;
                result.push(val);
            } else {
                result.push(env.get_undefined()?.into_unknown());
            }
        }
        Ok(result)
    }

    #[napi]
    pub fn mset(&self, env: Env, entries: JsObject, ttl_ms: Option<f64>) -> Result<()> {
        let keys_array = entries.get_property_names()?;
        let len = keys_array.get_array_length()?;
        let ttl = ttl_ms.map(|ms| ms as u64);
        
        for i in 0..len {
            let key_unknown: JsUnknown = keys_array.get_element(i)?;
            let key_str = key_unknown.coerce_to_string()?;
            let key = key_str.into_utf8()?.into_owned()?;
            
            let val: JsUnknown = entries.get_named_property(&key)?;
            let bytes = self.serialize_value(env, val)?;
            
            let shard_lock = self.get_shard(&key)?;
            let mut lock = shard_lock.lock();
            let cache = lock.as_mut().ok_or_else(|| napi::Error::new(napi::Status::GenericFailure, "Cache has been disposed"))?;
            cache.set(&key, bytes, ttl);
        }
        Ok(())
    }

    #[napi]
    pub fn mdelete(&self, keys: Vec<String>) -> Result<u32> {
        let mut count = 0;
        for key in keys {
            let shard_lock = self.get_shard(&key)?;
            let mut lock = shard_lock.lock();
            let cache = lock.as_mut().ok_or_else(|| napi::Error::new(napi::Status::GenericFailure, "Cache has been disposed"))?;
            if cache.delete(&key) {
                count += 1;
            }
        }
        Ok(count)
    }
}
