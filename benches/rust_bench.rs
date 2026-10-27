use bytes::Bytes;
use criterion::{criterion_group, criterion_main, Criterion};
use offheap::algorithms::{arc::ArcCache, lru::LruCache, tinylfu::TinyLfuCache, CacheImpl};
use std::hash::BuildHasher;

fn h(key: &str) -> u64 {
    foldhash::fast::FixedState::with_seed(0).hash_one(key)
}

fn payload() -> Bytes {
    Bytes::from_static(&[1, 2, 3])
}

fn bench_sets(c: &mut Criterion) {
    let mut group = c.benchmark_group("Cache Set");
    let capacity = 10000;

    group.bench_function("LRU Set", |b| {
        let mut cache = LruCache::new(capacity);
        let mut i = 0;
        b.iter(|| {
            let key = format!("key-{}", i);
            cache.set(h(&key), &key, payload(), None);
            i += 1;
        });
    });

    group.bench_function("ARC Set", |b| {
        let mut cache = ArcCache::new(capacity);
        let mut i = 0;
        b.iter(|| {
            let key = format!("key-{}", i);
            cache.set(h(&key), &key, payload(), None);
            i += 1;
        });
    });

    group.bench_function("W-TinyLFU Set", |b| {
        let mut cache = TinyLfuCache::new(capacity);
        let mut i = 0;
        b.iter(|| {
            let key = format!("key-{}", i);
            cache.set(h(&key), &key, payload(), None);
            i += 1;
        });
    });

    group.finish();
}

fn bench_gets(c: &mut Criterion) {
    let mut group = c.benchmark_group("Cache Get");
    let capacity = 10000;

    // LRU Warm up
    let mut lru = LruCache::new(capacity);
    for i in 0..capacity {
        {
            let key = format!("key-{}", i);
            lru.set(h(&key), &key, payload(), None);
        };
    }

    group.bench_function("LRU Get Hit", |b| {
        let mut i = 0;
        b.iter(|| {
            let key = format!("key-{}", i % capacity);
            lru.get(h(&key), &key);
            i += 1;
        });
    });

    // ARC Warm up
    let mut arc = ArcCache::new(capacity);
    for i in 0..capacity {
        {
            let key = format!("key-{}", i);
            arc.set(h(&key), &key, payload(), None);
        };
    }

    group.bench_function("ARC Get Hit", |b| {
        let mut i = 0;
        b.iter(|| {
            let key = format!("key-{}", i % capacity);
            arc.get(h(&key), &key);
            i += 1;
        });
    });

    // TinyLFU Warm up
    let mut tinylfu = TinyLfuCache::new(capacity);
    for i in 0..capacity {
        {
            let key = format!("key-{}", i);
            tinylfu.set(h(&key), &key, payload(), None);
        };
    }

    group.bench_function("W-TinyLFU Get Hit", |b| {
        let mut i = 0;
        b.iter(|| {
            let key = format!("key-{}", i % capacity);
            tinylfu.get(h(&key), &key);
            i += 1;
        });
    });

    group.finish();
}

criterion_group!(benches, bench_sets, bench_gets);
criterion_main!(benches);
