# Architecture & Design

```mermaid
graph TD
    JS[Node.js Application] <-->|NAPI-RS FFI| Bridge[Native Addon Bridge]
    Bridge <--> Manager[CacheManager]
    Manager -->|Instantiates| Cache1[Cache: 'Sessions']
    Manager -->|Instantiates| Cache2[Cache: 'Products']
    
    subgraph Rust Memory Space (Off-Heap)
        Cache2 --> Lock[Mutex Lock]
        Lock --> Core[Cache Core Engine]
        Core --> LRU[LRU Eviction]
        Core --> ARC[ARC Eviction]
        Core --> TinyLFU[W-TinyLFU Eviction]
        
        TinyLFU -.-> Sketch[4-bit Count-Min Sketch]
    end
```

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
