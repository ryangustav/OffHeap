const test = require('node:test');
const assert = require('node:assert');
const MockRedis = require('ioredis-mock');
const { CacheManager } = require('../index.js');

test('Redis L3 - basic operations when disabled', async () => {
  const manager = new CacheManager();
  const cache = manager.createCache('no-redis-basic', { policy: 'lru', capacity: 10 });

  cache.set('key', 'val');
  assert.strictEqual(cache.get('key'), 'val');
  assert.strictEqual(await cache.getAsync('key'), 'val');
  assert.strictEqual(cache.stats().l3, null);

  manager.deleteCache('no-redis-basic');
});

test('Redis L3 - getAsync hit and back-populate', async () => {
  const client = new MockRedis();
  await client.flushall();
  const manager = new CacheManager({
    redis: { client, keyPrefix: 'test:' }
  });
  const cache = manager.createCache('test-cache-hit');

  // Populate L3 manually to simulate remote entry
  // Tag 2 signifies UTF-8 string format in JS wrapper
  const payload = Buffer.concat([Buffer.from([2]), Buffer.from('hello', 'utf8')]);
  await client.set('test:test-cache-hit:key', payload, 'PX', 5000);

  // Initial miss in L1 and L2
  assert.strictEqual(cache.get('key'), undefined);

  // getAsync retrieves it from L3 and back-populates
  const val = await cache.getAsync('key');
  assert.strictEqual(val, 'hello');

  // Now it must be in L2 (L1 hit)
  assert.strictEqual(cache.get('key'), 'hello');

  // Check stats
  const stats = cache.stats();
  assert.strictEqual(stats.l3.connected, true);

  manager.deleteCache('test-cache-hit');
});

test('Redis L3 - getAsync miss', async () => {
  const client = new MockRedis();
  await client.flushall();
  const manager = new CacheManager({
    redis: { client, keyPrefix: 'test:' }
  });
  const cache = manager.createCache('test-cache-miss');

  const val = await cache.getAsync('nonexistent');
  assert.strictEqual(val, undefined);

  manager.deleteCache('test-cache-miss');
});

test('Redis L3 - set write-through and delete propagation', async () => {
  const client = new MockRedis();
  await client.flushall();
  const manager = new CacheManager({
    redis: { client, keyPrefix: 'test:' }
  });
  const cache = manager.createCache('test-cache-write-through');

  cache.set('key', 'hello-world', 10000);

  // Verify L3 has the value with type tag 2 (string)
  const rawBytes = await client.getBuffer('test:test-cache-write-through:key');
  assert.ok(rawBytes);
  assert.strictEqual(rawBytes[0], 2);
  assert.strictEqual(rawBytes.subarray(1).toString('utf8'), 'hello-world');

  // Verify TTL on Redis
  const ttl = await client.pttl('test:test-cache-write-through:key');
  assert.ok(ttl > 0 && ttl <= 10000);

  // Propagate deletion
  cache.delete('key');
  const rawBytesAfter = await client.getBuffer('test:test-cache-write-through:key');
  assert.strictEqual(rawBytesAfter, null);

  manager.deleteCache('test-cache-write-through');
});

test('Redis L3 - getOrSet L3 hit (factory not called)', async () => {
  const client = new MockRedis();
  await client.flushall();
  const manager = new CacheManager({
    redis: { client, keyPrefix: 'test:' }
  });
  const cache = manager.createCache('test-cache-getorset-l3-hit');

  const payload = Buffer.concat([Buffer.from([2]), Buffer.from('hello-redis', 'utf8')]);
  await client.set('test:test-cache-getorset-l3-hit:key', payload);

  let factoryCalled = 0;
  const res = await cache.getOrSet('key', () => {
    factoryCalled++;
    return 'computed';
  });

  assert.strictEqual(res, 'hello-redis');
  assert.strictEqual(factoryCalled, 0);

  manager.deleteCache('test-cache-getorset-l3-hit');
});

test('Redis L3 - getOrSet concurrent stampede coalescing', async () => {
  const client = new MockRedis();
  await client.flushall();
  const manager = new CacheManager({
    redis: { client, keyPrefix: 'test:' }
  });
  const cache = manager.createCache('test-cache-getorset-coalesce', { l1Capacity: 0 }); // disable L1 for pure L2/L3 testing

  // Spy on client.pipeline
  let pipelineCount = 0;
  const originalPipeline = client.pipeline.bind(client);
  client.pipeline = function(...args) {
    pipelineCount++;
    return originalPipeline(...args);
  };

  let factoryCount = 0;
  const factory = () => {
    factoryCount++;
    return new Promise(resolve => setTimeout(() => resolve('computed-value'), 20));
  };

  // Trigger 5 concurrent requests
  const results = await Promise.all([
    cache.getOrSet('key', factory),
    cache.getOrSet('key', factory),
    cache.getOrSet('key', factory),
    cache.getOrSet('key', factory),
    cache.getOrSet('key', factory)
  ]);

  // All results must be identical
  results.forEach(r => assert.strictEqual(r, 'computed-value'));

  // Pipeline GET (Redis check) and factory must only be called once!
  assert.strictEqual(pipelineCount, 1);
  assert.strictEqual(factoryCount, 1);

  manager.deleteCache('test-cache-getorset-coalesce');
});

