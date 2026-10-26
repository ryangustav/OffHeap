const { Bench } = require('tinybench');
const Redis = require('ioredis');
const { CacheManager } = require('../index.js');

async function runL3Benchmarks() {
  console.log('====================================================');
  console.log('🏆 L3 REDIS CACHE THROUGHPUT BENCHMARKS');
  console.log('====================================================');

  const client = new Redis();
  await client.flushall();
  const manager = new CacheManager({
    redis: { client, keyPrefix: 'bench:' }
  });

  const cacheWithL3 = manager.createCache('l3-active');
  const cacheL2Only = manager.createCache('l3-l2-only', { l1Capacity: 0 });
  const cacheNoL3 = manager.createCache('l3-inactive');

  const key = 'bench-key';
  const val = { val: 'a'.repeat(200) };

  // Setup values
  cacheWithL3.set(key, val);
  cacheL2Only.set(key, val);
  cacheNoL3.set(key, val);

  // Setup simulated L3 remote key
  const payload = Buffer.concat([Buffer.from([2]), Buffer.from('\0J' + JSON.stringify(val), 'utf8')]);
  await client.set('bench:l3-active:remote-key', payload);

  // 1. Throughput of getAsync with L1 Hit vs L2 Hit vs L3 Hit
  const getAsyncBench = new Bench({ time: 1000 });
  getAsyncBench
    .add('getAsync - L1 Hit', async () => {
      await cacheWithL3.getAsync(key);
    })
    .add('getAsync - L2 Hit (L1 capacity = 0)', async () => {
      await cacheL2Only.getAsync(key);
    })
    .add('getAsync - L3 Hit (forced miss in L1/L2)', async () => {
      cacheWithL3._l1.delete('remote-key');
      cacheWithL3._native.delete('remote-key');
      await cacheWithL3.getAsync('remote-key');
    });

  console.log('\nEvaluating read path throughput (getAsync)...');
  await getAsyncBench.run();
  console.table(getAsyncBench.table());

  // 2. Throughput of set with vs without L3 (overhead of fire-and-forget)
  const setBench = new Bench({ time: 1000 });
  let idx = 0;
  setBench
    .add('set() - Without L3', () => {
      cacheNoL3.set(`key-${idx}`, val);
      idx++;
    })
    .add('set() - With L3 (fire-and-forget)', () => {
      cacheWithL3.set(`key-${idx}`, val);
      idx++;
    });

  console.log('\nEvaluating write path throughput (set write-through)...');
  await setBench.run();
  console.table(setBench.table());

  // Cleanup
  cacheL2Only.dispose();
  cacheWithL3.dispose();
  cacheNoL3.dispose();
  manager.deleteCache('l3-l2-only');
  manager.deleteCache('l3-active');
  manager.deleteCache('l3-inactive');
  manager.dispose();
  await client.quit();
}

runL3Benchmarks().catch(console.error);
