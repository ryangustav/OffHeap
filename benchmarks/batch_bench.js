const { LRUCache } = require('lru-cache');
const { CacheManager } = require('../index.js');

async function runBatchBenchmark() {
  const manager = new CacheManager();
  const capacity = 100000;
  
  const jsLru = new LRUCache({ max: capacity });
  const offheap = manager.createCache('bench-batch', { policy: 'lru', capacity });

  const numKeys = 20000;
  const keys = Array.from({ length: numKeys }, (_, i) => `key-${i}`);
  const val = { val: 'test-data-payload' };

  console.log('Warming up caches with 20,000 entries...');
  for (let i = 0; i < numKeys; i++) {
    jsLru.set(keys[i], val);
    offheap.set(keys[i], val);
  }

  const batchSizes = [10, 100, 1000, 5000];
  const runs = 100; // Repeat to get stable duration

  console.log('\n========================================================================');
  console.log('📊 BATCH OPERATION THROUGHPUT WORKBENCH (mget vs Loop-based get)');
  console.log('========================================================================');
  console.log('| Batch Size | Loop JS lru-cache | Loop OffHeap (L2) | Batch OffHeap (mget) |');
  console.log('|------------|-------------------|-------------------|----------------------|');

  for (const size of batchSizes) {
    // Slice a batch of keys
    const batchKeys = keys.slice(0, size);

    // 1. Loop JS lru-cache
    const startLru = performance.now();
    for (let r = 0; r < runs; r++) {
      for (let i = 0; i < size; i++) {
        jsLru.get(batchKeys[i]);
      }
    }
    const durationLru = (performance.now() - startLru) / runs;

    // 2. Loop OffHeap L2
    const startOffheapLoop = performance.now();
    for (let r = 0; r < runs; r++) {
      for (let i = 0; i < size; i++) {
        // Disable L1 bypass for this test to measure L2 FFI overhead directly
        offheap._native.get(batchKeys[i]);
      }
    }
    const durationOffheapLoop = (performance.now() - startOffheapLoop) / runs;

    // 3. Batch OffHeap mget
    const startOffheapBatch = performance.now();
    for (let r = 0; r < runs; r++) {
      offheap.mget(batchKeys);
    }
    const durationOffheapBatch = (performance.now() - startOffheapBatch) / runs;

    const formatTime = (ms) => {
      if (ms < 1) {
        return (ms * 1000).toFixed(0) + ' μs';
      }
      return ms.toFixed(2) + ' ms';
    };

    console.log(
      `| ${size.toString().padEnd(10)} | ` +
      `${formatTime(durationLru).padEnd(17)} | ` +
      `${formatTime(durationOffheapLoop).padEnd(17)} | ` +
      `${formatTime(durationOffheapBatch).padEnd(20)} |`
    );
  }
  console.log('========================================================================');

  offheap.dispose();
  manager.dispose();
}

runBatchBenchmark().catch(console.error);