test('Redis L3 - getOrSet factory populates L3', async () => {
  const client = new MockRedis();
  await client.flushall();
  const manager = new CacheManager({
    redis: { client, keyPrefix: 'test:' }
  });
  const cache = manager.createCache('test-cache-getorset-factory');

  const res = await cache.getOrSet('key', () => 'computed', 5000);
  assert.strictEqual(res, 'computed');

  // Verify L3 got populated
  const rawBytes = await client.getBuffer('test:test-cache-getorset-factory:key');
  assert.ok(rawBytes);
  assert.strictEqual(rawBytes[0], 2);
  assert.strictEqual(rawBytes.subarray(1).toString('utf8'), 'computed');

  manager.deleteCache('test-cache-getorset-factory');
});

test('Redis L3 - namespace isolation', async () => {
  const client = new MockRedis();
  await client.flushall();
  const manager = new CacheManager({
    redis: { client, keyPrefix: 'test:' }
  });
  const cacheUsers = manager.createCache('users-namespace');
  const cacheSessions = manager.createCache('sessions-namespace');

  cacheUsers.set('123', 'user-data');
  cacheSessions.set('123', 'session-data');

  assert.ok(await client.exists('test:users-namespace:123'));
  assert.ok(await client.exists('test:sessions-namespace:123'));

  const rawUsers = await client.getBuffer('test:users-namespace:123');
  const rawSessions = await client.getBuffer('test:sessions-namespace:123');
  assert.strictEqual(rawUsers.subarray(1).toString('utf8'), 'user-data');
  assert.strictEqual(rawSessions.subarray(1).toString('utf8'), 'session-data');

  manager.deleteCache('users-namespace');
  manager.deleteCache('sessions-namespace');
});

test('Redis L3 - PTTL edge cases', async () => {
  const client = new MockRedis();
  await client.flushall();
  const manager = new CacheManager({
    redis: { client, keyPrefix: 'test:' }
  });
  const cache = manager.createCache('test-cache-pttl');

  // Subcase 1: PTTL = -1 (key exists but has no TTL)
  const payload1 = Buffer.concat([Buffer.from([2]), Buffer.from('val-no-ttl', 'utf8')]);
  await client.set('test:test-cache-pttl:key-no-ttl', payload1);
  const val1 = await cache.getAsync('key-no-ttl');
  assert.strictEqual(val1, 'val-no-ttl');
  // In native L2 cache, it should not have expired
  assert.strictEqual(cache.get('key-no-ttl'), 'val-no-ttl');

  // Subcase 2: PTTL = -2 (key expired in Redis between GET and PTTL)
  const originalPipeline = client.pipeline.bind(client);
  client.pipeline = function() {
    const pipe = originalPipeline();
    const originalExec = pipe.exec.bind(pipe);
    pipe.exec = async function() {
      const results = await originalExec();
      // results structure: [[null, buffer], [null, pttl]]
      if (results[1]) {
        results[1][1] = -2; // override pttl result to simulate expired key
      }
      return results;
    };
    return pipe;
  };

  const payload2 = Buffer.concat([Buffer.from([2]), Buffer.from('expired-val', 'utf8')]);
  await client.set('test:test-cache-pttl:key-expired', payload2);
  const val2 = await cache.getAsync('key-expired');
  assert.strictEqual(val2, undefined); // treated as miss

  manager.deleteCache('test-cache-pttl');
});

test('Redis L3 - graceful degradation on read/write errors', async () => {
  const client = new MockRedis();
  await client.flushall();
  
  let errorCount = 0;
  let lastOp = '';
  const manager = new CacheManager({
    redis: {
      client,
      keyPrefix: 'test:',
      onError: (err, op, key) => {
        errorCount++;
        lastOp = op;
      }
    }
  });
  const cache = manager.createCache('test-cache-errors');

  // Force get/set/del to throw on client to mock network failures
  client.pipeline = function() {
    throw new Error('Redis connection lost');
  };
  client.set = function() {
    throw new Error('Redis write failed');
  };
  client.del = function() {
    throw new Error('Redis delete failed');
  };

  // 1. getAsync degradation (treated as miss, no crash)
  const val = await cache.getAsync('key');
  assert.strictEqual(val, undefined);
  assert.strictEqual(errorCount, 1);
  assert.strictEqual(lastOp, 'get');

  // 2. set degradation (writes locally, fire-and-forget fails silently, no crash)
  cache.set('key', 'local-value');
  assert.strictEqual(cache.get('key'), 'local-value');
  assert.strictEqual(errorCount, 2);
  assert.strictEqual(lastOp, 'set');

  // 3. delete degradation (deletes locally, L3 fails silently)
  cache.delete('key');
  assert.strictEqual(errorCount, 3);
  assert.strictEqual(lastOp, 'delete');

  manager.deleteCache('test-cache-errors');
});

