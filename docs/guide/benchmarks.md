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
| **Total Duration** | 14,733 ms | 10,259 ms | **OffHeap is 1.4x faster overall** |
| **Average Cache Latency** | 1.1 μs | 9.0 μs | FFI crossing baseline |
| **V8 GC Events Triggered** | 100 | 100 | Sync GC sweep checks |
| **Total V8 GC Pause Duration** | **13,295.8 ms** (13.3s) | **887.8 ms** (0.88s) | **OffHeap spends 15.0x less time in GC** |
| **Worst Single GC STW Stop** | **319.8 ms** | **16.7 ms** | **OffHeap worst-case pause is 19.1x shorter** |
| **Heap Usage (End)** | 251.56 MB | 47.19 MB | Flat V8 Heap footprint |
| **RSS Memory (Start)** | 85.70 MB | 85.24 MB | Clean start isolation |
| **RSS Memory (End)** | 446.57 MB | 556.70 MB | Process RSS at 500k entries |
| **RSS Memory Delta** | **360.87 MB** | **471.46 MB** | Isolated footprint overhead |

*   **Footprint Explanation**: In clean, isolated OS processes, the memory footprint delta of `OffHeap` is **471.46 MB** compared to JS `lru-cache`'s **360.87 MB** (only a ~30% physical overhead to store 500k records off-heap, completely protecting the V8 heap from Stop-the-World pauses).

### 3. Binary Buffer Storage Memory Footprint (500k Keys, 500B Buffers)

When storing raw binary data (like Node.js `Buffer`s, Protocol Buffers, or MessagePack), OffHeap does not suffer from V8 object wrapper overhead or repeating JSON schema keys. In isolated process tests, OffHeap uses **88 MB less physical RAM** than JS `lru-cache`:

| Metric | JS lru-cache (In-Heap) | OffHeap (L2 Native) | Difference |
| :--- | :--- | :--- | :--- |
| **Heap Usage (End)** | 145.95 MB | 23.25 MB | **122.7 MB less heap** |
| **RSS Memory (Start)** | 84.91 MB | 84.56 MB | Clean start baseline |
| **RSS Memory (End)** | 532.85 MB | 444.31 MB | Process RSS at end |
| **RSS Memory Delta** | **447.94 MB** | **359.75 MB** | **OffHeap uses 88.19 MB less physical memory (20% reduction)** |

*   **Why OffHeap Wins**: In JavaScript, storing a unique `Buffer` requires allocating a JS `Uint8Array` wrapper in the V8 heap (~100 bytes) along with V8 external backing store allocation overhead. OffHeap copies the raw bytes directly into contiguous native memory in Rust, bypassing JS object allocations entirely.

---

## 📊 Batch Operation Amortization (`mget`)

To bypass the FFI crossing overhead for multi-key lookups, use the native `mget` method. This aggregates queries inside a single boundary crossing. The table below compares loop-based reads against the optimized array-based native `mget` batch read:

### 1. Batch Size: 100 Keys
- **JS `lru-cache` (Loop)**: **2.0 μs** avg per batch
- **OffHeap L2 (Loop)**: **54.7 μs** avg per batch
- **OffHeap `mget` (Single FFI)**: **51.8 μs** avg per batch *(Faster than loop!)*

### 2. Batch Size: 1000 Keys
- **JS `lru-cache` (Loop)**: **22.6 μs** avg per batch
- **OffHeap L2 (Loop)**: **548.9 μs** avg per batch
- **OffHeap `mget` (Single FFI)**: **505.3 μs** avg per batch *(Faster than loop!)*



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
> The communication between Node.js (V8) and Rust occurs via a Native Foreign Function Interface (FFI) boundary. Crossing this boundary, casting types, and copying bytes incurs a micro overhead of approximately **0.5 to 1.0 microseconds** per operation.
> 
> * **When NOT to use OffHeap**: If you are caching small amounts of data (under 50 MB) consisting of tiny objects, a pure JavaScript Map or simple JS LRU cache will be faster, as it runs entirely in the V8 heap without FFI crossings.
> * **When to use OffHeap**: If your cache holds **gigabytes of data** or **millions of active keys** where Garbage Collection pauses dominate your application latency, OffHeap is highly superior. The micro FFI overhead is a tiny fraction of the latency saved by preventing major Stop-the-World GC sweeps and memory fragmentation.

