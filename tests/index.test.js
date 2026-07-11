const test = require('node:test');
const assert = require('node:assert');
const { CacheManager } = require('../index.js');

test('CacheManager - isolated caches', () => {
  const manager = new CacheManager();
  
  const cache1 = manager.createCache('lru-1', { policy: 'lru', capacity: 10 });
  const cache2 = manager.createCache('lru-2', { policy: 'lru', capacity: 10 });
  
  cache1.set('key', 'value-1');
  cache2.set('key', 'value-2');
  
  assert.strictEqual(cache1.get('key'), 'value-1');
  assert.strictEqual(cache2.get('key'), 'value-2');
  
  const statsLeft = manager.getCache('lru-1').stats();
  const statsRight = cache1.stats();
  statsLeft.memory.processRss = 0;
  statsRight.memory.processRss = 0;
  assert.deepStrictEqual(statsLeft, statsRight);
  
  manager.deleteCache('lru-1');
  assert.strictEqual(manager.getCache('lru-1'), null);
  assert.ok(manager.getCache('lru-2') !== null);
});

test('Cache - DataType Preservation (String, Buffer, JSON Object)', () => {
  const manager = new CacheManager();
  const cache = manager.createCache('lru-types', { policy: 'lru', capacity: 10, shards: 1 });

  // String
  cache.set('str', 'hello world');
  assert.strictEqual(cache.get('str'), 'hello world');

  // Buffer
  const buf = Buffer.from([1, 2, 3, 4]);
  cache.set('buf', buf);
  const retrievedBuf = cache.get('buf');
  assert.ok(Buffer.isBuffer(retrievedBuf));
  assert.deepStrictEqual(retrievedBuf, buf);

  // JSON Object
  const obj = { x: 42, y: 'test', z: [1, 2] };
  cache.set('json', obj);
  assert.deepStrictEqual(cache.get('json'), obj);
});

test('Cache Policies - LRU eviction', () => {
  const manager = new CacheManager();
  // Set shards: 1, l1Capacity: 0 to ensure a single native eviction pool of size 3
  const cache = manager.createCache('lru-evict', { policy: 'lru', capacity: 3, shards: 1, l1Capacity: 0 });

  cache.set('a', 1);
  cache.set('b', 2);
  cache.set('c', 3);

  // Access a to make it most recently used, b becomes oldest
  cache.get('a');

  // Insert d, which should evict b
  cache.set('d', 4);

  assert.strictEqual(cache.get('b'), undefined);
  assert.strictEqual(cache.get('a'), 1);
  assert.strictEqual(cache.get('c'), 3);
  assert.strictEqual(cache.get('d'), 4);
});

test('Cache Policies - ARC adaptation & eviction', () => {
  const manager = new CacheManager();
  // Set shards: 1, l1Capacity: 0 to ensure a single native eviction pool of size 4
  const cache = manager.createCache('arc-evict', { policy: 'arc', capacity: 4, shards: 1, l1Capacity: 0 });

  // Warm up ARC
  cache.set('a', 1);
  cache.set('b', 2);
  cache.set('c', 3);
  cache.set('d', 4);

  // Retrieve some keys to establish frequency (moving them to T2)
  cache.get('a');
  cache.get('b');

  // Now a and b are in T2 (frequent), c and d are in T1 (recent)
  // Insert e, which triggers replace. Since |T1| > p, it evicts from T1 (evicting c or d)
  cache.set('e', 5);

  const stats = cache.stats();
  assert.strictEqual(stats.size, 4);
});

test('Cache Policies - W-TinyLFU eviction competition', () => {
  const manager = new CacheManager();
  // Set shards: 1, l1Capacity: 0 to ensure a single native eviction pool of size 10
  const cache = manager.createCache('tinylfu-evict', { policy: 'w-tinylfu', capacity: 10, shards: 1, l1Capacity: 0 });

  // Fill cache
  for (let i = 0; i < 12; i++) {
    cache.set(`key-${i}`, i);
  }

  const stats = cache.stats();
  assert.strictEqual(stats.capacity, 10);
  assert.ok(stats.size <= 10);
});

