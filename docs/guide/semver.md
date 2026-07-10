# Versioning Policy (SemVer)

OffHeap strictly follows [Semantic Versioning 2.0.0 (SemVer)](https://semver.org/) for release versioning. Every release is represented in the format `MAJOR.MINOR.PATCH` (e.g., `0.3.1`).

---

## Pre-1.0.0 Rules (0.y.z)

While the project is in the initial development phase (`0.y.z`), the API is subject to refinement based on real-world usage and performance feedback. 

To maintain transparency and prevent developer frustration, OffHeap adheres to the following conventions:
* **Breaking Changes (`0.x.0`)**: Breaking API changes, signature modifications, or significant behavior alterations will trigger a bump in the **minor** version (e.g., from `0.2.0` to `0.3.0`).
* **Features & Optimizations (`0.x.y`)**: Backward-compatible new features, performance enhancements, and optimizations will bump the **patch** version (e.g., from `0.3.0` to `0.3.1`).
* **Fixes & Releases (`0.x.y`)**: Bug fixes, packaging updates, and pipeline improvements also bump the **patch** version.

---

## The 1.0.0 Production Milestone

Graduating to version `1.0.0` represents a strong guarantee of API stability, robustness, and suitability for mission-critical production environments.

> [!IMPORTANT]
> **The 1.0.0 Exit Criteria:**
> OffHeap will **not** release version `1.0.0` until the framework has been successfully deployed and verified in a **real, active production environment by someone other than the main creator and maintainer (Ryan Gustavo)**.

### Why this rule exists?
1. **Real-world Validation**: Synthetic benchmarks and isolated integration tests cannot replicate the chaotic nature of production environments (varying memory pressure, network spikes, diverse query workloads, and unique concurrency profiles).
2. **API Completeness**: Real-world developers integrating the library in foreign systems will identify missing knobs, configuration gaps, or edge cases.
3. **No False Promises**: A `1.0.0` version should represent a battle-tested caching solution, not just a complete feature list.

### Help Us Reach 1.0.0!
If you are using OffHeap in your projects or are planning to deploy it to staging or production, please share your experience! We would love to hear from you:
* Open a [GitHub Discussion](https://github.com/ryangustav/OffHeap/discussions) with your use case.
* Report any anomalies, tail latency figures, or memory usage statistics.
