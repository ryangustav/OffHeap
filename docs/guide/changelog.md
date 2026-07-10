# Changelog

All notable changes to the OffHeap project are documented here.

---

## [0.3.1] - 2026-07-09

### Fixed
* **Alpine/Musl TLS Relocation**: Enabled the `local_dynamic_tls` feature flag on the `mimalloc` allocator crate. This resolves the `initial-exec TLS resolves to dynamic definition` runtime relocation error encountered on musl libc systems (like Alpine Linux) when Node.js dynamically loads (`dlopen`) the compiled Rust native library.
* **macOS Cross-Architecture Tests**: Disabled native testing for the `x86_64-apple-darwin` target built on macOS host runners (which are now Apple Silicon `arm64`), preventing the loader mismatch error.
* **CI Release Pipeline**: Updated the CI upload artifacts to include generated binding loader files (`binding.js`, `binding.d.ts`) and configured the publish step to loop over and publish all platform-specific sub-packages in `npm/*` alongside the main package.

### Added
* **Postinstall Load Check**: Added a diagnostic postinstall check in `scripts/postinstall.js` that verifies native binary loading and fails with descriptive advice if platform binaries fail to install, while exiting cleanly during local development.

---

## [0.3.0] - 2026-07-09

### Added
* **Process-wide Shared State**: Moved the internal cache registry to a process-wide global static map in Rust (`GLOBAL_CACHES`), enabling multiple Node.js `worker_threads` (isolated V8 Isolates) to share and mutate the exact same underlying native L2 cache memory space directly without serialization/copying overhead.
* **Worker Threads Integration Tests**: Added a multi-threaded concurrent stress test verifying thread-safety and shared memory access.

### Fixed
* **Finalization Registry Bug**: Removed the JS-heap `FinalizationRegistry` from the `Cache` class wrapper, preventing the GC of a single thread's cache wrapper from destroying the cache contents for other threads. Memory is now cleanly managed by Rust standard reference counting (`Arc`).
* **CacheManager Memory Leak**: Removed the global `activeManagers` tracking set and `process.on('exit')` cleanup hooks, allowing unused `CacheManager` instances to be garbage collected in Node.js.

---

## [0.2.0] - 2026-07-09

* Version bump to release layout.

---

## [0.1.0] - 2026-07-08

* Initial implementation of the caching framework in Rust with NAPI-RS.
* Support for Least Recently Used (LRU), Adaptive Replacement Cache (ARC), and Window TinyLFU (W-TinyLFU) eviction policies.
* High-performance sharded memory locks, serialization boundaries, and telemetry tracking.
