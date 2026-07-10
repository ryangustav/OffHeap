# Getting Started

```javascript
const { CacheManager } = require('offheap');

// 1. Initialize the Central Manager
const manager = new CacheManager();

// 2. Instantiate isolated caches with specific eviction policies
const productCache = manager.createCache('products', {
  eviction: {
    policy: 'w-tinylfu', // Options: 'lru', 'arc', 'w-tinylfu'
    capacity: 100000     // Sized for up to 100,000 items
  }
});

const sessionCache = manager.createCache('sessions', {
  eviction: {
    policy: 'lru',
    capacity: 20000
  }
});

// 3. Store and retrieve values (Preserves Buffers, Strings, and JSON Objects)
productCache.set('prod_1', { id: 1, name: 'Premium Widget', tags: ['fast'] });
const product = productCache.get('prod_1'); // Returns the parsed JS object

// 4. Set values with TTL (lazily expired)
sessionCache.set('session_token', 'user_123', 60000); // 60 seconds (60000 ms) TTL
```

## Automatic Serialization & Type Preservation

OffHeap dynamically adapts its serialization path at the FFI boundary to maximize performance and save memory. You do not need to manually stringify objects or convert buffers to strings:

```javascript
// 1. Binary Data (zero-copy buffer allocation)
cache.set('binary_data', fs.readFileSync('image.png'));
const buffer = cache.get('binary_data'); // Returns a Node.js Buffer

// 2. Plain Text / Strings
cache.set('text_data', 'Hello World!');
const message = cache.get('text_data'); // Returns a standard JS string

// 3. Structured JSON (objects, arrays, numbers, booleans)
cache.set('json_data', { username: 'ryangustavo', role: 'admin' });
const profile = cache.get('json_data'); // Returns the deserialized JS Object

// 4. Atomic Counters (high-efficiency integer values)
cache.set('page_views', 100);
cache.increment('page_views', 1); // Returns 101 without JSON overhead
```

## Introduction

OffHeap is a high-performance, in-process, off-heap caching framework for Node.js written from scratch in **Rust** using **NAPI-RS**. By moving cached values out of the V8 JavaScript engine heap and into Rust memory space, OffHeap prevents V8 Garbage Collection (GC) sweeps from scanning the cache graphs. This layout eliminates Stop-the-World GC latency spikes under high retention workloads.

Unlike traditional Node.js cache engines that store values as JavaScript objects, OffHeap serializes values and stores them as raw bytes in Rust memory. When data is requested, it is transferred across the Foreign Function Interface (FFI) boundary and deserialized back into its original type. The framework supports raw binary Buffers, text Strings, and complex JSON Objects, choosing the most efficient serialization path based on the type.

OffHeap is built using an architecture where you instantiate isolated, named `Cache` instances from a single central `CacheManager`. This design lets you run multiple caches under different eviction policies—including classic Least Recently Used (LRU), Adaptive Replacement Cache (ARC), and Window TinyLFU (W-TinyLFU)—providing optimal hit ratios tailored to each microservice's access pattern.

## Installation

Install the package using your preferred Node.js package manager:

```bash
# npm
npm install offheap

# yarn
yarn add offheap

# pnpm
pnpm add offheap
```

> [!NOTE]
> The package automatically downloads pre-built native binaries compiled for your target operating system and architecture (Windows, macOS, and Linux) during installation. No local C++ compiler or Rust toolchain is required for end-users.
