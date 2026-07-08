const { LRUCache } = require('lru-cache');
const { CacheManager } = require('../index.js');

function formatMemory(bytes) {
  return (bytes / 1024 / 1024).toFixed(2) + ' MB';
}

// Zipfian distribution generator (theta = 0.99)
function createZipfGenerator(numKeys, theta = 0.99) {
  let sum = 0;
  const pdf = new Float64Array(numKeys);
  for (let i = 1; i <= numKeys; i++) {
    pdf[i - 1] = 1.0 / Math.pow(i, theta);
    sum += pdf[i - 1];
  }
  for (let i = 0; i < numKeys; i++) {
    pdf[i] /= sum;
  }
  const cdf = new Float64Array(numKeys);
  let acc = 0;
  for (let i = 0; i < numKeys; i++) {
    acc += pdf[i];
    cdf[i] = acc;
  }
  return () => {
    const r = Math.random();
    let lo = 0, hi = numKeys - 1;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (cdf[mid] >= r) {
        hi = mid;
      } else {
        lo = mid + 1;
      }
    }
    return lo;
  };
}

// Generates a fully unique payload on the fly (no reference sharing)
function getRealisticPayload() {
  const randStr = Math.random().toString(36).repeat(5); // ~55 unique characters
  return {
    id: Math.random(),
    name: 'Product Record ' + Math.random(),
    tags: [randStr.substring(0, 10), 'fast', 'cache'],
    attributes: {
      sku: 'SKU-' + Math.floor(Math.random() * 1000000),
      price: Math.random() * 100
    },
    description: randStr.repeat(15) // ~1 KB unique string payload
  };
}

async function runZipfBenchmark() {
  console.log('Generating Zipfian distribution generator (200k keys)...');
  const keyUniverseSize = 200000;
  const getIndex = createZipfGenerator(keyUniverseSize, 0.99);

  const keys = Array.from({ length: keyUniverseSize }, (_, i) => `key-${i}`);
  const opsCount = 1000000; // 1M total ops
  const cacheCapacity = 100000; // 100k entries capacity

  // Pre-generate operations queue (90% GET, 10% SET)
  console.log('Pre-generating 1,000,000 operations queue...');
  const ops = new Uint8Array(opsCount);
  const targetKeyIdxs = new Int32Array(opsCount);
  for (let i = 0; i < opsCount; i++) {
    ops[i] = Math.random() < 0.9 ? 0 : 1; // 0 = GET, 1 = SET
    targetKeyIdxs[i] = getIndex();
  }

  // 1. Measure lru-cache (Pure JS)
  global.gc && global.gc();
  const startRssLru = process.memoryUsage().rss;
  const startHeapLru = process.memoryUsage().heapUsed;
  const jsLru = new LRUCache({ max: cacheCapacity });

  console.log('\nRunning Pure JS lru-cache (1,000,000 operations)...');
  const startLru = Date.now();
  const lruLatencies = new Float64Array(opsCount);

  for (let i = 0; i < opsCount; i++) {
    const key = keys[targetKeyIdxs[i]];
    const opStart = performance.now();
    
    if (ops[i] === 0) {
      const val = jsLru.get(key);
    } else {
      jsLru.set(key, getRealisticPayload());
    }
    
    lruLatencies[i] = performance.now() - opStart;
  }
  const lruDuration = Date.now() - startLru;
  const endRssLru = process.memoryUsage().rss;
  const endHeapLru = process.memoryUsage().heapUsed;

  jsLru.clear();
  global.gc && global.gc();

  // 2. Measure OffHeap (L1 + L2 Hybrid)
  global.gc && global.gc();
  const startRssOffheap = process.memoryUsage().rss;
  const startHeapOffheap = process.memoryUsage().heapUsed;
  
  const manager = new CacheManager();
  const offheap = manager.createCache('zipf-hybrid', {
    policy: 'lru',
    capacity: cacheCapacity,
    shards: 16,
    l1Capacity: 10000 
  });

  console.log('Running OffHeap Hybrid L1+L2 (1,000,000 operations)...');
  const startOffheap = Date.now();
  const offheapLatencies = new Float64Array(opsCount);

  for (let i = 0; i < opsCount; i++) {
    const key = keys[targetKeyIdxs[i]];
    const opStart = performance.now();
    
    if (ops[i] === 0) {
      const val = offheap.get(key);
    } else {
      offheap.set(key, getRealisticPayload());
    }
    
    offheapLatencies[i] = performance.now() - opStart;
  }
  const offheapDuration = Date.now() - startOffheap;
  const endRssOffheap = process.memoryUsage().rss;
  const endHeapOffheap = process.memoryUsage().heapUsed;

  offheap.dispose();
  manager.dispose();
  global.gc && global.gc();

  // Sort latencies for percentiles
  lruLatencies.sort();
  offheapLatencies.sort();

  const getPercentile = (arr, pct) => {
    const idx = Math.floor(arr.length * (pct / 100));
    return (arr[idx] * 1000000).toFixed(0) + ' ns'; // Convert ms to ns
  };

  const getAverage = (arr) => {
    let sum = 0;
    for (let i = 0; i < arr.length; i++) {
      sum += arr[i];
    }
    return ((sum / arr.length) * 1000000).toFixed(0) + ' ns';
  };

  console.log('\n========================================================================');
  console.log('🏆 1,000,000 ZIPFIAN REALISTIC WORKLOAD WORKBENCH (90% GET, 10% SET, 1 KB UNIQUE)');
  console.log('========================================================================');
  console.log('| Metric           | JS lru-cache (Baseline) | OffHeap Hybrid (L1+L2)   |');
  console.log('|------------------|-------------------------|--------------------------|');
  console.log(`| Total Time       | ${lruDuration} ms               | ${offheapDuration} ms                 |`);
  console.log(`| Average Latency  | ${getAverage(lruLatencies).padEnd(23)} | ${getAverage(offheapLatencies).padEnd(24)} |`);
  console.log(`| p50 (Median)     | ${getPercentile(lruLatencies, 50).padEnd(23)} | ${getPercentile(offheapLatencies, 50).padEnd(24)} |`);
  console.log(`| p90              | ${getPercentile(lruLatencies, 90).padEnd(23)} | ${getPercentile(offheapLatencies, 90).padEnd(24)} |`);
  console.log(`| p99 (GC Sweeps)  | ${getPercentile(lruLatencies, 99).padEnd(23)} | ${getPercentile(offheapLatencies, 99).padEnd(24)} |`);
  console.log(`| V8 Heap Bloat    | ${formatMemory(endHeapLru - startHeapLru).padEnd(23)} | ${formatMemory(endHeapOffheap - startHeapOffheap).padEnd(24)} |`);
  console.log(`| RSS Memory Delta | ${formatMemory(endRssLru - startRssLru).padEnd(23)} | ${formatMemory(endRssOffheap - startRssOffheap).padEnd(24)} |`);
  console.log('========================================================================');

  console.log('\n========================= Footprint Graph (RSS Memory Delta) =========================');
  const maxRss = Math.max(endRssLru - startRssLru, endRssOffheap - startRssOffheap);
  const getBar = (bytes) => {
    if (bytes <= 0) return '(0 MB)';
    const pct = bytes / maxRss;
    const len = Math.round(pct * 40);
    return '█'.repeat(len) + ` (${formatMemory(bytes)})`;
  };
  console.log(`JS lru-cache : ${getBar(endRssLru - startRssLru)}`);
  console.log(`OffHeap L1/L2: ${getBar(endRssOffheap - startRssOffheap)}`);
}

runZipfBenchmark().catch(console.error);
