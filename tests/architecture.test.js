const test = require('node:test');
const assert = require('node:assert');
const path = require('node:path');
const { Worker } = require('node:worker_threads');
const { CacheManager, nativeVersion } = require('../index.js');
const { version } = require('../package.json');

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

test('Format - strings starting with "\\0J" round-trip as strings', () => {
  const manager = new CacheManager();
  for (const l1 of [true, false]) {
    const cache = manager.createCache(`nul-strings-${l1}`, { l1: { enabled: l1 } });
    const spoofed = '\0J{"admin":true}';
    const broken = '\0Jnot json';

    cache.set('a', spoofed);
    cache.set('b', broken);
    cache.mset({ c: '\0' });
    assert.strictEqual(cache.get('a'), spoofed);
    assert.strictEqual(cache.get('b'), broken);
    assert.deepStrictEqual(cache.mget(['a', 'c']), { a: spoofed, c: '\0' });
    assert.strictEqual(cache.peek('b'), broken);
    manager.deleteCache(`nul-strings-${l1}`);
  }
});

test('Shards - default count is a power of two and keeps shards useful', () => {
  const manager = new CacheManager();
  const small = manager.createCache('shards-small', { capacity: 10 });
  const large = manager.createCache('shards-large', { capacity: 100000 });

  assert.strictEqual(small.stats().shards.count, 1);
  assert.strictEqual(small.stats().capacity, 10);
  const count = large.stats().shards.count;
  assert.ok(count >= 1 && (count & (count - 1)) === 0);
  assert.ok(100000 / count >= 64);

  manager.deleteCache('shards-small');
  manager.deleteCache('shards-large');
});

test('Expiration - purgeExpired removes never-read entries and fires onExpire', async () => {
  const expired = [];
  const manager = new CacheManager();
  const cache = manager.createCache('purge', {
    capacity: 1000,
    hooks: { onExpire: (key) => expired.push(key) },
  });

  for (let i = 0; i < 50; i++) cache.set(`t${i}`, i, 5);
  cache.set('keep', 'x');
  await sleep(20);

  assert.strictEqual(cache.purgeExpired(), 50);
  assert.strictEqual(expired.length, 50);
  assert.strictEqual(cache.stats().size, 1);
  assert.strictEqual(cache.get('keep'), 'x');
  manager.deleteCache('purge');
});

test('Expiration - writes sweep expired entries of their shard', async () => {
  const manager = new CacheManager();
  const cache = manager.createCache('write-sweep', { capacity: 10000, shards: 1 });

  for (let i = 0; i < 200; i++) cache.set(`t${i}`, i, 5);
  await sleep(20);
  for (let i = 0; i < 100; i++) cache.set(`p${i}`, i);

  const stats = cache.stats();
  assert.ok(stats.expirations > 0);
  assert.ok(stats.size < 300);
  manager.deleteCache('write-sweep');
});

test('Expiration - ttl.sweepIntervalMs purges in the background', async () => {
  const manager = new CacheManager();
  const cache = manager.createCache('timer-sweep', { capacity: 1000, ttl: { sweepIntervalMs: 10 } });

  for (let i = 0; i < 20; i++) cache.set(`t${i}`, i, 5);
  await sleep(80);
  assert.strictEqual(cache.stats().size, 0);

  manager.deleteCache('timer-sweep');
  assert.strictEqual(cache._sweepTimer, null);
});

test('Expiration - sliding TTL is extended by reads', async () => {
  const manager = new CacheManager();
  const cache = manager.createCache('sliding', { ttl: { mode: 'sliding' } });

  // Reads every 60ms keep a 200ms TTL alive well past its original deadline.
  cache.set('k', 'v', 200);
  for (let i = 0; i < 5; i++) {
    await sleep(60);
    assert.strictEqual(cache.get('k'), 'v');
  }
  await sleep(300);
  assert.strictEqual(cache.get('k'), undefined);
  manager.deleteCache('sliding');

  assert.throws(() => manager.createCache('bad-mode', { ttl: { mode: 'lru' } }), RangeError);
});

