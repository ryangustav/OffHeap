use std::sync::Arc;
use napi::{Env, JsUnknown, JsBuffer, JsString, Result, ValueType};
use super::algorithms::{CacheImpl, CacheStats, lru::LruCache, arc::ArcCache, tinylfu::TinyLfuCache};

#[napi(object)]
pub struct CacheConfig {
    pub policy: String,
    pub capacity: u32,
}

#[napi(object)]
pub struct CacheStatsJs {
    pub hits: f64,
    pub misses: f64,
    pub capacity: f64,
    pub size: f64,
}

#[napi]
pub struct Cache {
    inner: Arc<parking_lot::Mutex<Box<dyn CacheImpl>>>,
}

impl Clone for Cache {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl Cache {
    pub fn from_config(config: CacheConfig) -> Self {
        let capacity = config.capacity as usize;
        let policy_lower = config.policy.to_lowercase();
        let inner: Box<dyn CacheImpl> = match policy_lower.as_str() {
            "arc" => Box::new(ArcCache::new(capacity)),
            "tinylfu" | "w-tinylfu" => Box::new(TinyLfuCache::new(capacity)),
            _ => Box::new(LruCache::new(capacity)),
        };
        Self {
            inner: Arc::new(parking_lot::Mutex::new(inner)),
        }
    }
}

#[napi]
impl Cache {
    #[napi]
    pub fn get(&self, env: Env, key: String) -> Result<JsUnknown> {
        let mut lock = self.inner.lock();
        if let Some(bytes) = lock.get(&key) {
            if bytes.is_empty() {
                return Ok(env.get_undefined()?.into_unknown());
            }
            let tag = bytes[0];
            let payload = &bytes[1..];
            match tag {
                1 => {
                    // Buffer
                    let js_buf = env.create_buffer_copy(payload)?;
                    Ok(js_buf.into_unknown())
                }
                2 => {
                    // String
                    let s = std::str::from_utf8(payload)
                        .map_err(|e| napi::Error::new(napi::Status::StringExpected, e.to_string()))?;
                    let js_str = env.create_string(s)?;
                    Ok(js_str.into_unknown())
                }
                3 => {
                    // JSON
                    let json_val: serde_json::Value = serde_json::from_slice(payload)
                        .map_err(|e| napi::Error::new(napi::Status::InvalidArg, e.to_string()))?;
                    let parsed = env.to_js_value(&json_val)?;
                    Ok(parsed)
                }
                _ => Err(napi::Error::new(napi::Status::InvalidArg, "Invalid data type tag in cache storage")),
            }
        } else {
            Ok(env.get_undefined()?.into_unknown())
        }
    }

    #[napi]
    pub fn set(&self, env: Env, key: String, value: JsUnknown, ttl_ms: Option<f64>) -> Result<JsUnknown> {
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
            bytes.push(2); // Tag: String
            bytes.extend_from_slice(utf8.as_slice());
        } else {
            // Treat as JSON
            let json_val: serde_json::Value = env.from_js_value(value)?;
            let serialized = serde_json::to_vec(&json_val)
                .map_err(|e| napi::Error::new(napi::Status::InvalidArg, e.to_string()))?;
            bytes.push(3); // Tag: JSON
            bytes.extend_from_slice(&serialized);
        }

        let ttl = ttl_ms.map(|ms| ms as u64);
        let mut lock = self.inner.lock();
        let old_bytes = lock.set(&key, bytes, ttl);

        if let Some(bytes) = old_bytes {
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
                    let json_val: serde_json::Value = serde_json::from_slice(payload)
                        .map_err(|e| napi::Error::new(napi::Status::InvalidArg, e.to_string()))?;
                    let parsed = env.to_js_value(&json_val)?;
                    Ok(parsed)
                }
                _ => Err(napi::Error::new(napi::Status::InvalidArg, "Invalid data type tag in cache storage")),
            }
        } else {
            Ok(env.get_undefined()?.into_unknown())
        }
    }

    #[napi]
    pub fn delete(&self, key: String) -> bool {
        let mut lock = self.inner.lock();
        lock.delete(&key)
    }

    #[napi]
    pub fn clear(&self) {
        let mut lock = self.inner.lock();
        lock.clear();
    }

    #[napi]
    pub fn stats(&self) -> CacheStatsJs {
        let lock = self.inner.lock();
        let stats = lock.stats();
        CacheStatsJs {
            hits: stats.hits as f64,
            misses: stats.misses as f64,
            capacity: stats.capacity as f64,
            size: stats.size as f64,
        }
    }

    #[napi]
    pub fn keys(&self) -> Vec<String> {
        let lock = self.inner.lock();
        lock.keys()
    }
}