test('Cache - TTL (Time To Live)', async () => {
  const manager = new CacheManager();
  const cache = manager.createCache('lru-ttl', { policy: 'lru', capacity: 10, shards: 1 });

  cache.set('short-lived', 'expire-me', 10); // 10ms TTL
  cache.set('long-lived', 'keep-me', 1000);   // 1000ms TTL

  assert.strictEqual(cache.get('short-lived'), 'expire-me');
  assert.strictEqual(cache.get('long-lived'), 'keep-me');

  // Wait 30ms for short-lived key to expire
  await new Promise((resolve) => setTimeout(resolve, 30));

  assert.strictEqual(cache.get('short-lived'), undefined);
  assert.strictEqual(cache.get('long-lived'), 'keep-me');
});

test('Cache - keys() and delete()', () => {
  const manager = new CacheManager();
  const cache = manager.createCache('lru-ops', { policy: 'lru', capacity: 5, shards: 1 });

  cache.set('a', 1);
  cache.set('b', 2);
  cache.set('c', 3);

  assert.deepStrictEqual(cache.keys().sort(), ['a', 'b', 'c']);

  assert.strictEqual(cache.delete('b'), true);
  assert.strictEqual(cache.delete('non-existent'), false);

  assert.deepStrictEqual(cache.keys().sort(), ['a', 'c']);
});

test('Cache - touch(key, ttl)', async () => {
  const manager = new CacheManager();
  const cache = manager.createCache('lru-touch', { policy: 'lru', capacity: 10, shards: 1 });

  cache.set('a', 'value', 15); // 15ms TTL
  assert.strictEqual(cache.touch('a', 1000), true); // Extend to 1000ms

  // Wait 30ms. It would normally have expired, but should remain due to touch
  await new Promise((resolve) => setTimeout(resolve, 30));
  assert.strictEqual(cache.get('a'), 'value');
  
  assert.strictEqual(cache.touch('non-existent', 100), false);
});

test('Cache - Atomic Counters (increment / decrement)', () => {
  const manager = new CacheManager();
  const cache = manager.createCache('lru-counters', { policy: 'lru', capacity: 10, shards: 1 });

  // Init and increment
  assert.strictEqual(cache.increment('counter', 5), 5);
  assert.strictEqual(cache.increment('counter', 2), 7);
  assert.strictEqual(cache.get('counter'), 7);

  // Decrement
  assert.strictEqual(cache.decrement('counter', 3), 4);
  assert.strictEqual(cache.decrement('counter', 1), 3);
  assert.strictEqual(cache.get('counter'), 3);
});

test('Cache - Batch Operations (mget / mset / mdelete)', () => {
  const manager = new CacheManager();
  const cache = manager.createCache('lru-batch', { policy: 'lru', capacity: 10, shards: 1 });

  // mset
  cache.mset({ a: 1, b: 'hello', c: { x: 9 } });

  // mget
  const retrieved = cache.mget(['a', 'b', 'c', 'd']);
  assert.deepStrictEqual(retrieved, {
    a: 1,
    b: 'hello',
    c: { x: 9 },
  });

  // mdelete
  assert.strictEqual(cache.mdelete(['a', 'b', 'z']), 2); // 'a' and 'b' deleted, 'z' not found
  assert.strictEqual(cache.get('a'), undefined);
  assert.deepStrictEqual(cache.get('c'), { x: 9 });
});

test('Cache - has() and peek()', () => {
  const manager = new CacheManager();
  // Set l1Capacity: 0 to ensure we directly verify native peek & LRU updating
  const cache = manager.createCache('lru-has-peek', { policy: 'lru', capacity: 3, shards: 1, l1Capacity: 0 });

  cache.set('a', 1);
  cache.set('b', 2);
  cache.set('c', 3);

  // has
  assert.strictEqual(cache.has('a'), true);
  assert.strictEqual(cache.has('z'), false);

  // peek does not update eviction order
  assert.strictEqual(cache.peek('a'), 1);
  cache.set('d', 4); // Should evict 'a' (the tail, since peek didn't promote it!)
  assert.strictEqual(cache.get('a'), undefined);
});

