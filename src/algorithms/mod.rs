use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub capacity: usize,
    pub size: usize,
    pub bytes_used: usize,
}

#[derive(Debug, Clone)]
pub struct Eviction {
    pub key: String,
    pub value: Vec<u8>,
    pub reason: String, // "evicted", "expired"
}

#[derive(Debug, Clone)]
pub struct CacheEntry {
    pub value: Vec<u8>,
    pub expires_at: Option<Instant>,
}

impl CacheEntry {
    pub fn new(value: Vec<u8>, ttl_ms: Option<u64>) -> Self {
        let expires_at = ttl_ms.map(|ms| Instant::now() + Duration::from_millis(ms));
        Self { value, expires_at }
    }

    pub fn is_expired(&self) -> bool {
        if let Some(expiry) = self.expires_at {
            Instant::now() > expiry
        } else {
            false
        }
    }
}

pub trait CacheImpl: Send + Sync {
    fn get(&mut self, key: &str) -> (Option<Vec<u8>>, Option<Eviction>);
    fn peek(&mut self, key: &str) -> (Option<Vec<u8>>, Option<Eviction>);
    fn has(&mut self, key: &str) -> (bool, Option<Eviction>);
    fn set(&mut self, key: &str, value: Vec<u8>, ttl_ms: Option<u64>) -> (Option<Vec<u8>>, Vec<Eviction>);
    fn touch(&mut self, key: &str, ttl_ms: Option<u64>) -> (bool, Option<Eviction>);
    fn delete(&mut self, key: &str) -> bool;
    fn clear(&mut self);
    fn stats(&self) -> CacheStats;
    fn keys(&self) -> Vec<String>;
}

pub mod lru;
pub mod arc;
pub mod tinylfu;
