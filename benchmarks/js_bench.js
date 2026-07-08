const { Bench } = require('tinybench');
const { LRUCache } = require('lru-cache');
const { CacheManager } = require('../index.js');

async function runBenchmark() {
  const manager = new CacheManager();
  const capacity = 10000;

  // Instantiate caches
  const lruCacheL1 = manager.createCache('bench-lru-l1', { policy: 'lru', capacity, l1Capacity: capacity });
  const lruCacheL2 = manager.createCache('bench-lru-l2', { policy: 'lru', capacity, l1Capacity: 0 });
  const arcCache = manager.createCache('bench-arc', { policy: 'arc', capacity, l1Capacity: 0 });
  const tinyLfuCache = manager.createCache('bench-tinylfu', { policy: 'tinylfu', capacity, l1Capacity: 0 });
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
    .add('OffHeap L1+L2 - Set', () => {
      lruCacheL1.set(keys[writeIdx % keys.length], val);
      writeIdx++;
    })
    .add('OffHeap L2 Only - Set', () => {
      lruCacheL2.set(keys[writeIdx % keys.length], val);
      writeIdx++;
    });

  console.log('=== Benchmarking SET operations (10k capacity, 20k unique keys) ===');
  await benchSet.run();
  console.table(benchSet.table());

  // Warm up caches for GET benchmarks
  for (let i = 0; i < capacity; i++) {
    jsMap.set(keys[i], val);
    jsLru.set(keys[i], val);
    lruCacheL1.set(keys[i], val);
    lruCacheL2.set(keys[i], val);
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
    .add('OffHeap L1 Hit - Get', () => {
      lruCacheL1.get(keys[readIdx % capacity]);
      readIdx++;
    })
    .add('OffHeap L2 Hit (LRU) - Get', () => {
      lruCacheL2.get(keys[readIdx % capacity]);
      readIdx++;
    })
    .add('OffHeap L2 Hit (ARC) - Get', () => {
      arcCache.get(keys[readIdx % capacity]);
      readIdx++;
    })
    .add('OffHeap L2 Hit (W-TinyLFU) - Get', () => {
      tinyLfuCache.get(keys[readIdx % capacity]);
      readIdx++;
    });

  console.log('\n=== Benchmarking GET operations (100% Cache Hits) ===');
  await benchGet.run();
  console.table(benchGet.table());

  lruCacheL1.dispose();
  lruCacheL2.dispose();
  arcCache.dispose();
  tinyLfuCache.dispose();
  manager.dispose();
}

runBenchmark().catch(console.error);
