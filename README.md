# OffHeap

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE-MIT)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](LICENSE-APACHE)

**OffHeap** is a high-performance, in-process, off-heap caching framework for Node.js written from scratch in **Rust** using **NAPI-RS**. It is designed to isolate cached items outside the V8 heap, eliminating V8 Garbage Collection (GC) sweeps and event loop Stop-the-World (STW) pauses.

---

## ⚡ The Architectural Pitch: Tail Latency over Raw Micro-ops

If your Node.js application caches only small amounts of data (under 50 MB) of tiny objects, a pure JavaScript Map or simple JS LRU cache will be faster. Crossing the Native Foreign Function Interface (FFI) boundary incurs a micro-latency of **0.5 to 1.0 microseconds** per operation.

However, once your cache holds **hundreds of thousands of records**, V8 is forced to scan millions of live heap references on every garbage collection sweep. This triggers **Stop-the-World event loop pauses** that block your entire application process. 

**OffHeap trades minor operation latency overhead for rock-solid, predictable tail latencies (p99/p99.9) and event loop freedom under heavy loads.**

---

## ⏱️ Garbage Collection & Memory Footprint Telemetry

All benchmarks below are process-isolated, statistically verified, and fully reproducible.

### 1. V8 GC Latency Pressure Test (500,000 Keys, 1,000,000 Operations)
This test simulates active request handling under memory pressure, comparing JS `lru-cache` against OffHeap Hybrid (L1+L2):

| Metric | JS lru-cache (In-Heap) | OffHeap Hybrid (L1+L2) | Difference / Impact |
| :--- | :--- | :--- | :--- |
| **Total Duration** | 14,733 ms | 10,259 ms | **OffHeap is 1.4x faster overall** |
| **Average Cache Latency** | **1.1 μs** | **9.0 μs** | FFI crossing baseline |
| **V8 GC Events Triggered** | 100 | 100 | Sync GC sweep checks |
| **Total V8 GC Pause Duration**| **13,295.8 ms** (13.3s) | **887.8 ms** (0.88s) | **OffHeap spends 15.0x less time in GC** |
| **Worst Single GC STW Stop** | **319.8 ms** | **16.7 ms** | **OffHeap worst-case pause is 19.1x shorter** |

*   **The SLA Winner**: While JS `lru-cache` causes the event loop to freeze for up to **319.8 ms** during a sweep (blocking all incoming HTTP requests), OffHeap isolates cache entries in native Rust memory, restricting the maximum event loop pause to just **16.7 ms**.

---

### 2. Isolated RSS Footprint Trade-off
Storing data off-heap introduces different memory layouts depending on your payload type:

#### A. Caching JavaScript JSON Objects (500k Entries, ~500B Objects)
*   **JS `lru-cache` Delta RSS**: **360.87 MB**
*   **OffHeap L2 Delta RSS**: **471.46 MB** (~30% higher memory)
*   *Why?* V8 uses highly optimized "Hidden Classes" to store object property names once. OffHeap serializes JS objects to JSON strings, meaning the schema keys (`"id"`, `"price"`, etc.) are duplicated in every record.

#### B. Caching Binary Buffers (500k Entries, 500B Buffers)
*   **JS `lru-cache` Delta RSS**: **447.94 MB**
*   **OffHeap L2 Delta RSS**: **359.75 MB** (**20% memory reduction!**)
*   *Why?* Unique `Buffer` objects in JavaScript carry massive JS wrapper object overhead (~100 bytes per buffer wrapper) and alignment tracking. OffHeap copies contiguous raw bytes directly into native memory via `mimalloc`, avoiding V8 wrapper overhead entirely.

---

## 📚 Academic Foundations & Architecture

To ensure high performance and transparent hit-rate optimization under varying workloads, **OffHeap is written entirely from scratch** based on established public academic caching research.

### Eviction Policies

1. **Least Recently Used (LRU)**
   - Doubly linked list and sharded `seahash` hash map for $O(1)$ operations.
2. **Adaptive Replacement Cache (ARC)**
   - *Paper: "ARC: A Self-Tuning, Low Overhead Replacement Cache" (FAST '03)*
   - Dynamically tunes allocation between recency ($T_1$) and frequency ($T_2$) via history/ghost lists.
3. **Window TinyLFU (W-TinyLFU)**
   - *Paper: "TinyLFU: A Highly Efficient Cache Admission Policy" (TDE '17)*
   - Utilizes a **4-bit Count-Min Sketch** frequency sketch (with a decay/reset aging policy). Combines a small Window LRU and a Segmented LRU Main Cache.

---

## 💻 Usage

```javascript
const { CacheManager } = require('./index.js');

const manager = new CacheManager();

// Create isolated caches with different policies and capacities
const sessionCache = manager.createCache('sessions', {
  policy: 'lru',
  capacity: 10000
});

const productCache = manager.createCache('products', {
  policy: 'tinylfu',
  capacity: 50000
});

// A. Store Buffers (Zero JSON overhead, 20% RSS memory saving)
productCache.set('prod_101', Buffer.from([255, 128, 64]));
const rawBuf = productCache.get('prod_101'); // returns Buffer

// B. Store JSON objects
productCache.set('prod_metadata', { id: 101, price: 99.99 });
const meta = productCache.get('prod_metadata'); // returns JavaScript Object

// C. Batch Operations (Optimized array serialization, single FFI crossing)
productCache.mset({ key1: 'value1', key2: 'value2' });
const results = productCache.mget(['key1', 'key2', 'key3']); // returns { key1: 'value1', key2: 'value2' }
```

---

## 📊 Run Benchmarks & Tests
All telemetry tests are located in `/benchmarks` and can be executed via:

```bash
# Run unit & integration tests
npm test

# Run micro, batch and process-isolated GC pressure benchmarks
npm run benchmark

# Run process-isolated binary Buffer storage footprint benchmark
node benchmarks/buffer_rss_test.js
```

---

## 📄 License

This project is double licensed under:
- **MIT License** ([LICENSE-MIT](LICENSE-MIT))
- **Apache License, Version 2.0** ([LICENSE-APACHE](LICENSE-APACHE))