test('Cache - getOrSet() Coalescing', async () => {
  const manager = new CacheManager();
  const cache = manager.createCache('lru-getorset', { policy: 'lru', capacity: 10, shards: 1 });

  let calls = 0;
  const factory = () => {
    calls++;
    return new Promise((resolve) => setTimeout(() => resolve('computed'), 20));
  };

  // Trigger concurrent calls
  const [res1, res2] = await Promise.all([
    cache.getOrSet('key', factory),
    cache.getOrSet('key', factory),
  ]);

  assert.strictEqual(res1, 'computed');
  assert.strictEqual(res2, 'computed');
  assert.strictEqual(calls, 1); // Factory must only be called once!

  assert.strictEqual(cache.get('key'), 'computed');
});

test('Cache - maxBytes Memory Eviction Limit', () => {
  const manager = new CacheManager();
  // Set capacity = 100, shards = 1, l1Capacity = 0, but maxBytes = 35.
  const cache = manager.createCache('lru-bytes', {
    policy: 'lru',
    capacity: 100,
    maxBytes: 35,
    shards: 1,
    l1Capacity: 0
  });

  cache.set('k1', '1234567890'); // key len 2 + value len 10 = 12 bytes + 1 tag = 13 bytes.
  cache.set('k2', '1234567890'); // key len 2 + value len 10 = 12 bytes + 1 tag = 13 bytes. Total = 26 bytes.
  
  assert.strictEqual(cache.get('k1'), '1234567890');
  assert.strictEqual(cache.get('k2'), '1234567890');

  // Insert k3. Total would be 39 bytes (exceeds 35 maxBytes). Should evict k1.
  cache.set('k3', '1234567890');

  assert.strictEqual(cache.get('k1'), undefined);
  assert.strictEqual(cache.get('k2'), '1234567890');
  assert.strictEqual(cache.get('k3'), '1234567890');
});

test('Cache - Deterministic dispose()', () => {
  const manager = new CacheManager();
  const cache = manager.createCache('lru-dispose', { policy: 'lru', capacity: 10, shards: 1 });

  cache.set('a', 1);
  cache.dispose();

  assert.throws(() => {
    cache.get('a');
  }, /Cache has been disposed/);
});

test('Cache - Optional LZ4 Compression', () => {
  const manager = new CacheManager();
  const cache = manager.createCache('lru-compressed', {
    policy: 'lru',
    capacity: 10,
    compression: true,
    shards: 1
  });

  const obj = { message: 'hello with compression', values: [1, 2, 3] };
  cache.set('key', obj);

  assert.deepStrictEqual(cache.get('key'), obj);
  cache.dispose();
  manager.dispose();
});

test('Cache - Three-layered Config & Overrides', () => {
  const manager = new CacheManager({
    compression: { enabled: true, minSizeBytes: 100 },
    l1: { enabled: true, capacity: 5 },
    eviction: { policy: 'lru', capacity: 10 }
  });

  const cache1 = manager.createCache('test-override-1', {
    compression: { enabled: false }
  });

  const cache2 = manager.createCache('test-override-2');

  const smallObj = { val: 'small' };
  const largeObj = { val: 'a'.repeat(200) };

  // cache1 has compression disabled, so it shouldn't compress even large objects
  cache1.set('k1', largeObj);
  assert.deepStrictEqual(cache1.get('k1'), largeObj);

  // cache2 has compression enabled (inherited), but k2 is small (<100 bytes) so it won't compress
  cache2.set('k2', smallObj);
  assert.deepStrictEqual(cache2.get('k2'), smallObj);

  // cache2: k3 is large (>=100 bytes) so it will compress
  cache2.set('k3', largeObj);
  assert.deepStrictEqual(cache2.get('k3'), largeObj);

  // cache2: k4 is large, but operation override disables compression
  cache2.set('k4', largeObj, { compression: false });
  assert.deepStrictEqual(cache2.get('k4'), largeObj);

  cache1.dispose();
  cache2.dispose();
  manager.dispose();
});

