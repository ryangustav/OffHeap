---
layout: home

hero:
  name: OffHeap
  text: High-Performance Off-Heap Caching
  tagline: Bypassing the V8 Heap using native Rust and NAPI-RS. Zero Garbage Collection overhead at scale.
  actions:
    - theme: brand
      text: Get Started
      link: /guide/getting-started
    - theme: alt
      text: View API Reference
      link: /guide/api

features:
  - title: Zero GC Pause
    details: Storing serialized data in Rust memory keeps the V8 heap clean and prevents Stop-the-World garbage collection sweeps.
  - title: W-TinyLFU & ARC
    details: Implementations of Adaptive Replacement Cache and Window TinyLFU from scratch to maximize cache hit rates under skewed workloads.
  - title: Multiple Isolated Caches
    details: Provision isolated cache instances from a central manager, each with its own capacity and eviction strategies.
---
