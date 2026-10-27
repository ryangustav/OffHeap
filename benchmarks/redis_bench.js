/**
 * benchmarks/redis_bench.js
 *
 * Comparative benchmark: OffHeap vs Redis (via ioredis).
 *
 * Requirements:
 *   npm install --save-dev ioredis
 *   Redis running on localhost:6379
 *
 * Usage:
 *   node --expose-gc benchmarks/redis_bench.js
 */

'use strict';

const { Bench } = require('tinybench');
const { CacheManager } = require('../index.js');

let Redis;
try {
  Redis = require('ioredis');
} catch {
  console.error('\n❌ ioredis not installed. Run: npm install --save-dev ioredis\n');
  process.exit(1);
}

const ITERATIONS = 50_000;
const KEY_COUNT  = 10_000;

const redis = new Redis({ host: '127.0.0.1', port: 6379, lazyConnect: true });

const manager = new CacheManager();
const offheap = manager.createCache('bench', {
  policy: 'w-tinylfu',
  capacity: KEY_COUNT,
  shards: 8,
});

const keys   = Array.from({ length: KEY_COUNT }, (_, i) => `key:${i}`);
const values = Array.from({ length: KEY_COUNT }, (_, i) => ({ id: i, data: 'x'.repeat(64) }));

async function run() {
  try {
    await redis.connect();
  } catch (err) {
    console.error('\n❌ Could not connect to Redis at localhost:6379');
    console.error('   Make sure Redis is running: redis-server');
    process.exit(1);
  }

  console.log('⏳ Pre-warming caches...');
  const pipeline = redis.pipeline();
  for (let i = 0; i < KEY_COUNT; i++) {
    offheap.set(keys[i], values[i]);
    pipeline.set(keys[i], JSON.stringify(values[i]));
  }
  await pipeline.exec();
  console.log('✅ Pre-warm complete\n');

  // ── Suite 1: SET throughput ──────────────────────────────────────────────
  const setSuite = new Bench({ iterations: ITERATIONS });

  setSuite.add('OffHeap SET (w-tinylfu)', () => {
    const i = (Math.random() * KEY_COUNT) | 0;
    offheap.set(keys[i], values[i]);
  });

  setSuite.add('Redis  SET (single)', async () => {
    const i = (Math.random() * KEY_COUNT) | 0;
    await redis.set(keys[i], JSON.stringify(values[i]));
  });

  // ── Suite 2: GET throughput (hot cache) ──────────────────────────────────
  const getSuite = new Bench({ iterations: ITERATIONS });

  getSuite.add('OffHeap GET (hit)  ', () => {
    const i = (Math.random() * KEY_COUNT) | 0;
    offheap.get(keys[i]);
  });

  getSuite.add('Redis  GET (single)', async () => {
    const i = (Math.random() * KEY_COUNT) | 0;
    const raw = await redis.get(keys[i]);
    if (raw) JSON.parse(raw);
  });

  // ── Suite 3: Mixed 80% reads / 20% writes ────────────────────────────────
  const mixedSuite = new Bench({ iterations: ITERATIONS });
  let mixedIdx = 0;

  mixedSuite.add('OffHeap Mixed 80/20', () => {
    const i = mixedIdx++ % KEY_COUNT;
    if (i % 5 === 0) offheap.set(keys[i], values[i]);
    else offheap.get(keys[i]);
  });

  mixedSuite.add('Redis  Mixed 80/20 ', async () => {
    const i = mixedIdx++ % KEY_COUNT;
    if (i % 5 === 0) {
      await redis.set(keys[i], JSON.stringify(values[i]));
    } else {
      const raw = await redis.get(keys[i]);
      if (raw) JSON.parse(raw);
    }
  });

  // ── Suite 4: Batch mget (10 keys per call) ───────────────────────────────
  const batchSuite = new Bench({ iterations: Math.floor(ITERATIONS / 10) });
  const batchKeys = keys.slice(0, 10);

  batchSuite.add('OffHeap mget(10)   ', () => {
    offheap.mget(batchKeys);
  });

  batchSuite.add('Redis  mget(10)    ', async () => {
    const raws = await redis.mget(...batchKeys);
    raws.forEach(r => r && JSON.parse(r));
  });

  // ── Run ──────────────────────────────────────────────────────────────────
  const suites = [
    { label: 'SET Throughput', bench: setSuite },
    { label: 'GET Throughput (hot cache)', bench: getSuite },
    { label: 'Mixed 80% read / 20% write', bench: mixedSuite },
    { label: 'Batch mget (10 keys)', bench: batchSuite },
  ];

  for (const { label, bench } of suites) {
    console.log(`\n🔥 ${label}`);
    console.log('─'.repeat(62));
    await bench.run();

    for (const task of bench.tasks) {
      const ops = Math.round(task.result.hz).toLocaleString();
      const avg = (task.result.mean * 1000).toFixed(3);
      console.log(`  ${task.name.padEnd(24)} ${ops.padStart(12)} ops/sec   avg ${avg}ms`);
    }
  }

  // ── Memory comparison ─────────────────────────────────────────────────────
  console.log('\n📊 Memory Footprint');
  console.log('─'.repeat(62));

  const st = offheap.stats();
  const redisInfo = await redis.info('memory');
  const match = redisInfo.match(/used_memory:(\d+)/);
  const redisUsed = match ? parseInt(match[1]) : null;

  console.log(`  OffHeap payload bytes:  ${(st.bytesUsed / 1024).toFixed(1)} KB`);
  console.log(`  OffHeap process RSS:    ${(st.memory.processRss / 1024 / 1024).toFixed(1)} MB`);
  if (redisUsed !== null) {
    console.log(`  Redis  used_memory:     ${(redisUsed / 1024).toFixed(1)} KB`);
  }

  // ── Cleanup ───────────────────────────────────────────────────────────────
  await redis.flushdb();
  await redis.quit();
  offheap.dispose();
  manager.dispose();

  console.log('\n✅ Benchmark complete');
}

run().catch(err => {
  console.error('Benchmark failed:', err);
  process.exit(1);
});
