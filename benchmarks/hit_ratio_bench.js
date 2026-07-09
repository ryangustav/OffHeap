const { CacheManager } = require('../index.js');

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

function runPolicy(policy, capacity, accessSequence) {
  const manager = new CacheManager();
  const cache = manager.createCache('hit-ratio-test', {
    policy,
    capacity,
    shards: 1, // Single shard to isolate eviction logic correctly
    l1Capacity: 0 // Disable L1 to focus exclusively on L2 policy hit-ratio
  });

  let hits = 0;
  let misses = 0;

  // Cache is populated dynamically during reads (miss -> fetch -> write)
  for (const item of accessSequence) {
    const key = `key-${item}`;
    const val = cache.get(key);
    if (val !== undefined) {
      hits++;
    } else {
      misses++;
      cache.set(key, 'payload');
    }
  }

  const stats = cache.stats();
  cache.dispose();
  manager.dispose();

  const ratio = (hits / accessSequence.length) * 100;
  return { policy, hits, misses, ratio };
}

async function run() {
  const cacheCapacity = 1000;
  const uniqueItems = 5000; // 5x cache capacity
  const zipfSkew = 0.9;
  const totalOps = 100000;

  console.log(`============================================================`);
  console.log(`📊 EVICTION POLICY HIT-RATIO BENCHMARK`);
  console.log(`============================================================`);
  console.log(`Cache Capacity  : ${cacheCapacity} entries`);
  console.log(`Unique Keys     : ${uniqueItems} (Zipfian space)`);
  console.log(`Zipf Skew (s)   : ${zipfSkew} (Realistic popularity model)`);
  console.log(`Total Operations: ${totalOps}`);
  console.log(`------------------------------------------------------------`);

  console.log('Generating access sequence...');
  const zipf = new FastZipf(uniqueItems, zipfSkew);
  const accessSequence = new Int32Array(totalOps);
  for (let i = 0; i < totalOps; i++) {
    accessSequence[i] = zipf.next();
  }

  console.log('Running LRU policy...');
  const lru = runPolicy('lru', cacheCapacity, accessSequence);

  console.log('Running ARC policy...');
  const arc = runPolicy('arc', cacheCapacity, accessSequence);

  console.log('Running W-TinyLFU policy...');
  const tinylfu = runPolicy('tinylfu', cacheCapacity, accessSequence);

  console.log('\n========================= RESULTS =========================');
  console.log(`| Policy   | Hits     | Misses   | Hit Ratio (%) | VS LRU (Baseline) |`);
  console.log(`|----------|----------|----------|---------------|-------------------|`);
  
  const printRow = (res) => {
    const delta = res.policy === 'lru' 
      ? '-' 
      : `${(res.ratio - lru.ratio).toFixed(2)}%`;
    console.log(`| ${res.policy.toUpperCase().padEnd(8)} | ${String(res.hits).padEnd(8)} | ${String(res.misses).padEnd(8)} | ${res.ratio.toFixed(2).padEnd(13)}% | ${delta.padEnd(17)} |`);
  };

  printRow(lru);
  printRow(arc);
  printRow(tinylfu);
  console.log(`============================================================`);
}

run().catch(console.error);
