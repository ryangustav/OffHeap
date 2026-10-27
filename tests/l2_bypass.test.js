const test = require('node:test');
const assert = require('node:assert');
const { CacheManager } = require('../index.js');

test('L2 Bypass - basic get, set, delete', () => {
  const manager = new CacheManager();
  const cache = manager.createCache('l2-bypass-basic', {
    l2: { enabled: false },
    l1: { enabled: true },
    eviction: { capacity: 5 }
  });

  // Verify L2 disabled stats
  const statsBefore = cache.stats();
  assert.strictEqual(statsBefore.l2.enabled, false);
  assert.strictEqual(statsBefore.l1.capacity, 5); // Default to full capacity because L2 is disabled

  // Set & Get
  cache.set('key1', 'val1');
  assert.strictEqual(cache.get('key1'), 'val1');
  assert.strictEqual(cache.peek('key1'), 'val1');
  assert.strictEqual(cache.has('key1'), true);

  // Delete
  assert.strictEqual(cache.delete('key1'), true);
  assert.strictEqual(cache.get('key1'), undefined);
  assert.strictEqual(cache.delete('key1'), false);

  cache.dispose();
  manager.dispose();
});

test('L2 Bypass - L1 capacity eviction limits', () => {
  const manager = new CacheManager();
  const cache = manager.createCache('l2-bypass-eviction', {
    l2: { enabled: false },
    l1: { enabled: true },
    eviction: { capacity: 3 }
  });

  // Add more keys than capacity
  cache.set('k1', 1);
  cache.set('k2', 2);
  cache.set('k3', 3);
  cache.set('k4', 4); // should evict k1 (FIFO)

  assert.strictEqual(cache.get('k1'), undefined);
  assert.strictEqual(cache.get('k2'), 2);
  assert.strictEqual(cache.get('k3'), 3);
  assert.strictEqual(cache.get('k4'), 4);

  cache.dispose();
  manager.dispose();
});

test('L2 Bypass - increment and decrement (thread-local)', () => {
  const manager = new CacheManager();
  const cache = manager.createCache('l2-bypass-counters', {
    l2: { enabled: false },
    l1: { enabled: true },
    eviction: { capacity: 10 }
  });

  assert.strictEqual(cache.increment('counter', 5), 5);
  assert.strictEqual(cache.increment('counter', 3), 8);
  assert.strictEqual(cache.decrement('counter', 2), 6);
  assert.strictEqual(cache.get('counter'), 6);

  cache.dispose();
  manager.dispose();
});

test('L2 Bypass - batch operations (mget, mset, mdelete)', () => {
  const manager = new CacheManager();
  const cache = manager.createCache('l2-bypass-batch', {
    l2: { enabled: false },
    l1: { enabled: true },
    eviction: { capacity: 10 }
  });

  // mset
  cache.mset({ a: 'foo', b: 'bar', c: 'baz' });

  // mget
  const result = cache.mget(['a', 'b', 'd']);
  assert.deepStrictEqual(result, {
    a: 'foo',
    b: 'bar'
  });

  // mdelete
  assert.strictEqual(cache.mdelete(['a', 'b', 'd']), 2);
  assert.strictEqual(cache.get('a'), undefined);
  assert.strictEqual(cache.get('b'), undefined);
  assert.strictEqual(cache.get('c'), 'baz');

  cache.dispose();
  manager.dispose();
});

test('L2 Bypass - hooks onMiss', () => {
  const manager = new CacheManager();
  const misses = [];
  const cache = manager.createCache('l2-bypass-hooks', {
    l2: { enabled: false },
    l1: { enabled: true },
    hooks: {
      onMiss: (key) => misses.push(key)
    }
  });

  cache.get('nonexistent');
  cache.mget(['missing1', 'missing2']);

  assert.deepStrictEqual(misses, ['nonexistent', 'missing1', 'missing2']);

  cache.dispose();
  manager.dispose();
});
