const { execSync } = require('child_process');

console.log('=== RUNNING BUFFER MEMORY FOOTPRINT COMPARISON (500k Keys, 500B Binary Payloads) ===');
console.log('Running JS lru-cache memory test...');
const lru = JSON.parse(execSync('node --expose-gc benchmarks/buffer_rss_child.js lru').toString().trim());

console.log('Running OffHeap memory test...');
const offheap = JSON.parse(execSync('node --expose-gc benchmarks/buffer_rss_child.js offheap').toString().trim());

console.log('\n========================================================================');
console.log('📊 BUFFER STORAGE MEMORY COMPARISON RESULTS');
console.log('========================================================================');
console.log('| Metric             | JS lru-cache (In-Heap) | OffHeap (L2 Native)      |');
console.log('|--------------------|------------------------|--------------------------|');
console.log(`| Heap Usage (End)   | ${lru.endHeap.padEnd(22)} | ${offheap.endHeap.padEnd(24)} |`);
console.log(`| RSS Memory (Start) | ${lru.startRss.padEnd(22)} | ${offheap.startRss.padEnd(24)} |`);
console.log(`| RSS Memory (End)   | ${lru.endRss.padEnd(22)} | ${offheap.endRss.padEnd(24)} |`);
console.log(`| RSS Memory Delta   | ${lru.rssDelta.padEnd(22)} | ${offheap.rssDelta.padEnd(24)} |`);
console.log('========================================================================');