test('Expiration - sliding TTL in L1-only mode', async () => {
  const manager = new CacheManager();
  const cache = manager.createCache('sliding-l1', { l2: { enabled: false }, ttl: { mode: 'sliding' } });

  cache.set('k', 'v', 200);
  for (let i = 0; i < 5; i++) {
    await sleep(60);
    assert.strictEqual(cache.get('k'), 'v');
  }
  await sleep(300);
  assert.strictEqual(cache.get('k'), undefined);
  manager.deleteCache('sliding-l1');
});

test('L1 invalidation - writes through another wrapper drop stale L1 copies', () => {
  const managerA = new CacheManager();
  const cacheA = managerA.createCache('l1-gen-local');
  const cacheB = new CacheManager().getCache('l1-gen-local'); // separate wrapper, own L1

  cacheA.set('k', 1);
  assert.strictEqual(cacheA.get('k'), 1); // L1 filled in A
  cacheB.set('k', 2);
  assert.strictEqual(cacheA.get('k'), 2);

  assert.strictEqual(cacheA.get('k'), 2);
  cacheB.delete('k');
  assert.strictEqual(cacheA.get('k'), undefined);

  cacheA.mset({ a: 1, b: 2 });
  assert.deepStrictEqual(cacheA.mget(['a', 'b']), { a: 1, b: 2 });
  cacheB.mset({ a: 10 });
  assert.deepStrictEqual(cacheA.mget(['a', 'b']), { a: 10, b: 2 });

  managerA.deleteCache('l1-gen-local');
});

test('L1 invalidation - writes from a worker thread drop stale L1 copies', async () => {
  const manager = new CacheManager();
  const cache = manager.createCache('l1-gen-worker');
  cache.set('k', 'main');
  assert.strictEqual(cache.get('k'), 'main');

  const indexPath = path.join(__dirname, '..', 'index.js');
  await new Promise((resolve, reject) => {
    const worker = new Worker(
      `const { CacheManager } = require(${JSON.stringify(indexPath)});
       new CacheManager().getCache('l1-gen-worker').set('k', 'worker');`,
      { eval: true }
    );
    worker.once('error', reject);
    worker.once('exit', (code) => (code === 0 ? resolve() : reject(new Error(`worker exited with ${code}`))));
  });

  assert.strictEqual(cache.get('k'), 'worker');
  manager.deleteCache('l1-gen-worker');
});

test('L1 invalidation - can be disabled', () => {
  const manager = new CacheManager();
  const cache = manager.createCache('l1-gen-off', { l1: { sharedInvalidation: false } });
  assert.strictEqual(cache._gens, null);
  manager.deleteCache('l1-gen-off');
});

test('Versioning - native addon version matches package.json', () => {
  assert.strictEqual(nativeVersion, version);
});

test('API - getOrSetAsync always returns a Promise', async () => {
  const manager = new CacheManager();
  const cache = manager.createCache('get-or-set-async');

  const first = cache.getOrSetAsync('k', () => 'computed');
  assert.ok(first instanceof Promise);
  assert.strictEqual(await first, 'computed');

  const cached = cache.getOrSetAsync('k', () => 'other');
  assert.ok(cached instanceof Promise);
  assert.strictEqual(await cached, 'computed');

  await assert.rejects(cache.getOrSetAsync('boom', () => { throw new Error('factory failed'); }), /factory failed/);
  manager.deleteCache('get-or-set-async');
});

test('API - key length limit still counts UTF-8 bytes', () => {
  const manager = new CacheManager();
  const cache = manager.createCache('key-limit');

  cache.set('a'.repeat(8192), 1);
  assert.throws(() => cache.set('a'.repeat(8193), 1), RangeError);
  assert.throws(() => cache.set('é'.repeat(4097), 1), RangeError); // 8194 bytes, 4097 code units
  manager.deleteCache('key-limit');
});
