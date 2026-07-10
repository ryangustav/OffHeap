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
| **Total Duration** | 14,320 ms | 10,355 ms | **OffHeap is 1.38x faster overall** |
| **Average Cache Latency** | **1.1 μs** | **9.2 μs** | FFI boundary + LZ4 decompression |
| **V8 GC Events Triggered** | 100 | 100 | Sync GC sweep checks |
| **Total V8 GC Pause Duration**| **12,886.2 ms** (12.8s) | **851.3 ms** (0.85s) | **OffHeap spends 15.1x less time in GC** |
| **Worst Single GC STW Stop** | **274.4 ms** | **14.1 ms** | **OffHeap worst-case pause is 19.4x shorter** |

*   **The SLA Winner**: While JS `lru-cache` causes the event loop to freeze for up to **274.4 ms** during a sweep (blocking all incoming HTTP requests), OffHeap isolates cache entries in native Rust memory, restricting the maximum event loop pause to just **14.1 ms**.

---

### 2. Isolated RSS Footprint & Compression Trade-off
OffHeap incorporates optional native **LZ4 block compression** for serialized JSON values (via `lz4_flex`) and `mimalloc` to avoid memory fragmentation. This introduces a classic engineering trade-off:

*   **`compression: false` (Default)**: Guarantees peak throughput (**757.6k write ops/sec**) with zero extra CPU overhead.
*   **`compression: true` (Opt-in)**: Reduces RSS memory footprint by up to **29%** in exchange for a ~20% write throughput penalty.

Under process-isolated tests:

#### A. Caching JavaScript JSON Objects (500k Entries, ~500B Objects, Compression Active)
*   **JS `lru-cache` Delta RSS**: **359.69 MB**
*   **OffHeap L2 Delta RSS**: **255.44 MB** (**29% physical memory reduction!**)
*   *Why?* While V8 hidden classes deduplicate object structures in memory, storing them as raw objects incurs massive V8 heap overhead. OffHeap serializes and compresses them with LZ4 in Rust, dropping the raw footprint by ~50%.

#### B. Caching Binary Buffers (500k Entries, 500B Buffers)
*   **JS `lru-cache` Delta RSS**: **447.94 MB**
*   **OffHeap L2 Delta RSS**: **359.75 MB** (**20% physical memory reduction!**)
*   *Why?* Unique `Buffer` objects in JavaScript carry massive JS wrapper object overhead (~100 bytes per buffer wrapper) and alignment tracking. OffHeap copies contiguous raw bytes directly into native memory via `mimalloc`, avoiding V8 wrapper overhead entirely.

---

### 3. Eviction Policy Hit-Ratio Comparison (Zipfian Popularities, 100k Operations)
To justify making **W-TinyLFU** the default eviction engine, we simulated 100,000 accesses against a popularity space of 5,000 unique keys (cache capacity restricted to 1,000 keys) across low, medium, and high Zipfian skews:

| Zipfian Skew ($s$) | LRU (Baseline) | ARC (Self-Tuning) | W-TinyLFU (Default) | W-TinyLFU VS LRU (Delta) |
| :--- | :--- | :--- | :--- | :--- |
| **Low Skew ($s = 0.5$)** | 30.82% | 34.69% | **37.64%** | **+6.82% (Winner)** |
| **Medium Skew ($s = 0.7$)**| 45.85% | 50.70% | **53.34%** | **+7.49% (Winner)** |
| **High Skew ($s = 0.9$)** | 65.21% | 69.11% | **70.67%** | **+5.46% (Winner)** |

*   **Why W-TinyLFU Wins Consistently**: LRU and ARC are vulnerable to quick eviction when cold keys enter the cache in bursts. W-TinyLFU's Count-Min frequency sketch acts as a filter, rejecting low-frequency entries and admitting new keys only if their access frequency exceeds the victim candidate, consistently outperforming LRU and ARC across all skew intensities.

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
   - Utilizes a **4-bit Count-Min Sketch** frequency sketch (with a decay/reset aging policy). Combines a small Window LRU and a Segmented LRU Main Cache.

