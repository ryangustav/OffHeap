# Benchmarks & Telemetry

Below are the performance metrics comparing the throughput of the LRU, ARC, and W-TinyLFU cache engines.

---

## Performance Results

### 1. Write Throughput (`SET` Operations)
*10,000 capacity, 20,000 unique keys (forcing evictions)*

| Engine | Operations/sec | Avg Latency | Margin | Samples |
| :--- | :--- | :--- | :--- | :--- |
| **OffHeap LRU** | **486,701 ops/s** | 2.05 μs | ±0.26% | 486,702 |
| **OffHeap ARC** | **485,246 ops/s** | 2.06 μs | ±0.24% | 485,247 |
| **OffHeap W-TinyLFU** | **375,509 ops/s** | 2.66 μs | ±0.30% | 375,510 |

### 2. Read Throughput (`GET` Operations)
*100% Cache Hits*

| Engine | Operations/sec | Avg Latency | Margin | Samples |
| :--- | :--- | :--- | :--- | :--- |
| **OffHeap LRU** | **774,848 ops/s** | 1.29 μs | ±0.84% | 774,849 |
| **OffHeap ARC** | **784,789 ops/s** | 1.27 μs | ±0.13% | 784,790 |
| **OffHeap W-TinyLFU** | **727,002 ops/s** | 1.37 μs | ±0.14% | 727,003 |

---

## Testing Methodology

All benchmarks were run locally and are fully reproducible using the scripts in our repository.

* **Machine Specifications**: Intel(R) Core(TM) i9 CPU (2.4 GHz, 8 Cores), 16 GB RAM.
* **OS Environment**: Windows 11 (build-optimized native binary).
* **Node.js Version**: v24.16.0
* **Payload Size**: Storing flat JSON objects containing a 100-character payload string (`{ data: "..." }`).
* **Test Runner**: Benchmarks compiled with release optimizations (`cargo build --release` / `napi build --release`) and executed using [tinybench](https://www.npmjs.com/package/tinybench) over a 1000ms duration per task.

---

## ⚠️ Important Architectural Warning

> [!WARNING]
> **FFI Boundary Overhead**
> The communication between Node.js (V8) and Rust occurs via a Native Foreign Function Interface (FFI) boundary. Crossing this boundary, casting types, and copying bytes incurs a micro overhead of approximately **1.2 to 2.5 microseconds** per operation.
> 
> * **When NOT to use OffHeap**: If you are caching small amounts of data (under a few megabytes) consisting of tiny objects, a pure JavaScript Map or simple JS LRU cache will be faster, as it runs entirely in the V8 heap without FFI crossings.
> * **When to use OffHeap**: If your cache holds **gigabytes of data** or **millions of active keys** where Garbage Collection pauses dominate your application latency, OffHeap is highly superior. The micro FFI overhead is a tiny fraction of the latency saved by preventing major Stop-the-World GC sweeps.
