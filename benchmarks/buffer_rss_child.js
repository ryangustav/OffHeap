const { LRUCache } = require('lru-cache');
const { CacheManager } = require('../index.js');

const engine = process.argv[2]; // 'lru' or 'offheap'
if (!engine || (engine !== 'lru' && engine !== 'offheap')) {
  console.error('Usage: node buffer_rss_child.js [lru|offheap]');
  process.exit(1);
}

function formatMemory(bytes) {
  return (bytes / 1024 / 1024).toFixed(2) + ' MB';
}

async function run() {
  const numKeys = 500000;
  const keys = Array.from({ length: numKeys }, (_, i) => `key-${i}`);
  const payload = Buffer.alloc(500); // 500 bytes raw binary buffer

  global.gc && global.gc();
  const startHeap = process.memoryUsage().heapUsed;
  const startRss = process.memoryUsage().rss;

  let cache;
  let manager;
  
  if (engine === 'lru') {
    cache = new LRUCache({ max: numKeys });
    for (let i = 0; i < numKeys; i++) {
      cache.set(keys[i], Buffer.alloc(500));
    }
  } else {
    manager = new CacheManager();
    cache = manager.createCache('zipf-buffer-gc', {
      policy: 'lru',
      capacity: numKeys,
      shards: 16,
      l1Capacity: 0
    });
    for (let i = 0; i < numKeys; i++) {
      cache.set(keys[i], Buffer.alloc(500));
    }
  }

  // Wait briefly
  await new Promise((resolve) => setTimeout(resolve, 300));
  
  const endHeap = process.memoryUsage().heapUsed;
  const endRss = process.memoryUsage().rss;

  if (engine === 'lru') {
    cache.clear();
  } else {
    cache.dispose();
    manager.dispose();
  }
  
  global.gc && global.gc();

  const results = {
    startHeap: formatMemory(startHeap),
    endHeap: formatMemory(endHeap),
    startRss: formatMemory(startRss),
    endRss: formatMemory(endRss),
    rssDelta: formatMemory(endRss - startRss)
  };

  console.log(JSON.stringify(results));
}

run().catch(console.error);
