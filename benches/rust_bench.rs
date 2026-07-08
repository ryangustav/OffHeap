use criterion::{criterion_group, criterion_main, Criterion};
use offheap::algorithms::{
    CacheImpl,
    lru::LruCache,
    arc::ArcCache,
    tinylfu::TinyLfuCache,
};

fn bench_sets(c: &mut Criterion) {
    let mut group = c.benchmark_group("Cache Set");
    let capacity = 10000;
    
    group.bench_function("LRU Set", |b| {
        let mut cache = LruCache::new(capacity);
        let mut i = 0;
        b.iter(|| {
            let key = format!("key-{}", i);
            cache.set(&key, vec![1, 2, 3], None);
            i += 1;
        });
    });

    group.bench_function("ARC Set", |b| {
        let mut cache = ArcCache::new(capacity);
        let mut i = 0;
        b.iter(|| {
            let key = format!("key-{}", i);
            cache.set(&key, vec![1, 2, 3], None);
            i += 1;
        });
    });

    group.bench_function("W-TinyLFU Set", |b| {
        let mut cache = TinyLfuCache::new(capacity);
        let mut i = 0;
        b.iter(|| {
            let key = format!("key-{}", i);
            cache.set(&key, vec![1, 2, 3], None);
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
        lru.set(&format!("key-{}", i), vec![1, 2, 3], None);
    }
    
    group.bench_function("LRU Get Hit", |b| {
        let mut i = 0;
        b.iter(|| {
            lru.get(&format!("key-{}", i % capacity));
            i += 1;
        });
    });

    // ARC Warm up
    let mut arc = ArcCache::new(capacity);
    for i in 0..capacity {
        arc.set(&format!("key-{}", i), vec![1, 2, 3], None);
    }

    group.bench_function("ARC Get Hit", |b| {
        let mut i = 0;
        b.iter(|| {
            arc.get(&format!("key-{}", i % capacity));
            i += 1;
        });
    });

    // TinyLFU Warm up
    let mut tinylfu = TinyLfuCache::new(capacity);
    for i in 0..capacity {
        tinylfu.set(&format!("key-{}", i), vec![1, 2, 3], None);
    }

    group.bench_function("W-TinyLFU Get Hit", |b| {
        let mut i = 0;
        b.iter(|| {
            tinylfu.get(&format!("key-{}", i % capacity));
            i += 1;
        });
    });

    group.finish();
}

criterion_group!(benches, bench_sets, bench_gets);
criterion_main!(benches);
