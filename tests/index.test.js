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
  
  assert.deepStrictEqual(manager.getCache('lru-1').stats(), cache1.stats());
  
  manager.deleteCache('lru-1');
  assert.strictEqual(manager.getCache('lru-1'), null);
  assert.ok(manager.getCache('lru-2') !== null);
});

test('Cache - DataType Preservation (String, Buffer, JSON Object)', () => {
  const manager = new CacheManager();
  const cache = manager.createCache('lru-types', { policy: 'lru', capacity: 10 });

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
  // Set shards: 1 to ensure a single eviction pool of size 3
  const cache = manager.createCache('lru-evict', { policy: 'lru', capacity: 3, shards: 1 });

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
  // Set shards: 1 to ensure a single eviction pool of size 4
  const cache = manager.createCache('arc-evict', { policy: 'arc', capacity: 4, shards: 1 });

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
  // Set shards: 1 to ensure a single eviction pool of size 10
  const cache = manager.createCache('tinylfu-evict', { policy: 'tinylfu', capacity: 10, shards: 1 });

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
  const cache = manager.createCache('lru-ttl', { policy: 'lru', capacity: 10 });

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
  const cache = manager.createCache('lru-ops', { policy: 'lru', capacity: 5 });

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
  const cache = manager.createCache('lru-touch', { policy: 'lru', capacity: 10 });

  cache.set('a', 'value', 15); // 15ms TTL
  assert.strictEqual(cache.touch('a', 1000), true); // Extend to 1000ms

  // Wait 30ms. It would normally have expired, but should remain due to touch
  await new Promise((resolve) => setTimeout(resolve, 30));
  assert.strictEqual(cache.get('a'), 'value');
  
  assert.strictEqual(cache.touch('non-existent', 100), false);
});

test('Cache - Atomic Counters (increment / decrement)', () => {
  const manager = new CacheManager();
  const cache = manager.createCache('lru-counters', { policy: 'lru', capacity: 10 });

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
  const cache = manager.createCache('lru-batch', { policy: 'lru', capacity: 10 });

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
  const cache = manager.createCache('lru-has-peek', { policy: 'lru', capacity: 3, shards: 1 });

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
  const cache = manager.createCache('lru-getorset', { policy: 'lru', capacity: 10 });

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
  // Set capacity = 100, shards = 1, but maxBytes = 35.
  const cache = manager.createCache('lru-bytes', {
    policy: 'lru',
    capacity: 100,
    maxBytes: 35,
    shards: 1
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
  const cache = manager.createCache('lru-dispose', { policy: 'lru', capacity: 10 });

  cache.set('a', 1);
  cache.dispose();

  assert.throws(() => {
    cache.get('a');
  }, /Cache has been disposed/);
});
