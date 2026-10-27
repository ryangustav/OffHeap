const test = require('node:test');
const assert = require('node:assert');
const { Cache, CacheManager } = require('../index.js');

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

test('L1 TTL - L1 copy expires together with the L2 entry', async () => {
  const manager = new CacheManager();
  const cache = manager.createCache('l1-ttl-get', { ttl: { defaultMs: 150 } });

  cache.set('k', 'v');
  assert.strictEqual(cache.get('k'), 'v'); // fills L1
  assert.strictEqual(cache.get('k'), 'v'); // served by L1
  assert.ok(cache.stats().l1.hits >= 1);

  await sleep(300);
  assert.strictEqual(cache.get('k'), undefined);
  assert.strictEqual(cache.has('k'), false);
  assert.strictEqual(cache.peek('k'), undefined);
  manager.deleteCache('l1-ttl-get');
});

test('L1 TTL - expiry fires onExpire even when the key was in L1', async () => {
  const expired = [];
  const manager = new CacheManager();
  const cache = manager.createCache('l1-ttl-hook', {
    hooks: { onExpire: (key) => expired.push(key) },
  });

  cache.set('k', 'v', 150);
  cache.get('k');
  await sleep(300);
  assert.strictEqual(cache.get('k'), undefined);
  assert.deepStrictEqual(expired, ['k']);
  manager.deleteCache('l1-ttl-hook');
});

test('L1 TTL - keys without TTL are not tracked', () => {
  const manager = new CacheManager();
  const cache = manager.createCache('l1-ttl-none');

  cache.set('k', { a: 1 });
  assert.deepStrictEqual(cache.get('k'), { a: 1 });
  assert.strictEqual(cache._l1.get('k').expiresAt, 0);
  manager.deleteCache('l1-ttl-none');
});

test('L1 TTL - mget respects TTL in L1', async () => {
  const manager = new CacheManager();
  const cache = manager.createCache('l1-ttl-mget');

  cache.mset({ a: 1, b: 2 }, 150);
  cache.set('c', 3);
  assert.deepStrictEqual(cache.mget(['a', 'b', 'c']), { a: 1, b: 2, c: 3 });

  await sleep(300);
  assert.deepStrictEqual(cache.mget(['a', 'b', 'c']), { c: 3 });
  manager.deleteCache('l1-ttl-mget');
});

test('L1 TTL - mset applies ttl.defaultMs like set()', async () => {
  const manager = new CacheManager();
  const cache = manager.createCache('l1-ttl-mset-default', { ttl: { defaultMs: 150 } });

  cache.mset({ a: 1 });
  assert.strictEqual(cache.get('a'), 1);
  await sleep(300);
  assert.strictEqual(cache.get('a'), undefined);
  manager.deleteCache('l1-ttl-mset-default');
});

test('L1 TTL - L1-only mode honours TTL and touch()', async () => {
  const manager = new CacheManager();
  const cache = manager.createCache('l1-ttl-only', { l2: { enabled: false } });

  cache.set('short', 'x', 150);
  cache.set('touched', 'y', 150);
  cache.set('forever', 'z');
  assert.strictEqual(cache.touch('touched', 1000), true);
  assert.strictEqual(cache.touch('missing', 1000), false);

  await sleep(300);
  assert.strictEqual(cache.get('short'), undefined);
  assert.strictEqual(cache.get('touched'), 'y');
  assert.strictEqual(cache.get('forever'), 'z');
  manager.deleteCache('l1-ttl-only');
});

test('L1 TTL - L1-only counters keep their deadline', async () => {
  const manager = new CacheManager();
  const cache = manager.createCache('l1-ttl-counter', { l2: { enabled: false } });

  assert.strictEqual(cache.increment('n', 1, 150), 1);
  assert.strictEqual(cache.increment('n'), 2);
  await sleep(300);
  assert.strictEqual(cache.get('n'), undefined);
  manager.deleteCache('l1-ttl-counter');
});

test('L1 TTL - binaries without getWithTtl skip L1 fills once TTLs are used', () => {
  const store = new Map();
  const legacyNative = {
    get: (key) => store.get(key),
    set: (key, value) => { store.set(key, value); },
  };
  const cache = new Cache(legacyNative, { l1: { enabled: true }, eviction: { capacity: 100 } });

  cache.set('plain', 'a');
  assert.strictEqual(cache.get('plain'), 'a');
  assert.strictEqual(cache._l1.has('plain'), true); // no TTL in play: L1 is filled

  cache.set('ttl', 'b', 1000);
  assert.strictEqual(cache.get('ttl'), 'b');
  assert.strictEqual(cache._l1.has('ttl'), false); // remaining TTL unknown: L1 is skipped
});
