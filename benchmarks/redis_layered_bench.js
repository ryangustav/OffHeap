const { execSync } = require('child_process');
const path = require('path');

const CONFIGS = [
  'lru-cache',
  'l1-only',
  'l1-l2',
  'l1-l2-l3-idle',
  'l1-l2-l3-active'
];

const CONFIG_LABELS = {
  'lru-cache': 'JS lru-cache (NPM)',
  'l1-only': 'OffHeap L1-Only (In-Heap)',
  'l1-l2': 'OffHeap L1+L2 (Hybrid Control)',
  'l1-l2-l3-idle': 'OffHeap L1+L2+L3 (L3 Idle)',
  'l1-l2-l3-active': 'OffHeap L1+L2+L3 (L3 Active)'
};

const BATTERIES = ['gc-pressure', 'throughput', 'hit-ratio'];
const ROUNDS = 3;

function shuffle(array) {
  const arr = [...array];
  for (let i = arr.length - 1; i > 0; i--) {
    const j = Math.floor(Math.random() * (i + 1));
    [arr[i], arr[j]] = [arr[j], arr[i]];
  }
  return arr;
}

function parseMemoryMB(str) {
  return parseFloat(str.replace(' MB', ''));
}

async function runBenchmark() {
  console.log('========================================================================');
  console.log('🏁 STARTING RIGOROUS LAYERED REDIS INTEGRATION BENCHMARKS');
  console.log(`Running ${ROUNDS} rounds in randomized order to eliminate resource scheduling bias...`);
  console.log('========================================================================\n');

  const rawResults = {};
  for (const config of CONFIGS) {
    rawResults[config] = {};
    for (const battery of BATTERIES) {
      rawResults[config][battery] = [];
    }
  }

  // Execute rounds
  for (let round = 1; round <= ROUNDS; round++) {
    console.log(`\n--- ROUND ${round} / ${ROUNDS} ---`);
    for (const battery of BATTERIES) {
      console.log(`Running Battery: [${battery}]`);
      const shuffledConfigs = shuffle(CONFIGS);
      for (const config of shuffledConfigs) {
        process.stdout.write(`  -> Running ${CONFIG_LABELS[config]}... `);
        try {
          const scriptPath = path.join(__dirname, 'redis_layered_child.js');
          const output = execSync(`node --expose-gc "${scriptPath}" ${config} ${battery}`, {
            stdio: ['pipe', 'pipe', 'ignore'],
            env: { ...process.env, NODE_ENV: 'production' }
          }).toString().trim();
          
          const data = JSON.parse(output);
          rawResults[config][battery].push(data);
          console.log(`✅ Done (Ops/sec: ${data.opsPerSec})`);
        } catch (err) {
          console.log(`❌ Failed!`);
          console.error(err.message);
        }
      }
    }
  }

  // Aggregate Averages
  const averages = {};
  for (const config of CONFIGS) {
    averages[config] = {};
    for (const battery of BATTERIES) {
      const runs = rawResults[config][battery];
      if (runs.length === 0) continue;

      const sum = {
        duration: 0,
        opsPerSec: 0,
        avgLat: 0,
        p50: 0,
        p90: 0,
        p99: 0,
        p99_9: 0,
        gcCount: 0,
        totalGcTime: 0,
        maxGcTime: 0,
        startHeap: 0,
        endHeap: 0,
        startRss: 0,
        endRss: 0,
        rssDelta: 0,
        hitRatio: 0
      };

      for (const run of runs) {
        sum.duration += run.duration;
        sum.opsPerSec += run.opsPerSec;
        sum.avgLat += parseFloat(run.avgLat);
        sum.p50 += parseFloat(run.p50);
        sum.p90 += parseFloat(run.p90);
        sum.p99 += parseFloat(run.p99);
        sum.p99_9 += parseFloat(run.p99_9);
        sum.gcCount += run.gcCount;
        sum.totalGcTime += parseFloat(run.totalGcTime);
        sum.maxGcTime += parseFloat(run.maxGcTime);
        sum.startHeap += parseMemoryMB(run.startHeap);
        sum.endHeap += parseMemoryMB(run.endHeap);
        sum.startRss += parseMemoryMB(run.startRss);
        sum.endRss += parseMemoryMB(run.endRss);
        sum.rssDelta += parseMemoryMB(run.rssDelta);
        if (run.hitRatio !== 'N/A') {
          sum.hitRatio += parseFloat(run.hitRatio);
        }
      }

      const count = runs.length;
      averages[config][battery] = {
        duration: Math.round(sum.duration / count),
        opsPerSec: Math.round(sum.opsPerSec / count),
        avgLat: (sum.avgLat / count).toFixed(1),
        p50: (sum.p50 / count).toFixed(1),
        p90: (sum.p90 / count).toFixed(1),
        p99: (sum.p99 / count).toFixed(1),
        p99_9: (sum.p99_9 / count).toFixed(1),
        gcCount: Math.round(sum.gcCount / count),
        totalGcTime: (sum.totalGcTime / count).toFixed(1),
        maxGcTime: (sum.maxGcTime / count).toFixed(1),
        startHeap: (sum.startHeap / count).toFixed(2) + ' MB',
        endHeap: (sum.endHeap / count).toFixed(2) + ' MB',
        startRss: (sum.startRss / count).toFixed(2) + ' MB',
        endRss: (sum.endRss / count).toFixed(2) + ' MB',
        rssDelta: (sum.rssDelta / count).toFixed(2) + ' MB',
        hitRatio: runs[0].hitRatio !== 'N/A' ? (sum.hitRatio / count).toFixed(2) + '%' : 'N/A'
      };
    }
  }

  // Print Consolidated Reports
  console.log('\n\n========================================================================');
  console.log('📊 CONSOLIDATED BENCHMARK REPORT (AVERAGES OVER ' + ROUNDS + ' ROUNDS)');
  console.log('========================================================================');

  // REPORT 1: GC Pressure
  console.log('\n--- 🔋 BATTERY: GC PRESSURE & MEMORY IMPACT (100k mixed ops under load) ---');
  console.log('| Configuration | RSS Delta | Heap (End) | GC Sweeps | GC Time (Total) | GC Worst Pause | Worst Latency (p99.9) |');
  console.log('| :--- | :--- | :--- | :--- | :--- | :--- | :--- |');
  for (const config of CONFIGS) {
    const r = averages[config]['gc-pressure'];
    console.log(`| ${CONFIG_LABELS[config].padEnd(30)} | ${r.rssDelta.padEnd(9)} | ${r.endHeap.padEnd(10)} | ${String(r.gcCount).padEnd(9)} | ${(r.totalGcTime + ' ms').padEnd(15)} | ${(r.maxGcTime + ' ms').padEnd(14)} | ${(r.p99_9 + ' μs').padEnd(20)} |`);
  }

  // REPORT 2: Throughput & Latency
  console.log('\n--- ⚡ BATTERY: THROUGHPUT & LATENCY (50k dense read/write ops) ---');
  console.log('| Configuration | Throughput | Avg Latency | p50 (Median) | p90 | p99 | p99.9 |');
  console.log('| :--- | :--- | :--- | :--- | :--- | :--- | :--- |');
  for (const config of CONFIGS) {
    const r = averages[config]['throughput'];
    console.log(`| ${CONFIG_LABELS[config].padEnd(30)} | ${(r.opsPerSec + ' ops/s').padEnd(10)} | ${(r.avgLat + ' μs').padEnd(11)} | ${(r.p50 + ' μs').padEnd(12)} | ${(r.p90 + ' μs').padEnd(7)} | ${(r.p99 + ' μs').padEnd(7)} | ${(r.p99_9 + ' μs').padEnd(7)} |`);
  }

  // REPORT 3: Hit Ratio
  console.log('\n--- 📊 BATTERY: ZIPFIAN HIT RATIO (50k access sequences, 50k unique space) ---');
  console.log('| Configuration | Hit Ratio | Avg Latency | p99 Latency |');
  console.log('| :--- | :--- | :--- | :--- |');
  for (const config of CONFIGS) {
    const r = averages[config]['hit-ratio'];
    console.log(`| ${CONFIG_LABELS[config].padEnd(30)} | ${r.hitRatio.padEnd(9)} | ${(r.avgLat + ' μs').padEnd(11)} | ${(r.p99 + ' μs').padEnd(11)} |`);
  }

  // Print Analytical Comparisons
  console.log('\n\n========================================================================');
  console.log('🧠 SCENARIO PAIR-TO-PAIR ANALYTICAL COMPARISONS');
  console.log('========================================================================');

  // Comparison 1: (2) vs (3) "Does simply connecting Redis idle-wise cost anything?"
  const c2 = averages['l1-l2'];
  const c3 = averages['l1-l2-l3-idle'];
  console.log('\n🔍 (2) vs (3) -> "Does having Redis instantiated & connected idle cost memory/GC even if hitting L2?"');
  const rssDiff = (parseMemoryMB(c3['gc-pressure'].rssDelta) - parseMemoryMB(c2['gc-pressure'].rssDelta)).toFixed(2);
  const gcTimeDiff = (parseFloat(c3['gc-pressure'].totalGcTime) - parseFloat(c2['gc-pressure'].totalGcTime)).toFixed(1);
  const latPct = ((parseFloat(c3['throughput'].avgLat) - parseFloat(c2['throughput'].avgLat)) / parseFloat(c2['throughput'].avgLat) * 100).toFixed(1);
  console.log(`   - RSS Delta Difference   : ${rssDiff > 0 ? '+' : ''}${rssDiff} MB (${CONFIG_LABELS['l1-l2-l3-idle']} vs ${CONFIG_LABELS['l1-l2']})`);
  console.log(`   - Total GC Time Delta    : ${gcTimeDiff > 0 ? '+' : ''}${gcTimeDiff} ms`);
  console.log(`   - Avg Latency overhead   : ${latPct > 0 ? '+' : ''}${latPct}% on throughput path`);
  console.log(`   - Insight: Idle Redis client keeps sockets & buffers in V8 heap, which affects V8 memory footprint slightly.`);

  // Comparison 2: (3) vs (4) "What is the active cost of exercising L3 network/serialization?"
  const c4 = averages['l1-l2-l3-active'];
  console.log('\n🔍 (3) vs (4) -> "What is the physical cost of actually exercising L3 Redis (misses) vs Idle presence?"');
  const activeRssDiff = (parseMemoryMB(c4['gc-pressure'].rssDelta) - parseMemoryMB(c3['gc-pressure'].rssDelta)).toFixed(2);
  const activeGcTimeDiff = (parseFloat(c4['gc-pressure'].totalGcTime) - parseFloat(c3['gc-pressure'].totalGcTime)).toFixed(1);
  const tputRatio = (c3['throughput'].opsPerSec / c4['throughput'].opsPerSec).toFixed(1);
  console.log(`   - RSS Delta Difference   : ${activeRssDiff > 0 ? '+' : ''}${activeRssDiff} MB`);
  console.log(`   - Total GC Time Delta    : ${activeGcTimeDiff > 0 ? '+' : ''}${activeGcTimeDiff} ms`);
  console.log(`   - Throughput Penalty     : ${CONFIG_LABELS['l1-l2-l3-idle']} is ${tputRatio}x faster than ${CONFIG_LABELS['l1-l2-l3-active']}`);
  console.log(`   - Insight: Active serialization/deserialization & pipeline client work directly increases V8 allocations & FFI/network duration.`);

  // Comparison 3: (1) vs (2) "Sanity comparison of JS-only vs OffHeap control"
  const c1 = averages['l1-only'];
  console.log('\n🔍 (1) vs (2) -> "Sanity check: OffHeap Hybrid L1+L2 vs L1-Only In-Heap (No L2)"');
  const heapDiff = (parseMemoryMB(c1['gc-pressure'].endHeap) - parseMemoryMB(c2['gc-pressure'].endHeap)).toFixed(2);
  const gcCountDiff = c1['gc-pressure'].gcCount - c2['gc-pressure'].gcCount;
  console.log(`   - Heap Size saved by L2  : ${heapDiff} MB (L1-Only Heap: ${c1['gc-pressure'].endHeap} | Hybrid L1+L2 Heap: ${c2['gc-pressure'].endHeap})`);
  console.log(`   - GC Sweeps Count Delta  : ${gcCountDiff > 0 ? '+' : ''}${gcCountDiff} sweeps`);
  console.log(`   - Insight: By moving eviction pools off-heap into L2, OffHeap maintains low V8 heap pressure even with high keys count.`);
  console.log('========================================================================\n');
}

runBenchmark().catch(console.error);