test('Cache - Advanced Compression Matrices, Inheritances, Boundaries & Safety', () => {
  const manager = new CacheManager({
    compression: { enabled: true, minSizeBytes: 100 },
    eviction: { policy: 'lru', capacity: 100 }
  });

  const cache = manager.createCache('compat-advanced');

  const createJsonOfSize = (size) => {
    // base structure: {"d":""} is 8 bytes
    const padLength = size - 8;
    return { d: 'a'.repeat(padLength) };
  };

  const obj99 = createJsonOfSize(99);
  const obj100 = createJsonOfSize(100);
  const obj101 = createJsonOfSize(101);

  cache.set('key99', obj99);
  cache.set('key100', obj100);
  cache.set('key101', obj101);

  assert.deepStrictEqual(cache.get('key99'), obj99);
  assert.deepStrictEqual(cache.get('key100'), obj100);
  assert.deepStrictEqual(cache.get('key101'), obj101);

  // Mixed compression mget batch test
  const batchRes = cache.mget(['key99', 'key100', 'key101']);
  assert.deepStrictEqual(batchRes, {
    key99: obj99,
    key100: obj100,
    key101: obj101
  });

  // Unknown tag compatibility / safety
  assert.throws(() => {
    cache._native.testDeserializeRaw([99, 1, 2, 3]);
  }, /Invalid data type tag in cache storage/);

  cache.dispose();
  manager.dispose();
});

test('Cache - Read/Write Config Change Isolation', () => {
  const manager = new CacheManager({
    compression: { enabled: true, minSizeBytes: 10 },
    eviction: { policy: 'lru', capacity: 10 }
  });

  const cache = manager.createCache('change-isolation-cache');
  const payload = { test: 'value_to_compress' };

  cache.set('item', payload);

  // Dynamically disable compression
  cache._config.compression.enabled = false;

  // Reading must still deserialize correctly because Tag 5 is self-describing
  assert.deepStrictEqual(cache.get('item'), payload);

  cache.dispose();
  manager.dispose();
});

test('Cache - Reverse Tag Change Isolation', () => {
  const manager = new CacheManager({
    compression: { enabled: false },
    eviction: { policy: 'lru', capacity: 10 }
  });

  const cache = manager.createCache('reverse-isolation-cache');
  const payload = { test: 'value_uncompressed' };

  // Stored with Tag 3 (raw JSON)
  cache.set('item', payload);

  // Dynamically enable compression on the config object
  cache._config.compression.enabled = true;
  cache._config.compression.minSizeBytes = 5;

  // Reading must still deserialize correctly because Tag 3 is self-describing
  assert.deepStrictEqual(cache.get('item'), payload);

  cache.dispose();
  manager.dispose();
});

test('Performance Regression - Fast Path Throughput', () => {
  const manager = new CacheManager({
    eviction: { capacity: 10000 }
  });
  const cache = manager.createCache('perf-test');

  const start = performance.now();
  const iterations = 30000;
  for (let i = 0; i < iterations; i++) {
    cache.set(`key-${i}`, { id: i });
  }
  const duration = performance.now() - start;
  const opsSec = (iterations / duration) * 1000;

  // The default fast-path should comfortably do at least 150k ops/sec even in loaded test environments
  assert.ok(opsSec > 150000, `Default fast-path throughput degraded! Measured: ${Math.round(opsSec)} ops/sec`);

  cache.dispose();
  manager.dispose();
});

test('Cache Policies - W-TinyLFU Eviction Memory Leak Protection', () => {
  const manager = new CacheManager();
  const capacity = 100;
  
  const cache = manager.createCache('tinylfu-leak-test', {
    policy: 'tinylfu',
    capacity,
    shards: 1, // Single shard for deterministic sizing behavior
    l1Capacity: 0
  });

  // Write way more items than the cache capacity to trigger evictions
  for (let i = 0; i < 1000; i++) {
    cache.set(`key-${i}`, `val-${i}`);
  }

  // 1. Verify stats size matches capacity boundaries
  const stats = cache.stats();
  assert.ok(stats.size <= capacity, `Cache size exceeded capacity! Size: ${stats.size}`);

  // 2. Black-box leak check: query every key to confirm only <= capacity keys are readable/present
  let readableKeysCount = 0;
  for (let i = 0; i < 1000; i++) {
    if (cache.get(`key-${i}`) !== undefined) {
      readableKeysCount++;
    }
  }
  assert.ok(readableKeysCount <= capacity, `Leaked keys are readable! Count: ${readableKeysCount}`);

  cache.dispose();
  manager.dispose();
});

