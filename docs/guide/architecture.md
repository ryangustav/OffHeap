# Architecture & Design

```mermaid
graph TD
    JS[Node.js Application] <-->|NAPI-RS FFI| Bridge[Native Addon Bridge]
    Bridge <--> Manager[CacheManager]
    Manager -->|Instantiates| Cache1[Cache: 'Sessions']
    Manager -->|Instantiates| Cache2[Cache: 'Products']
    
    subgraph V8 Heap Space
        Cache2 --> L1[L1 Cache: JS Map]
    end

    subgraph Rust Memory Space (Off-Heap)
        Cache2 --> L2[L2 Cache: Sharded Mutex]
        L2 --> Core[Cache Core Engine]
        Core --> LRU[LRU Eviction]
        Core --> ARC[ARC Eviction]
        Core --> TinyLFU[W-TinyLFU Eviction]
    end

    subgraph Remote Network Space
        Cache2 --> L3[L3 Cache: Redis]
    end
```

## Three-Layer Hybrid Caching (L1 + L2 + L3)

OffHeap supports an optional three-layer hybrid caching hierarchy to optimize local memory footprint, cross-thread sharing, and multi-node shared state:

1. **L1 (V8 Heap - Fast Read Layer)**: A lightweight, thread-local FIFO memory layer in the JS heap. It captures extreme recency bursts and reads in sub-microsecond speeds without FFI overhead.
2. **L2 (Off-Heap Rust - Core Engine)**: A sharded, thread-safe, and process-wide cache stored in Rust memory. It implements advanced eviction policies (LRU, ARC, W-TinyLFU) and is shared across all Node.js worker threads without Structured Clone serialization costs.
3. **L3 (Redis - Distributed Layer)**: An optional read-through / write-through shared Redis cache. It acts as a distributed "warm" layer shared across different physical servers or containers.

### L3 Design Principles
- **Grouped Pipeline Operations**: `getAsync` fetches the payload and PTTL (remaining TTL) using a pipeline grouped call in a single network round-trip. This ensures the native L2 cache is back-populated with the exact remaining TTL, preventing cache lifespan extension.
- **Single-Flight Coalescing**: Stampede protection covers the entire L3 check + factory path. Concurrent requests for a missing key resolve through a single Promise, yielding exactly 1 Redis read and 1 database/factory execution.
- **Graceful Degradation**: If the Redis server experiences a connection failure or timeout, L3 read/write errors are handled as silent misses. The local L1+L2 layers continue to function normally, triggering the user's `onError` logging hook without throwing exceptions to the caller.

---

## V8 Heap vs. Off-Heap Caching

In high-throughput Node.js applications, caching millions of elements in standard JavaScript memory (on-heap) introduces major performance bottlenecks. 

### The Problem: V8 Garbage Collection Pauses
The V8 engine tracks every JavaScript object, array, and string to manage memory. As the cache grows to hundreds of thousands or millions of active entries, the Garbage Collector must traverse an increasingly complex object graph during its mark-and-sweep phases. 

This causes two major issues:
1. **Stop-the-World Pauses**: Under heavy memory pressure, V8 triggers major GC collections that pause the main JavaScript execution thread, raising P99 latencies from milliseconds to several seconds.
2. **Heap Memory Limits**: Node.js defaults to a maximum heap size (often 1.4 GB or 4 GB depending on system configuration). Exceeding this limit causes `FATAL ERROR: Ineffective mark-compacts near heap limit Allocation failed - JavaScript heap out of memory`.

### The Solution: Off-Heap Memory
OffHeap completely bypasses the JavaScript heap by allocating memory directly in the **Rust memory space** (off-heap). 

- When you write an entry using `set()`, OffHeap serializes the JavaScript value and transfers the raw bytes across the Foreign Function Interface (FFI) boundary.
- The raw bytes are stored in standard Rust heap allocations (`Vec<u8>`).
- To the V8 Garbage Collector, the entire cache exists as a single native pointer. V8 does not traverse, inspect, or manage this memory, reducing major GC sweep times to virtually zero.

---

## Caching Policies

### Least Recently Used (LRU)
The baseline policy. It evicts the least recently accessed item when the cache capacity is reached. LRU is implemented using a hash map for $O(1)$ lookups and an index-based doubly linked list to track access order.

