const { Bench } = require('tinybench');
const { LRUCache } = require('lru-cache');
const { execSync } = require('child_process');
const { CacheManager } = require('../index.js');

function formatMemory(bytes) {
  return (bytes / 1024 / 1024).toFixed(2) + ' MB';
}

async function runMicroBenchmarks() {
  const manager = new CacheManager();
  const capacity = 10000;

  const lruCacheL1 = manager.createCache('bench-lru-l1', { policy: 'lru', capacity, l1Capacity: capacity });
  const lruCacheL2 = manager.createCache('bench-lru-l2', { policy: 'lru', capacity, l1Capacity: 0 });
  const arcCache = manager.createCache('bench-arc', { policy: 'arc', capacity, l1Capacity: 0 });
  const tinyLfuCache = manager.createCache('bench-tinylfu', { policy: 'tinylfu', capacity, l1Capacity: 0 });
  const jsLru = new LRUCache({ max: capacity });
  const jsMap = new Map();

  const keys = Array.from({ length: 20000 }, (_, i) => `key-${i}`);
  const val = { data: 'a'.repeat(100) };
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

  console.log('=== SECTION 1: Micro-Benchmarking SET operations (10k capacity, 20k keys) ===');
  await benchSet.run();
  console.table(benchSet.table());

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

  console.log('\n=== SECTION 2: Micro-Benchmarking GET operations (100% Cache Hits) ===');
  await benchGet.run();
  console.table(benchGet.table());

  lruCacheL1.dispose();
  lruCacheL2.dispose();
  arcCache.dispose();
  tinyLfuCache.dispose();
  manager.dispose();
}

async function runBatchBenchmark() {
  const manager = new CacheManager();
  const capacity = 100000;
  const jsLru = new LRUCache({ max: capacity });
  const offheap = manager.createCache('bench-batch', { policy: 'lru', capacity, l1Capacity: 0 });

  const numKeys = 20000;
  const keys = Array.from({ length: numKeys }, (_, i) => `key-${i}`);
  const val = { val: 'test-data-payload' };

  for (let i = 0; i < numKeys; i++) {
    jsLru.set(keys[i], val);
    offheap.set(keys[i], val);
  }

  console.log('\n=== SECTION 3: BATCH OPERATION THROUGHPUT (Statistical Tinybench Suite) ===');

  // 1. Batch Size 100
  const bench100 = new Bench({ time: 1000 });
  const keys100 = keys.slice(0, 100);
  bench100
    .add('JS lru-cache - Batch 100 (Loop)', () => {
      for (let i = 0; i < 100; i++) jsLru.get(keys100[i]);
    })
    .add('OffHeap L2 - Batch 100 (Loop)', () => {
      // Calls wrapper.get() to ensure JSON.parse is executed for fair comparison
      for (let i = 0; i < 100; i++) offheap.get(keys100[i]);
    })
    .add('OffHeap mget - Batch 100 (Single FFI)', () => {
      offheap.mget(keys100);
    });

  console.log('\n--- Batch size: 100 keys ---');
  await bench100.run();
  console.table(bench100.table());

  // 2. Batch Size 1000
  const bench1000 = new Bench({ time: 1000 });
  const keys1000 = keys.slice(0, 1000);
  bench1000
    .add('JS lru-cache - Batch 1000 (Loop)', () => {
      for (let i = 0; i < 1000; i++) jsLru.get(keys1000[i]);
    })
    .add('OffHeap L2 - Batch 1000 (Loop)', () => {
      for (let i = 0; i < 1000; i++) offheap.get(keys1000[i]);
    })
    .add('OffHeap mget - Batch 1000 (Single FFI)', () => {
      offheap.mget(keys1000);
    });

  console.log('\n--- Batch size: 1000 keys ---');
  await bench1000.run();
  console.table(bench1000.table());

  offheap.dispose();
  manager.dispose();
}

async function runGCPressureBenchmark() {
  console.log('\n=== SECTION 4: GARBAGE COLLECTION LATENCY PRESSURE TEST (500k Keys, 1M Ops) ===');
  if (!global.gc) {
    console.warn('⚠️ Warning: Run with --expose-gc flag to include V8 GC latency metrics. Skipping GC pressure test.');
    return;
  }

  console.log('Spawning isolated child processes to run GC pressure tests...');
  
  console.log('Running JS lru-cache test in isolated process...');
  const lruRes = JSON.parse(execSync('node --expose-gc benchmarks/gc_pressure_child.js lru').toString().trim());
  
  console.log('Running OffHeap Hybrid test in isolated process...');
  const offheapRes = JSON.parse(execSync('node --expose-gc benchmarks/gc_pressure_child.js offheap').toString().trim());

  console.log('\n========================================================================');
  console.log('🏆 GARBAGE COLLECTION LATENCY PRESSURE TEST RESULTS (Process Isolated)');
  console.log('========================================================================');
  console.log('| Metric              | JS lru-cache (In-Heap) | OffHeap Hybrid (L1+L2)   |');
  console.log('|---------------------|------------------------|--------------------------|');
  console.log(`| Total Time          | ${lruRes.duration} ms               | ${offheapRes.duration} ms                 |`);
  console.log(`| Average Cache Lat   | ${lruRes.avgLat} μs                 | ${offheapRes.avgLat} μs                   |`);
  console.log(`| p50 Cache Latency   | ${lruRes.p50} μs                 | ${offheapRes.p50} μs                   |`);
  console.log(`| p90 Cache Latency   | ${lruRes.p90} μs                 | ${offheapRes.p90} μs                   |`);
  console.log(`| p99 Cache Latency   | ${lruRes.p99} μs                 | ${offheapRes.p99} μs                   |`);
  console.log(`| p99.9 Cache Latency | ${lruRes.p99_9} μs                 | ${offheapRes.p99_9} μs                   |`);
  console.log('|---------------------|------------------------|--------------------------|');
  console.log(`| V8 GC Events Trigger| ${lruRes.gcCount.toString().padEnd(22)} | ${offheapRes.gcCount.toString().padEnd(24)} |`);
  console.log(`| Total V8 GC Duration| ${parseFloat(lruRes.totalGcTime).toFixed(1).padEnd(19)} ms | ${parseFloat(offheapRes.totalGcTime).toFixed(1).padEnd(21)} ms |`);
  console.log(`| Worst Single GC Stop| ${parseFloat(lruRes.maxGcTime).toFixed(1).padEnd(19)} ms | ${parseFloat(offheapRes.maxGcTime).toFixed(1).padEnd(21)} ms |`);
  console.log(`| Heap Usage (End)    | ${lruRes.endHeap.padEnd(22)} | ${offheapRes.endHeap.padEnd(24)} |`);
  console.log(`| RSS Memory (Start)  | ${lruRes.startRss.padEnd(22)} | ${offheapRes.startRss.padEnd(24)} |`);
  console.log(`| RSS Memory (End)    | ${lruRes.endRss.padEnd(22)} | ${offheapRes.endRss.padEnd(24)} |`);
  console.log(`| RSS Memory Delta    | ${lruRes.rssDelta.padEnd(22)} | ${offheapRes.rssDelta.padEnd(24)} |`);
  console.log('========================================================================');
}

async function main() {
  await runMicroBenchmarks();
  await runBatchBenchmark();
  await runGCPressureBenchmark();
}

main().catch(console.error);