test('Panic Safety - Rust Panics do not crash Node.js process and poison cleanly', () => {
  const manager = new CacheManager();
  const cache = manager.createCache('panic-test', { capacity: 10, shards: 8 });

  // 1. Force a panic during a locked operation on "key-0"
  assert.throws(() => {
    cache._native.testPanic("key-0");
  }, /Intended test panic in Rust code!/);

  // 2. Future operations on keys mapping to that same poisoned shard must fail with a clear exception
  assert.throws(() => {
    cache.set("key-0", "val");
  }, /Cache has been disposed/);

  // 3. Keys mapping to different shards must continue to work perfectly without issue (isolation)
  let worksOnOtherShard = false;
  for (let i = 1; i < 100; i++) {
    try {
      cache.set(`key-${i}`, "val");
      assert.strictEqual(cache.get(`key-${i}`), "val");
      worksOnOtherShard = true;
      break;
    } catch (e) {
      // Hashed to the same poisoned shard, skip and try next
    }
  }
  assert.ok(worksOnOtherShard, "Unpoisoned shards should remain fully functional");

  cache.dispose();
  manager.dispose();
});

test('Security - LZ4 Decompression size safety limit protection', () => {
  const manager = new CacheManager();
  const cache = manager.createCache('decomp-limit-test', { capacity: 10 });

  // Construct a raw buffer payload:
  // - Byte 0: Tag 5 (LZ4)
  // - Bytes 1-4: 100 MB uncompressed size prefix (104,857,600 bytes = little-endian [0x00, 0x00, 0x40, 0x06])
  // - Bytes 5..: Dummy payload bytes
  const malformedPayload = [
    5,                      // Tag 5
    0x00, 0x00, 0x40, 0x06, // 100 MB size prefix
    0x12, 0x34              // Dummy bytes
  ];

  assert.throws(() => {
    cache._native.testDeserializeRaw(malformedPayload);
  }, /exceeds safety limit/);

  cache.dispose();
  manager.dispose();
});

test('Security - LZ4 Decompression size safety limit scales dynamically with maxBytes', () => {
  const manager = new CacheManager();
  // maxBytes is 10 KB, so limit scales down to 10% = 1 KB (1000 bytes)
  const cache = manager.createCache('decomp-limit-scale-test', { 
    capacity: 10,
    maxBytes: 10000
  });

  // Construct a payload:
  // - Byte 0: Tag 5 (LZ4)
  // - Bytes 1-4: 2 KB uncompressed size prefix (2048 bytes = little-endian [0x00, 0x08, 0x00, 0x00])
  // - Bytes 5..: Dummy payload bytes
  const malformedPayload = [
    5,                      // Tag 5
    0x00, 0x08, 0x00, 0x00, // 2 KB (2048) size prefix
    0x12, 0x34              // Dummy bytes
  ];

  assert.throws(() => {
    cache._native.testDeserializeRaw(malformedPayload);
  }, /exceeds safety limit of 1024 bytes/);

  cache.dispose();
  manager.dispose();
});

test('Security - Key length safety limit and type validation', () => {
  const manager = new CacheManager();
  const cache = manager.createCache('key-safety-test', { capacity: 10 });

  // 1. Key must be a string
  assert.throws(() => {
    cache.get(123);
  }, TypeError, /Key must be a string/);

  // 2. Key must not exceed 8192 bytes
  const longKey = 'a'.repeat(8193);
  assert.throws(() => {
    cache.get(longKey);
  }, RangeError, /Key length exceeds safety limit of 8192 bytes/);

  cache.dispose();
  manager.dispose();
});

test('Security - Key size accounting in maxBytes and eviction proof', () => {
  const manager = new CacheManager();
  const cache = manager.createCache('key-accounting-test', {
    policy: 'lru',
    capacity: 100,
    maxBytes: 5000, // 5 KB limit
    shards: 1,
    l1Capacity: 0
  });

  const largeKey = 'k'.repeat(4000);
  const tinyVal = '1234567890'; // 10 bytes payload + 1 byte tag = 11 bytes.
  
  cache.set(largeKey, tinyVal);

  const stats1 = cache.stats();
  // key (4000) + value (11) = 4011 bytes
  assert.strictEqual(stats1.bytesUsed, 4011);

  // Insert a second key of 2000 bytes.
  // Total size (4011 + 2011 = 6022) exceeds 5000 maxBytes, triggering eviction of largeKey.
  const secondKey = 'j'.repeat(2000);
  cache.set(secondKey, tinyVal);

  assert.strictEqual(cache.get(largeKey), undefined);
  assert.strictEqual(cache.get(secondKey), tinyVal);

  const stats2 = cache.stats();
  assert.strictEqual(stats2.bytesUsed, 2011);

  cache.dispose();
  manager.dispose();
});

