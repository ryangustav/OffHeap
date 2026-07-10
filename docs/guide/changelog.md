# Changelog

All notable changes to the OffHeap project are documented here.

## [0.3.7] - 2026-07-09

### Fixed
* **CI/CD Publish (npm provenance validation)**: Configured the `"repository"` URL block inside `package.json` and propagated it to all platform-specific sub-packages. This resolves the `Error verifying sigstore provenance bundle: Failed to validate repository information` error when publishing to NPM with provenance enabled.

---

## [0.3.6] - 2026-07-09

### Fixed
* **CI/CD Publish (npm local path & script execution)**:
  - Prefixed local publication paths with `./` (e.g. `./npm/*`) in `CI.yml` so that `npm publish` correctly recognizes them as local directories instead of attempting to fetch git ssh paths.
  - Added `--ignore-scripts` to the main package publication command. This prevents the `prepublishOnly` script hook from triggering `napi prepublish` inside CI, avoiding secondary target publication conflicts and registry E403 forbidden errors.

---

## [0.3.5] - 2026-07-09

### Fixed
* **CI/CD Publish (npm platform directories)**: Committed the `npm/` platform directories (generated via `napi create-npm-dir`) to the repository. These directories contain the `package.json` stubs with `os`, `cpu`, and `main` fields that NAPI-RS requires to exist on disk before `napi artifacts` can copy `.node` binaries into them. Previously they were never committed to git, causing `ENOENT` write failures in CI.
* **Removed redundant `Prepublish` CI step**: The `napi prepublish` command does not create directories — it only publishes existing ones. Removed the step from the publish job since it served no purpose without pre-existing directories.

---

## [0.3.4] - 2026-07-09

### Fixed
* **CI/CD Publish (prepublish order)**: Swapped the order of the `Prepublish` and `Copy artifacts` steps in the publish job in `CI.yml`. This ensures that the platform directories are scaffolded by `napi prepublish` before `napi artifacts` attempts to copy the `.node` binaries, preventing `ENOENT` directory-missing errors.

---

## [0.3.3] - 2026-07-09

### Fixed
* **CI/CD Publish (napi config)**: Reconfigured `"napi"` block to use `triples.additional` in `package.json` so that the NAPI-RS CLI correctly maps all 7 target platforms, resolving the `No dist dir found` error during artifact packaging.

---

## [0.3.2] - 2026-07-09

### Fixed
* **CI/CD Publish (napi artifacts)**: Added all 7 build targets to `optionalDependencies` in `package.json` to resolve the `Type Error: No dist dir found` error during `napi artifacts` execution in the release job.

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
