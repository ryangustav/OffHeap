use std::collections::HashMap;
use std::sync::LazyLock;
use napi::{Result, Error, Status};
use super::cache::{Cache, CacheConfig};

static GLOBAL_CACHES: LazyLock<parking_lot::RwLock<HashMap<String, Cache>>> = LazyLock::new(|| {
    parking_lot::RwLock::new(HashMap::new())
});

#[napi]
pub struct CacheManager;

#[napi]
impl CacheManager {
    #[napi(constructor)]
    pub fn new() -> Self {
        Self
    }

    #[napi(catch_unwind)]
    pub fn create_cache(&self, name: String, config: CacheConfig) -> Result<Cache> {
        let mut lock = GLOBAL_CACHES.write();
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
        let lock = GLOBAL_CACHES.read();
        lock.get(&name).cloned()
    }

    #[napi(catch_unwind)]
    pub fn delete_cache(&self, name: String) -> bool {
        let mut lock = GLOBAL_CACHES.write();
        lock.remove(&name).is_some()
    }

    #[napi(catch_unwind)]
    pub fn clear(&self) {
        let mut lock = GLOBAL_CACHES.write();
        lock.clear();
    }
}