### 🧵 Genuinely Shared State across Node.js Worker Threads

One of OffHeap's most powerful architectural advantages is its ability to share L2 cache state **genuinely and directly across multiple V8 worker threads** within the same Node.js process:

*   **The JS Limitation**: In pure JavaScript, sharing any state (like a `Map`) between `worker_threads` is impossible without serializing and cloning the data back and forth (via `postMessage`), incurring major CPU and latency overhead.
*   **The OffHeap Solution**: Because OffHeap's L2 cache is allocated in native Rust memory using thread-safe pointers (`Arc<Mutex<...>>`) in a process-wide global registry, different worker threads calling `.getCache(name)` on their own `CacheManager` instances will point to the **exact same underlying memory buffer**.
*   **Zero Serialization Overhead**: Key lookups and mutations read and write from/to the same shared native heap directly, making multi-threaded cache access virtually free of data-copying overhead.
*   **Consistency Control**: 
    *   **L1 Cache (Eventual Consistency)**: If L1 cache is enabled (default), each worker thread maintains a thread-local V8 Map of hot keys. This delivers maximum read throughput but may lead to temporary, eventual consistency across threads when keys are updated.
    *   **L2-Only (Strict Consistency)**: For strict cross-thread read/write consistency, instantiate your caches with L1 disabled:
        ```javascript
        const manager = new CacheManager({
          l1: { enabled: false } // Force all reads directly to shared L2
        });
        ```

### Security & Memory Considerations

*   **Physical Namespace Isolation vs. Collision Risk**:
    *   Calling `CacheManager.createCache(name, config)` creates a physically isolated native `Cache` instance with its own shards and maps. Keys cannot collide across different namespace instances.
    *   If developers choose to partition a single `Cache` instance manually (e.g., prefixing keys as `tenant_id + "::" + user_key`), they must sanitize/escape the `::` separator to prevent cross-tenant key collision vulnerabilities.
*   **Memory Releasing & Zeroization**:
    *   Evicted or deleted cache items are released back to the native `mimalloc` allocator.
    *   OffHeap does **not** zero out (wipe) the memory bytes before returning the buffer to the allocator. If caching highly sensitive credentials (like passwords or tokens), implement application-layer encryption or zeroization before caching if secure memory wiping is required.

---

## 💻 Usage

```javascript
const { CacheManager } = require('./index.js');

// 1. Global Config: Set defaults for all namespace caches
const manager = new CacheManager({
  eviction: { policy: 'w-tinylfu', capacity: 50000 },
  compression: { enabled: true, minSizeBytes: 1024 }, // LZ4 on JSON payloads >= 1KB
  l1: { enabled: true },
  ttl: { defaultMs: 1000 * 60 * 15 } // 15 mins default
});

// 2. Namespace Config: Inherits from global, overrides specific knobs
const sessionCache = manager.createCache('sessions', {
  eviction: { policy: 'arc', capacity: 10000 },
  compression: { enabled: false }, // Small session objects, skip CPU compression
  ttl: { defaultMs: 1000 * 60 * 30 } // 30 mins override
});

const reportCache = manager.createCache('reports'); // Inherits all global defaults

// 3. Operation Override: Fine-grained knobs on set()
reportCache.set('heavy_report_data', { id: 101, rows: [...] }); // Compressed (>=1KB)
reportCache.set('noclick_record', { clicked: false }, { compression: false }); // Uncompressed override

// A. Store Buffers (Zero JSON overhead, 20% RSS memory saving)
reportCache.set('prod_binary_101', Buffer.from([255, 128, 64]));
const rawBuf = reportCache.get('prod_binary_101'); // returns Buffer

// B. Store JSON objects
reportCache.set('prod_metadata', { id: 101, price: 99.99 });
const meta = reportCache.get('prod_metadata'); // returns JavaScript Object

// C. Batch Operations (Optimized array serialization, single FFI crossing)
reportCache.mset({ key1: 'value1', key2: 'value2' });
const results = reportCache.mget(['key1', 'key2']); // returns { key1: 'value1', key2: 'value2' }
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
