const test = require('node:test');
const assert = require('node:assert');
const { CacheManager } = require('./index.js'); // NAPI generated file

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
  const cache = manager.createCache('lru-evict', { policy: 'lru', capacity: 3 });

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
  const cache = manager.createCache('arc-evict', { policy: 'arc', capacity: 4 });

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

  // Check stats
  const stats = cache.stats();
  assert.strictEqual(stats.size, 4);
});

test('Cache Policies - W-TinyLFU eviction competition', () => {
  const manager = new CacheManager();
  // W-TinyLFU capacity 10
  const cache = manager.createCache('tinylfu-evict', { policy: 'tinylfu', capacity: 10 });

  // Fill cache
  for (let i = 0; i < 12; i++) {
    cache.set(`key-${i}`, i);
  }

  // Check that size is limited to capacity
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
