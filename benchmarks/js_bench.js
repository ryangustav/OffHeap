const { Bench } = require('tinybench');
const { LRUCache } = require('lru-cache');
const { CacheManager } = require('../index.js');

async function runBenchmark() {
  const manager = new CacheManager();
  const capacity = 10000;

  // Instantiate caches
  const lruCache = manager.createCache('bench-lru', { policy: 'lru', capacity });
  const arcCache = manager.createCache('bench-arc', { policy: 'arc', capacity });
  const tinyLfuCache = manager.createCache('bench-tinylfu', { policy: 'tinylfu', capacity });
  const jsLru = new LRUCache({ max: capacity });
  const jsMap = new Map();

  // Prepare benchmark keys and values
  const keys = Array.from({ length: 20000 }, (_, i) => `key-${i}`);
  const val = { data: 'a'.repeat(100) }; // 100 char string object
  let writeIdx = 0;
  let readIdx = 0;

  const benchSet = new Bench({ time: 1000 });
  
  benchSet
    .add('Pure JS Map - Set', () => {
      jsMap.set(keys[writeIdx % keys.length], val);
      writeIdx++;
    })
    .add('JS lru-cache - Set', () => {
      jsLru.set(keys[writeIdx % keys.length], val);
      writeIdx++;
    })
    .add('OffHeap LRU - Set', () => {
      lruCache.set(keys[writeIdx % keys.length], val);
      writeIdx++;
    })
    .add('OffHeap ARC - Set', () => {
      arcCache.set(keys[writeIdx % keys.length], val);
      writeIdx++;
    })
    .add('OffHeap W-TinyLFU - Set', () => {
      tinyLfuCache.set(keys[writeIdx % keys.length], val);
      writeIdx++;
    });

  console.log('=== Benchmarking SET operations (10k capacity, 20k unique keys) ===');
  await benchSet.run();
  console.table(benchSet.table());

  // Warm up caches for GET benchmarks
  for (let i = 0; i < capacity; i++) {
    jsMap.set(keys[i], val);
    jsLru.set(keys[i], val);
    lruCache.set(keys[i], val);
    arcCache.set(keys[i], val);
    tinyLfuCache.set(keys[i], val);
  }

  const benchGet = new Bench({ time: 1000 });

  benchGet
    .add('Pure JS Map - Get', () => {
      jsMap.get(keys[readIdx % capacity]);
      readIdx++;
    })
    .add('JS lru-cache - Get', () => {
      jsLru.get(keys[readIdx % capacity]);
      readIdx++;
    })
    .add('OffHeap LRU - Get', () => {
      lruCache.get(keys[readIdx % capacity]);
      readIdx++;
    })
    .add('OffHeap ARC - Get', () => {
      arcCache.get(keys[readIdx % capacity]);
      readIdx++;
    })
    .add('OffHeap W-TinyLFU - Get', () => {
      tinyLfuCache.get(keys[readIdx % capacity]);
      readIdx++;
    });

  console.log('\n=== Benchmarking GET operations (100% Cache Hits) ===');
  await benchGet.run();
  console.table(benchGet.table());

  lruCache.dispose();
  arcCache.dispose();
  tinyLfuCache.dispose();
  manager.dispose();
}

runBenchmark().catch(console.error);