test('Redis L3 - binary tag preservation (JSON, Buffer, String, Number)', async () => {
  const client = new MockRedis();
  await client.flushall();
  const manager = new CacheManager({
    redis: { client, keyPrefix: 'test:' }
  });
  const cache = manager.createCache('test-cache-binary');

  // 1. JSON Object
  const obj = { x: 42, y: 'hello' };
  cache.set('obj', obj);
  assert.deepStrictEqual(await cache.getAsync('obj'), obj);

  // 2. Buffer
  const buf = Buffer.from([10, 20, 30]);
  cache.set('buf', buf);
  const resBuf = await cache.getAsync('buf');
  assert.ok(Buffer.isBuffer(resBuf));
  assert.deepStrictEqual(resBuf, buf);

  // 3. Number
  cache.set('num', 3.14);
  assert.strictEqual(await cache.getAsync('num'), 3.14);

  manager.deleteCache('test-cache-binary');
});

test('Redis L3 - mset write-through and mdelete propagation', async () => {
  const client = new MockRedis();
  await client.flushall();
  const manager = new CacheManager({ redis: { client, keyPrefix: 'test:' } });
  const cache = manager.createCache('test-batch-l3');

  cache.mset({ a: 'x', b: { n: 1 } }, 10000);
  await new Promise((resolve) => setImmediate(resolve));
  assert.ok(await client.getBuffer('test:test-batch-l3:a'));
  assert.ok(await client.getBuffer('test:test-batch-l3:b'));
  const ttl = await client.pttl('test:test-batch-l3:a');
  assert.ok(ttl > 0 && ttl <= 10000);

  cache.mdelete(['a', 'b']);
  await new Promise((resolve) => setImmediate(resolve));
  assert.strictEqual(await client.getBuffer('test:test-batch-l3:a'), null);
  assert.strictEqual(await client.getBuffer('test:test-batch-l3:b'), null);

  manager.deleteCache('test-batch-l3');
});

test('Redis L3 - increment invalidates the stale L3 copy', async () => {
  const client = new MockRedis();
  await client.flushall();
  const manager = new CacheManager({ redis: { client, keyPrefix: 'test:' } });
  const cache = manager.createCache('test-counter-l3');

  cache.set('n', 1);
  await new Promise((resolve) => setImmediate(resolve));
  assert.ok(await client.getBuffer('test:test-counter-l3:n'));

  assert.strictEqual(cache.increment('n', 5), 6);
  await new Promise((resolve) => setImmediate(resolve));
  assert.strictEqual(await client.getBuffer('test:test-counter-l3:n'), null);

  manager.deleteCache('test-counter-l3');
});

test('Redis L3 - back-population reports L2 evictions to hooks', async () => {
  const client = new MockRedis();
  await client.flushall();
  const evicted = [];
  const manager = new CacheManager({ redis: { client, keyPrefix: 'test:' } });
  const cache = manager.createCache('test-backfill-evict', {
    policy: 'lru',
    capacity: 1,
    shards: 1,
    l1: { enabled: false },
    hooks: { onEvict: (key) => evicted.push(key) },
  });

  cache.set('old', 'value');
  const payload = Buffer.concat([Buffer.from([2]), Buffer.from('remote', 'utf8')]);
  await client.set('test:test-backfill-evict:remote', payload);

  assert.strictEqual(await cache.getAsync('remote'), 'remote');
  assert.deepStrictEqual(evicted, ['old']);

  manager.deleteCache('test-backfill-evict');
});

test('Redis L3 - strings starting with "\0" survive the L3 round-trip', async () => {
  const client = new MockRedis();
  await client.flushall();
  const manager = new CacheManager({ redis: { client, keyPrefix: 'test:' } });
  const writer = manager.createCache('test-nul-l3');
  const spoofed = '\0J{"admin":true}';

  writer.set('k', spoofed);
  await new Promise((resolve) => setImmediate(resolve));
  writer.clear(); // force the next read to come from L3

  assert.strictEqual(await writer.getAsync('k'), spoofed);
  manager.deleteCache('test-nul-l3');
});