test('Cache Policies - Eviction under multiple shards with randomized routing', () => {
  const manager = new CacheManager();
  // Total capacity = 20 across 4 shards (so each shard capacity = 5)
  const cache = manager.createCache('multi-shard-evict-test', {
    policy: 'lru',
    capacity: 20,
    shards: 4,
    l1Capacity: 0
  });

  // Insert 40 keys. Evictions must occur because total capacity is exceeded.
  const insertedKeys = [];
  for (let i = 0; i < 40; i++) {
    const key = `key-${i}`;
    cache.set(key, `val-${i}`);
    insertedKeys.push(key);
  }

  // Invariant 1: Total size across all shards is bounded by configured capacity (4 shards * 5 = 20)
  const stats = cache.stats();
  assert.ok(stats.size <= 20, `Total cache size (${stats.size}) should be bounded by capacity (20)`);

  // Invariant 2: Any key that still exists in the cache must map to its correct value.
  const keysInCache = cache.keys();
  for (const key of keysInCache) {
    const match = key.match(/^key-(\d+)$/);
    assert.ok(match, `Key ${key} matches pattern`);
    const index = match[1];
    assert.strictEqual(cache.get(key), `val-${index}`);
  }

  cache.dispose();
  manager.dispose();
});

test('Cache - Worker Threads Sharing', async () => {
  const { Worker } = require('node:worker_threads');
  
  const manager = new CacheManager({
    l1: { enabled: false } // Disable L1 for strict cross-thread consistency in this test
  });
  
  // Create the shared cache in main thread
  const cache = manager.createCache('shared-worker-cache', { policy: 'lru', capacity: 1000 });
  cache.set('main-key', 'main-value');
  
  // Start concurrent worker threads to write and read
  const workerCode = `
    const { workerData, parentPort } = require('node:worker_threads');
    const { CacheManager } = require('./index.js');
    
    const manager = new CacheManager({ l1: { enabled: false } });
    const cache = manager.getCache('shared-worker-cache');
    const threadId = workerData.threadId;
    
    for (let i = 0; i < 100; i++) {
      cache.set(\`thread-\${threadId}-key-\${i}\`, \`value-\${i}\`);
      const val = cache.get('main-key');
      if (val !== 'main-value') {
        throw new Error('Stale or missing main-key');
      }
    }
    
    parentPort.postMessage('done');
  `;
  
  const spawnWorker = (threadId) => {
    return new Promise((resolve, reject) => {
      const worker = new Worker(workerCode, {
        eval: true,
        workerData: { threadId }
      });
      worker.on('message', resolve);
      worker.on('error', reject);
      worker.on('exit', (code) => {
        if (code !== 0) reject(new Error(`Worker stopped with exit code ${code}`));
      });
    });
  };
  
  // Spawn 4 concurrent worker threads
  await Promise.all([
    spawnWorker(1),
    spawnWorker(2),
    spawnWorker(3),
    spawnWorker(4)
  ]);
  
  // Verify main thread can read all values written by the workers
  for (let t = 1; t <= 4; t++) {
    for (let i = 0; i < 100; i++) {
      assert.strictEqual(cache.get(`thread-${t}-key-${i}`), `value-${i}`);
    }
  }
  
  // Cleanup
  manager.deleteCache('shared-worker-cache');
  manager.dispose();
});

test('Lifecycle Hooks - onMiss', () => {
  const misses = [];
  const manager = new CacheManager();
  const cache = manager.createCache('hook-miss', {
    policy: 'lru',
    capacity: 10,
    hooks: {
      onMiss: (key) => {
        misses.push(key);
      }
    }
  });

  cache.get('miss1');
  cache.mget(['miss2', 'miss3']);

  assert.deepStrictEqual(misses, ['miss1', 'miss2', 'miss3']);
  manager.deleteCache('hook-miss');
});

test('Lifecycle Hooks - onEvict (replaced)', () => {
  const evictions = [];
  const manager = new CacheManager();
  const cache = manager.createCache('hook-evict-replaced', {
    policy: 'lru',
    capacity: 10,
    hooks: {
      onEvict: (key, value, reason) => {
        evictions.push({ key, value, reason });
      }
    }
  });

  cache.set('key1', 'val1');
  cache.set('key1', 'val2'); // overwritten/replaced

  assert.deepStrictEqual(evictions, [
    { key: 'key1', value: 'val1', reason: 'replaced' }
  ]);
  manager.deleteCache('hook-evict-replaced');
});