### Adaptive Replacement Cache (ARC)
ARC dynamically self-tunes between recency and frequency based on the active workload:
* It maintains two double-linked lists: $L_1$ for recency (items seen once) and $L_2$ for frequency (items seen multiple times).
* These lists are split into top sections (actual data stored in the cache) and bottom sections (ghost lists that only store keys of evicted items).
* If a cache hit occurs in the recency ghost list ($B_1$), the cache adjusts its target allocation to favor recency. If a hit occurs in the frequency ghost list ($B_2$), it tunes the allocation to favor frequency.

### Window TinyLFU (W-TinyLFU)
While LRU is the industry default, it is highly susceptible to **sparse access bursts**—occasional database sweeps or batch operations can completely flush out highly valuable, frequently accessed data. W-TinyLFU resolves this:

1. **Window LRU**: A small percentage of the total capacity (1%) is allocated to a Window LRU. All new writes enter this window first to capture short-term recency bursts.
2. **Segmented LRU (SLRU) Main Cache**: The remaining 99% of the capacity is split into a **Probationary Segment** (20%) and a **Protected Segment** (80%). When an item in Probation is hit, it is promoted to the Protected segment.
3. **TinyLFU Admission Filter**: When items overflow from the Window LRU, they compete to enter the Probationary segment. If the Probationary segment is full, the incoming item (candidate) is compared against the least recently used item in Probation (victim).
4. **4-bit Count-Min Sketch**: OffHeap uses a memory-efficient Count-Min Sketch (with an aging decay mechanism) to track key frequencies. An item is only admitted to the main cache if its access frequency is strictly higher than the victim's. If not, the candidate is evicted immediately.

---

## Multi-Threading & Genuinely Shared State

Unlike pure JavaScript where memory is strictly isolated between worker threads (unless utilizing complex `SharedArrayBuffer` structures), OffHeap supports a genuinely shared L2 cache state across multiple Node.js `worker_threads`:

* **OS Process-Level Registry**: In Rust, the cache manager stores references to all active `Cache` instances in a process-wide global static map.
* **Thread-Safe Memory Sharing**: Because the Rust library is loaded once per process, multiple worker threads calling `.getCache(name)` receive native cache wrappers pointing to the same underlying thread-safe atomic pointer (`Arc<Mutex<...>>`) to the cache shards.
* **Zero Serialization Overhead**: Threads read and write directly to/from the same native memory space, completely bypassing the V8 Structured Clone algorithm and `postMessage` overhead.
* **Consistency Options**: L2 is strictly consistent process-wide. The JS-level L1 cache is thread-local, but writes invalidate it across threads (see below). For strict read-your-writes across workers, L1 can still be disabled.

### Cross-thread L1 invalidation

Each JS `Cache` wrapper (one per thread) registers its own table of 4096 `u32` generation counters in native memory and reads it through an `ArrayBuffer`. Every native write (`set`, `delete`, `touch`, `increment`, batch variants, `clear`) bumps `table[slot(key)]` in all registered tables, where `slot` is FNV-1a over the key's UTF-16 code units, computed identically in Rust and JS.

An L1 entry stores the slot generation observed **before** the value was read from L2, and is served only while that generation is unchanged. A write racing with the read can therefore only cause an extra L2 read, never a stale L1 hit after the bump is visible. V8 does not allow two ArrayBuffers over the same memory, which is why each wrapper gets its own table instead of sharing one.

## Native Storage Layout

* **One hash per operation**: a seeded `foldhash` hash of the key picks the shard (upper 32 bits), indexes the key inside the shard (`hashbrown::HashTable`, low bits) and feeds the TinyLFU sketch (re-mixed).
* **Keys stored once**: the index holds only slab positions; the key lives in the node next to its cached hash.
* **Values as `Bytes`**: reading a value out of the shard lock is a reference-count bump; the payload is copied once, straight into the JS value.
* **Active expiration**: expiration is checked on access, and every write also inspects 4 slab slots of its shard (round-robin) once TTLs are in use, so never-read expired entries cannot pile up. `purgeExpired()` and `ttl.sweepIntervalMs` sweep explicitly.

