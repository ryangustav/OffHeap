const { LRUCache } = require('lru-cache');
const { PerformanceObserver } = require('perf_hooks');
const { CacheManager } = require('../index.js');

const engine = process.argv[2]; // 'lru' or 'offheap'
if (!engine || (engine !== 'lru' && engine !== 'offheap')) {
  console.error('Usage: node gc_pressure_child.js [lru|offheap]');
  process.exit(1);
}

function formatMemory(bytes) {
  return (bytes / 1024 / 1024).toFixed(2) + ' MB';
}

function getLargePayload(id) {
  return {
    id,
    title: 'Product Details Model #' + id,
    price: Math.random() * 500,
    active: true,
    tags: ['electronics', 'retail', 'promo', 'imported'],
    description: 'a'.repeat(400) // ~500 bytes total payload
  };
}

function allocateRequestGarbage() {
  const list = [];
  for (let i = 0; i < 300; i++) {
    list.push({
      requestId: Math.random(),
      route: '/api/v1/products/' + Math.floor(Math.random() * 1000),
      meta: { ua: 'Mozilla/5.0', ip: '127.0.0.1', method: 'GET' }
    });
  }
}

async function run() {
  const numKeys = 500000;
  const keys = Array.from({ length: numKeys }, (_, i) => `key-${i}`);
  const opsCount = 1000000;
  const garbageFrequency = 200;
  const gcForceFrequency = 10000;

  let gcCount = 0;
  let totalGcTime = 0;
  let maxGcTime = 0;

  const obs = new PerformanceObserver((list) => {
    for (const entry of list.getEntries()) {
      gcCount++;
      totalGcTime += entry.duration;
      if (entry.duration > maxGcTime) {
        maxGcTime = entry.duration;
      }
    }
  });

  global.gc && global.gc();
  const startHeap = process.memoryUsage().heapUsed;
  const startRss = process.memoryUsage().rss;

  let cache;
  let manager;
  
  if (engine === 'lru') {
    cache = new LRUCache({ max: numKeys });
    for (let i = 0; i < numKeys; i++) {
      cache.set(keys[i], getLargePayload(i));
    }
  } else {
    manager = new CacheManager();
    cache = manager.createCache('zipf-hybrid-gc', {
      policy: 'lru',
      capacity: numKeys,
      shards: 16,
      l1Capacity: 10000
    });
    for (let i = 0; i < numKeys; i++) {
      cache.set(keys[i], getLargePayload(i));
    }
  }

  obs.observe({ entryTypes: ['gc'] });

  const startTime = Date.now();
  const latencies = new Float64Array(opsCount);

  for (let i = 0; i < opsCount; i++) {
    const key = keys[Math.floor(Math.random() * numKeys)];
    const opStart = performance.now();
    
    if (i % 10 === 0) {
      cache.set(key, getLargePayload(i));
    } else {
      cache.get(key);
    }
    
    latencies[i] = performance.now() - opStart;

    if (i % garbageFrequency === 0) {
      allocateRequestGarbage();
    }

    if (i % gcForceFrequency === 0 && global.gc) {
      global.gc();
    }
  }

  const duration = Date.now() - startTime;
  
  // Wait to flush PerformanceObserver events
  await new Promise((resolve) => setTimeout(resolve, 300));
  
  const endHeap = process.memoryUsage().heapUsed;
  const endRss = process.memoryUsage().rss;

  obs.disconnect();
  
  if (engine === 'lru') {
    cache.clear();
  } else {
    cache.dispose();
    manager.dispose();
  }
  
  global.gc && global.gc();

  // Sort latencies for percentiles
  latencies.sort();

  const getPercentile = (arr, pct) => {
    const idx = Math.floor(arr.length * (pct / 100));
    return (arr[idx] * 1000).toFixed(1); // returned in μs
  };

  const getAverage = (arr) => {
    let sum = 0;
    for (let i = 0; i < arr.length; i++) {
      sum += arr[i];
    }
    return ((sum / arr.length) * 1000).toFixed(1); // returned in μs
  };

  const results = {
    duration,
    avgLat: getAverage(latencies),
    p50: getPercentile(latencies, 50),
    p90: getPercentile(latencies, 90),
    p99: getPercentile(latencies, 99),
    p99_9: getPercentile(latencies, 99.9),
    gcCount,
    totalGcTime: totalGcTime.toFixed(1),
    maxGcTime: maxGcTime.toFixed(1),
    startHeap: formatMemory(startHeap),
    endHeap: formatMemory(endHeap),
    startRss: formatMemory(startRss),
    endRss: formatMemory(endRss),
    rssDelta: formatMemory(endRss - startRss)
  };

  console.log(JSON.stringify(results));
}

run().catch(console.error);