test('Lifecycle Hooks - onEvict (capacity eviction)', () => {
  const evictions = [];
  const manager = new CacheManager();
  const cache = manager.createCache('hook-evict-capacity', {
    policy: 'lru',
    capacity: 2,
    shards: 1,
    l1Capacity: 0,
    hooks: {
      onEvict: (key, value, reason) => {
        evictions.push({ key, value, reason });
      }
    }
  });

  cache.set('key1', 'val1');
  cache.set('key2', 'val2');
  cache.set('key3', 'val3'); // Should evict key1

  assert.strictEqual(evictions.length, 1);
  assert.strictEqual(evictions[0].key, 'key1');
  assert.strictEqual(evictions[0].value, 'val1');
  assert.strictEqual(evictions[0].reason, 'evicted');
  manager.deleteCache('hook-evict-capacity');
});

test('Lifecycle Hooks - onExpire (lazy expiration)', async () => {
  const expirations = [];
  const manager = new CacheManager();
  const cache = manager.createCache('hook-expire', {
    policy: 'lru',
    capacity: 10,
    shards: 1,
    hooks: {
      onExpire: (key, value) => {
        expirations.push({ key, value });
      }
    }
  });

  cache.set('temp1', 'exp1', 10); // TTL 10ms
  cache.set('temp2', 'exp2', 10); // TTL 10ms
  
  // Wait for TTL to expire
  await new Promise((resolve) => setTimeout(resolve, 100));

  // Access temp1 to trigger lazy expiration
  const val1 = cache.get('temp1');
  assert.strictEqual(val1, undefined);
  assert.deepStrictEqual(expirations, [{ key: 'temp1', value: 'exp1' }]);

  // Access temp2 using has/touch/peek/mget
  const has2 = cache.has('temp2');
  assert.strictEqual(has2, false);
  assert.deepStrictEqual(expirations, [
    { key: 'temp1', value: 'exp1' },
    { key: 'temp2', value: 'exp2' }
  ]);

  manager.deleteCache('hook-expire');
});

test('Observability - Enriched Stats & Lock-free Atomics', () => {
  const manager = new CacheManager();
  const cache = manager.createCache('obs-stats', {
    policy: 'lru',
    capacity: 10,
    shards: 2,
    l1Capacity: 0
  });

  // Check initial stats
  let st = cache.stats();
  assert.strictEqual(st.sets, 0);
  assert.strictEqual(st.hits, 0);
  assert.strictEqual(st.misses, 0);
  assert.strictEqual(st.deletes, 0);
  assert.strictEqual(st.evictions, 0);
  assert.strictEqual(st.expirations, 0);
  assert.strictEqual(st.hitRate, 0);
  assert.ok(st.uptimeMs >= 0);
  assert.strictEqual(st.l1.size, 0);

  // Perform operations
  cache.set('a', 1);
  cache.set('b', 2);
  cache.set('c', 3); // 3 sets
  
  cache.get('a'); // 1 hit
  cache.get('b'); // 2 hits
  cache.get('nonexistent'); // 1 miss

  cache.delete('c'); // 1 delete

  st = cache.stats();
  assert.strictEqual(st.sets, 3);
  assert.strictEqual(st.hits, 2);
  assert.strictEqual(st.misses, 1);
  assert.strictEqual(st.deletes, 1);
  assert.strictEqual(st.hitRate, 2 / 3);

  manager.deleteCache('obs-stats');
});

test('Observability - Monitor Floors & Rejections', () => {
  const manager = new CacheManager({
    monitoring: { minIntervalMs: 200 }
  });
  const cache = manager.createCache('obs-floors');

  // Callback dummy
  const cb = () => {};

  // 1. Absolute hard floor rejection (< 16ms)
  assert.throws(() => {
    cache.monitor(cb, 10);
  }, /is below the absolute minimum of 16ms/);

  // 2. Configured soft floor rejection (requested 100ms < configured 200ms)
  assert.throws(() => {
    cache.monitor(cb, 100);
  }, /is below configured minIntervalMs 200/);

  // 3. Invalid callback rejection
  assert.throws(() => {
    cache.monitor('not a function');
  }, TypeError);

  // 4. Default / valid interval runs
  const stop = cache.monitor(cb, 300);
  assert.strictEqual(typeof stop, 'function');
  stop();

  manager.deleteCache('obs-floors');
});

