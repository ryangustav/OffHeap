const { LRUCache } = require('lru-cache');
const Redis = require('ioredis');
const { PerformanceObserver } = require('perf_hooks');
const { CacheManager } = require('../index.js');

const configName = process.argv[2]; // 'lru-cache' | 'l1-only' | 'l1-l2' | 'l1-l2-l3-idle' | 'l1-l2-l3-active'
const testName = process.argv[3];   // 'gc-pressure' | 'throughput' | 'hit-ratio'

if (!configName || !testName) {
  console.error('Usage: node redis_layered_child.js <configName> <testName>');
  process.exit(1);
}

// Fixed capacities to ensure fair comparison (no heap size bias)
const CACHE_CAPACITY = 10000;
const ZIPF_KEYS_COUNT = 50000;

function formatMemory(bytes) {
  return (bytes / 1024 / 1024).toFixed(2) + ' MB';
}

function getPayload(id) {
  return {
    id,
    name: 'Object Product #' + id,
    price: Math.floor(Math.random() * 1000),
    active: true,
    tags: ['electronics', 'retail', 'promo'],
    data: 'a'.repeat(400) // ~500 bytes total JSON payload
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

// Zipfian Generator for hit-ratio testing
class FastZipf {
  constructor(size, skew) {
    this.size = size;
    this.cdf = new Float64Array(size);
    let sum = 0;
    for (let i = 1; i <= size; i++) {
      sum += 1.0 / Math.pow(i, skew);
    }
    let current = 0;
    for (let i = 1; i <= size; i++) {
      current += (1.0 / Math.pow(i, skew)) / sum;
      this.cdf[i - 1] = current;
    }
  }
  next() {
    const r = Math.random();
    let low = 0;
    let high = this.size - 1;
    while (low < high) {
      const mid = (low + high) >> 1;
      if (this.cdf[mid] >= r) {
        high = mid;
      } else {
        low = mid + 1;
      }
    }
    return low;
  }
}

async function run() {
  let cache;
  let manager;
  let redisClient;
  let redisOpsCount = 0;
  let isMeasuring = false;

  // Initialize Cache based on configuration
  if (configName === 'lru-cache') {
    cache = new LRUCache({ max: CACHE_CAPACITY });
  } else {
    const config = {
      eviction: { policy: 'lru', capacity: CACHE_CAPACITY },
      l1: { enabled: true, capacity: CACHE_CAPACITY } // Forces equal L1 size for fair comparison
    };

    if (configName === 'l1-only') {
      config.l2 = { enabled: false };
    } else {
      config.l2 = { enabled: true };
    }

    if (configName === 'l1-l2-l3-idle' || configName === 'l1-l2-l3-active') {
      redisClient = new Redis();
      await redisClient.flushall();

      // Instrument redis mock to track operations during active phase
      const originalSet = redisClient.set.bind(redisClient);
      const originalDel = redisClient.del.bind(redisClient);
      const originalPipeline = redisClient.pipeline.bind(redisClient);

      redisClient.set = async (...args) => {
        if (isMeasuring) redisOpsCount++;
        return originalSet(...args);
      };
      redisClient.del = async (...args) => {
        if (isMeasuring) redisOpsCount++;
        return originalDel(...args);
      };
      redisClient.pipeline = (...args) => {
        if (isMeasuring) redisOpsCount++;
        const pipe = originalPipeline(...args);
        const originalExec = pipe.exec.bind(pipe);
        pipe.exec = async () => {
          return originalExec();
        };
        return pipe;
      };

      config.redis = { client: redisClient, keyPrefix: 'layered:' };
    }

    manager = new CacheManager(config);
    cache = manager.createCache('redis-layered-bench');
  }

  // Pre-fill keys (warm up phase)
  const prefillKeys = Array.from({ length: CACHE_CAPACITY }, (_, i) => `key-${i}`);
  const payloadVal = getPayload(0);
  
  if (configName === 'lru-cache') {
    for (const key of prefillKeys) {
      cache.set(key, payloadVal);
    }
  } else if (configName === 'l1-only' || configName === 'l1-l2') {
    for (const key of prefillKeys) {
      cache.set(key, payloadVal);
    }
  } else if (configName === 'l1-l2-l3-idle') {
    // Fill L1, L2, and L3 (Redis) to ensure 100% local hits during Idle test
    for (const key of prefillKeys) {
      // Direct JS wrapper set will populate L2 and L3
      cache.set(key, payloadVal);
      // Let's populate L1 as well by reading it back
      await cache.getAsync(key);
    }
  } else if (configName === 'l1-l2-l3-active') {
    // Active L3: Populate Redis (L3) with values, but keep them absent from L1/L2
    const wrappedStr = '\0J' + JSON.stringify(payloadVal);
    const redisPrefixedPayload = Buffer.concat([Buffer.from([2]), Buffer.from(wrappedStr, 'utf8')]);
    for (const key of prefillKeys) {
      await redisClient.set(`layered:redis-layered-bench:${key}`, redisPrefixedPayload);
    }
  }

  // Start performance observer for GC pauses
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
  obs.observe({ entryTypes: ['gc'] });

  // Expose GC and do initial clean
  if (global.gc) {
    global.gc();
    global.gc();
  }

  const startHeap = process.memoryUsage().heapUsed;
  const startRss = process.memoryUsage().rss;
  const startTime = Date.now();

  isMeasuring = true;

  // RUN EXPERIMENT
  let hits = 0;
  let misses = 0;
  let opsExecuted = 0;
  let latencies;

  if (testName === 'gc-pressure') {
    const opsCount = 100000;
    latencies = new Float64Array(opsCount);
    const sampleKeys = Array.from({ length: CACHE_CAPACITY }, (_, i) => `key-${i}`);

    for (let i = 0; i < opsCount; i++) {
      const key = sampleKeys[Math.floor(Math.random() * CACHE_CAPACITY)];
      const opStart = performance.now();

      if (configName === 'l1-l2-l3-active') {
        // Force L3 Miss path on every read by clearing local caches
        cache._l1.clear();
        cache._native.clear();
        await cache.getAsync(key);
      } else {
        if (typeof cache.getAsync === 'function') {
          await cache.getAsync(key);
        } else {
          cache.get(key);
        }
      }

      latencies[i] = performance.now() - opStart;
      opsExecuted++;

      if (i % 200 === 0) {
        allocateRequestGarbage();
      }
      if (i % 10000 === 0 && global.gc) {
        global.gc();
      }
    }

  } else if (testName === 'throughput') {
    const opsCount = 50000;
    latencies = new Float64Array(opsCount);
    const sampleKeys = Array.from({ length: CACHE_CAPACITY }, (_, i) => `key-${i}`);

    for (let i = 0; i < opsCount; i++) {
      const key = sampleKeys[Math.floor(Math.random() * CACHE_CAPACITY)];
      const opStart = performance.now();

      if (configName === 'l1-l2-l3-active') {
        // Clear L1/L2 before read to force Redis network check
        cache._l1.clear();
        cache._native.clear();
        await cache.getAsync(key);
      } else {
        if (typeof cache.getAsync === 'function') {
          await cache.getAsync(key);
        } else {
          cache.get(key);
        }
      }

      latencies[i] = performance.now() - opStart;
      opsExecuted++;
    }

  } else if (testName === 'hit-ratio') {
    const totalOps = 50000;
    latencies = new Float64Array(totalOps);
    const zipf = new FastZipf(ZIPF_KEYS_COUNT, 0.7); // s=0.7 skew popularity
    const keysSequence = Array.from({ length: totalOps }, () => `key-${zipf.next()}`);

    for (let i = 0; i < totalOps; i++) {
      const key = keysSequence[i];
      const opStart = performance.now();

      let val;
      if (typeof cache.getAsync === 'function') {
        val = await cache.getAsync(key);
      } else {
        val = cache.get(key);
      }

      if (val !== undefined) {
        hits++;
      } else {
        misses++;
        cache.set(key, payloadVal);
      }

      latencies[i] = performance.now() - opStart;
      opsExecuted++;
    }
  }

  isMeasuring = false;

  const duration = Date.now() - startTime;
  
  // Wait to flush PerformanceObserver events
  await new Promise((resolve) => setTimeout(resolve, 300));

  const endHeap = process.memoryUsage().heapUsed;
  const endRss = process.memoryUsage().rss;

  obs.disconnect();

  // Cleanup
  if (configName === 'lru-cache') {
    cache.clear();
  } else {
    cache.dispose();
    manager.dispose();
  }

  if (redisClient) {
    await redisClient.quit();
  }
  
  if (global.gc) {
    global.gc();
  }

  // Calculate percentiles
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
    opsPerSec: Math.round((opsExecuted / duration) * 1000),
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
    rssDelta: formatMemory(endRss - startRss),
    hitRatio: opsExecuted > 0 && testName === 'hit-ratio' ? ((hits / opsExecuted) * 100).toFixed(2) : 'N/A',
    redisOpsCount
  };

  // Guard assertion for Idle scenario: should have exactly 0 Redis ops during measurement phase of gc-pressure or throughput
  if (configName === 'l1-l2-l3-idle' && testName !== 'hit-ratio' && redisOpsCount > 0) {
    console.error(`ERROR: L3 Idle scenario leaked ${redisOpsCount} operations to Redis!`);
    process.exit(2);
  }

  console.log(JSON.stringify(results));
}

run().catch((err) => {
  console.error(err);
  process.exit(1);
});
