use std::collections::HashMap;
use napi::{Result, Error, Status};
use super::cache::{Cache, CacheConfig};

#[napi]
pub struct CacheManager {
    caches: parking_lot::RwLock<HashMap<String, Cache>>,
}

#[napi]
impl CacheManager {
    #[napi(constructor)]
    pub fn new() -> Self {
        Self {
            caches: parking_lot::RwLock::new(HashMap::new()),
        }
    }

    #[napi(catch_unwind)]
    pub fn create_cache(&self, name: String, config: CacheConfig) -> Result<Cache> {
        let mut lock = self.caches.write();
        if lock.contains_key(&name) {
            return Err(Error::new(
                Status::InvalidArg,
                format!("Cache '{}' already exists", name),
            ));
        }
        let cache = Cache::from_config(config);
        lock.insert(name, cache.clone());
        Ok(cache)
    }

    #[napi(catch_unwind)]
    pub fn get_cache(&self, name: String) -> Option<Cache> {
        let lock = self.caches.read();
        lock.get(&name).cloned()
    }

    #[napi(catch_unwind)]
    pub fn delete_cache(&self, name: String) -> bool {
        let mut lock = self.caches.write();
        lock.remove(&name).is_some()
    }

    #[napi(catch_unwind)]
    pub fn clear(&self) {
        let mut lock = self.caches.write();
        lock.clear();
    }
}