test('Observability - Monitoring Config Inheritance', () => {
  // Manager config overrides default 500ms -> 300ms
  const manager = new CacheManager({
    monitoring: { minIntervalMs: 300 }
  });

  // Inherits 300ms
  const cache1 = manager.createCache('obs-inherit1');
  assert.throws(() => {
    cache1.monitor(() => {}, 200);
  }, /is below configured minIntervalMs 300/);

  // Overrides namespace to 100ms
  const cache2 = manager.createCache('obs-inherit2', {
    monitoring: { minIntervalMs: 100 }
  });
  const stop2 = cache2.monitor(() => {}, 150); // Works!
  stop2();

  assert.throws(() => {
    cache2.monitor(() => {}, 50); // Fails (< 100ms)
  }, /is below configured minIntervalMs 100/);

  manager.deleteCache('obs-inherit1');
  manager.deleteCache('obs-inherit2');
});

test('Observability - Monitor Realtime Delta & Rates', async () => {
  const manager = new CacheManager();
  const cache = manager.createCache('obs-realtime', {
    monitoring: { minIntervalMs: 50 }, // Allow low interval for testing
    l1Capacity: 0
  });

  const ticks = [];
  const stop = cache.monitor((snapshot) => {
    ticks.push(snapshot);
  }, 50);

  // Perform some rapid ops to trigger rates
  cache.set('x', 10);
  cache.get('x');
  cache.get('y'); // Miss

  // Wait for at least one tick
  await new Promise((resolve) => setTimeout(resolve, 80));
  stop(); // Stop poller

  assert.ok(ticks.length >= 1, 'Should have received at least one monitoring snapshot');
  const snap = ticks[0];
  
  // Verify totals
  assert.ok(snap.totals.sets >= 1);
  assert.ok(snap.totals.hits >= 1);
  assert.ok(snap.totals.misses >= 1);

  // Verify delta
  assert.ok(snap.delta.sets >= 1);
  assert.ok(snap.delta.hits >= 1);
  
  // Verify rates
  assert.ok(snap.rates.opsPerSec >= 0);
  assert.strictEqual(snap.rates.hitRate, 0.5); // 1 hit, 1 miss

  // Wait another interval to verify callback stopped
  const initialTickCount = ticks.length;
  await new Promise((resolve) => setTimeout(resolve, 80));
  assert.strictEqual(ticks.length, initialTickCount, 'Should have stopped poller after disposer');

  manager.deleteCache('obs-realtime');
});

test('Observability - Shard Imbalance & Memory RSS', () => {
  const manager = new CacheManager();
  const cache = manager.createCache('obs-shards-memory', {
    policy: 'lru',
    capacity: 16,
    shards: 4,
    l1Capacity: 0
  });

  // Populate cache to ensure some data is in shards
  cache.set('key1', 'val1');
  cache.set('key2', 'val2');
  cache.set('key3', 'val3');
  cache.set('key4', 'val4');

  const st = cache.stats();

  // Shards validation
  assert.strictEqual(st.shards.count, 4);
  assert.strictEqual(st.shards.details.length, 4);
  assert.strictEqual(typeof st.shards.sizeStdDev, 'number');
  assert.ok(!isNaN(st.shards.sizeStdDev));

  // Memory validation
  assert.strictEqual(st.memory.payloadBytes, st.bytesUsed);
  assert.strictEqual(typeof st.memory.processRss, 'number');
  assert.ok(st.memory.processRss > 0);

  // Assert sum of size of shards is equal to total size
  const sumSize = st.shards.details.reduce((a, b) => a + b.size, 0);
  assert.strictEqual(sumSize, st.size);

  // Assert sum of bytes used of shards is equal to total bytes used
  const sumBytes = st.shards.details.reduce((a, b) => a + b.bytesUsed, 0);
  assert.strictEqual(sumBytes, st.bytesUsed);

  manager.deleteCache('obs-shards-memory');
});



