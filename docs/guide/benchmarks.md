# Benchmarks & Telemetry

Below are the performance metrics comparing the throughput of the LRU, ARC, and W-TinyLFU cache engines under different conditions, followed by the **L1/L2 Hybrid** performance profile and **Garbage Collection (GC) Pressure** tests.

---

## ⚡ L1/L2 Hybrid Performance Profile

To bypass the Node-API (NAPI) FFI boundary latency for hot data, OffHeap uses a hybrid multi-level cache:
- **L1 Cache (JS-local)**: A size-bounded, high-speed JS Map with a FIFO replacement policy. It serves hot read hits instantly inside V8.
- **L2 Cache (Rust-native)**: The backing sharded Rust engine. It keeps the data safely off-heap.
- **Read-Through Writes**: `set` writes directly to L2 and invalidates L1 to prevent stale data. On L1 read misses, the item is fetched from L2 and promoted to L1.

### 1. SET Throughput
*10,000 capacity, 20,000 unique keys (forcing evictions)*

| Engine | Operations/sec | Avg Latency |
| :--- | :--- | :--- |
| **Pure JS Map** | ~9.2M ops/s | 108 ns |
| **JS `lru-cache`** | ~4.7M ops/s | 208 ns |
| **OffHeap L1+L2 (Read-through)** | **~596k ops/s** | **1.6 μs** |
| **OffHeap L2 Only** | **~562k ops/s** | **1.7 μs** |

### 2. GET Throughput
*100% Cache Hits*

| Engine | Operations/sec | Avg Latency | vs. `lru-cache` |
| :--- | :--- | :--- | :--- |
| **Pure JS Map** | ~18.1M ops/s | 54 ns | — |
| **JS `lru-cache`** | ~12.0M ops/s | 83 ns | Baseline |
| **OffHeap L1 Hit** | **~10.6M ops/s** | **94 ns** | **Technical Tie** |
| **OffHeap L2 Hit (LRU)** | **~881k ops/s** | **1.1 μs** | 13x slower |
| **OffHeap L2 Hit (ARC)** | **~883k ops/s** | **1.1 μs** | 13x slower |
| **OffHeap L2 Hit (W-TinyLFU)** | **~864k ops/s** | **1.1 μs** | 13x slower |

---

## ⏱️ Garbage Collection Latency Pressure Test

A cache with hundreds of thousands of keys stored in JavaScript forces the V8 GC to scan millions of live references on every sweep, causing **Stop-the-World (STW)** pause spikes. 

We ran a benchmark simulating 1,000,000 operations under active memory allocations with **500,000 unique keys** (~500B payloads):

| Metric | JS lru-cache (In-Heap) | OffHeap Hybrid (L1+L2) | Difference / Impact |
| :--- | :--- | :--- | :--- |
| **Total Duration** | 19,595 ms | 10,930 ms | **OffHeap is 1.8x faster overall** |
| **Average Cache Latency** | 1.2 μs | 9.6 μs | FFI crossing baseline |
| **V8 GC Events Triggered** | 100 | 100 | Sync GC sweep checks |
| **Total V8 GC Pause Duration** | **17,991.9 ms** (17.9s) | **919.1 ms** (0.91s) | **OffHeap spends 19.5x less time in GC** |
| **Worst Single GC STW Stop** | **364.1 ms** | **24.5 ms** | **OffHeap worst-case pause is 14.8x shorter** |
| **Heap Usage (End)** | 251.38 MB | 47.03 MB | Flat V8 Heap footprint |
| **RSS Memory (End)** | 536.86 MB | 959.42 MB | Off-heap physical storage |

*   **GC Stop-the-World Avoidance**: While in-heap JS structures cause V8 to block for up to **364ms** to scan the heap, OffHeap isolates cache data outside the V8 heap in native Rust memory, restricting the maximum GC latency spike to just **24ms**.

---

## 📊 Batch Operation Amortization (`mget`)

To bypass the FFI crossing overhead for multi-key lookups, use the native `mget` method. This aggregates queries inside a single boundary crossing:

| Batch Size | Loop JS lru-cache | Loop OffHeap (L2) | Batch OffHeap (mget) |
| :--- | :--- | :--- | :--- |
| **10** | 5 μs | 5 μs | **3 μs** (Faster than lru-cache loop!) |
| **100** | 9 μs | 43 μs | **9 μs** (technical tie) |
| **1000** | 79 μs | 654 μs | **108 μs** |
| **5000** | 392 μs | 2.60 ms | **349 μs** (Faster than lru-cache loop!) |

---

## Testing Methodology

All benchmarks were run locally and are fully reproducible using the scripts in our repository.

* **OS Environment**: Windows 11 (build-optimized native release binary).
* **Node.js Version**: >= v18
* **Reproduction Commands**:
  * Run all benchmark sections: `npm run benchmark`
  * Run V8 heap crossover study: `node --expose-gc benchmarks/crossover_bench.js`

---

## ⚠️ Architectural Trade-offs: The Crossover Decision

> [!WARNING]
> **FFI Boundary Overhead**
> The communication between Node.js (V8) and Rust occurs via a Native Foreign Function Interface (FFI) boundary. Crossing this boundary, casting types, and copying bytes incurs a micro overhead of approximately **1.1 to 1.7 microseconds** per operation.
> 
> * **When NOT to use OffHeap**: If you are caching small amounts of data (under 50 MB) consisting of tiny objects, a pure JavaScript Map or simple JS LRU cache will be faster, as it runs entirely in the V8 heap without FFI crossings.
> * **When to use OffHeap**: If your cache holds **gigabytes of data** or **millions of active keys** where Garbage Collection pauses dominate your application latency, OffHeap is highly superior. The micro FFI overhead is a tiny fraction of the latency saved by preventing major Stop-the-World GC sweeps and memory fragmentation.

