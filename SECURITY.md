# Security Policy and Auditing (SECURITY.md)

This document defines the security policy for the **OffHeap** project and details the continuous security audits implemented to ensure the library's integrity and robustness.

---

## 🛡️ Supported Versions

Currently, only the following versions receive security updates:

| Version | Supported |
| :--- | :---: |
| **0.3.x** | ✅ Yes (Active) |
| **< 0.3.0** | ❌ No |

We always recommend keeping the library updated to the latest minor/patch version available to ensure security fixes and optimizations are active in your production environment.

> [!TIP]
> **Installation Note:** Due to issues with automated publishing of native packages, versions between `0.3.0` and `0.3.10` may fail to install on certain platforms (such as Windows and Linux musl). We strongly recommend using version `0.3.11` or higher for a fully functional environment.

---

## 🔍 Automated Security Audits (CI)

To ensure that no vulnerable dependency is introduced into the library, we execute automated security audits on every *Push* and *Pull Request* in the Continuous Integration (CI) pipeline, located in [.github/workflows/CI.yml](file:///f:/ryang/Development/principal/L1-Cache/.github/workflows/CI.yml).

The audits cover the two main layers of the project (Rust and Node.js):

### 1. Cargo Audit (Rust Crates)
*   **What it does:** Scans the `Cargo.lock` file against the database of known vulnerabilities in Rust ([RustSec Advisory Database](https://rustsec.org)).
*   **CI Implementation:**
    ```yaml
    - name: Run Cargo Audit
      uses: actions-rust-lang/audit@v1
    ```

### 2. NPM Audit (Node.js Dependencies)
*   **What it does:** Analyzes the `package-lock.json` file against known vulnerabilities in the npm ecosystem.
*   **CI Implementation:**
    ```yaml
    - name: Run NPM Audit
      run: npm audit --omit=dev
    ```

> [!NOTE]  
> Both audits run before any compilation or publication. If any vulnerability is found in production dependencies, the build will automatically fail and prevent publishing new versions of the package to NPM.

---

## 🧠 Security and Memory Considerations

Since **OffHeap** manages data directly in native memory (outside the V8/Node.js heap), there are important details that developers should keep in mind when using the library:

### 1. Memory Safety with Rust
The core of OffHeap is written in **Rust**. In parts of the code that do not use `unsafe`, this by design eliminates issues such as *buffer overflows*, *dangling pointers*, and *data races*. 

Since OffHeap crosses the FFI boundary with Node.js, parts of the code require using `unsafe` to manage raw pointers on the native bridge. These sections undergo careful manual review and dedicated testing (such as validating *panic safety* and memory accounting under concurrency) instead of relying solely on the compiler's static guarantees.

Using the `mimalloc` allocator ensures that physical memory management is robust, fast, and protected against fragmentation.

### 2. Data Disposal and Zeroization (Memory Zeroization)
*   When a cache entry expires (TTL), is removed, or is evicted by eviction policies (LRU, ARC, W-TinyLFU), its memory space is returned to the native `mimalloc` allocator.
*   By default, **OffHeap does not overwrite bytes with zeros (zeroization) before freeing memory**.
*   **Recommendation:** If you are storing extremely sensitive data (e.g., private keys, raw passwords), encrypt them before inserting them into the cache, or ensure that you perform cleaning (zeroization) at the application level.

### 3. Namespace Isolation and Collision Risk
*   Using `CacheManager.createCache(name)` generates a physically and logically isolated cache instance. Keys do not collide between different cache namespaces.
*   **Warning:** If your application decides to unify the cache and use manual prefixes to separate tenants, e.g., `tenant_id + "::" + key`, make sure to sanitize the inputs to prevent key collision attacks.

### 4. Active Mitigations and Defenses against Abuse and DoS
OffHeap has explicit defenses integrated into the code architecture to contain common attack vectors in caching systems:
*   **Integer Overflow Protection**: We explicitly enable `overflow-checks = true` in the release profile (`Cargo.toml`). This ensures that arithmetic operations on size and byte accounting (`bytes_used`) trigger a controlled panic in case of overflow instead of failing silently (which could open doors for memory corruption).
*   **Key Limit Validation**: We apply a strict validation that no cache key can exceed the safety limit of **8192 bytes**. This check is performed redundantly: first at the JavaScript/TypeScript layer (to fail fast) and then at the native Rust layer.
*   **Hash Flooding Resistance**: For shard routing and internal cache indexing, OffHeap uses Rust's standard `HashMap` structure, which employs the `SipHash-1-3` algorithm with cryptographically secure seeds randomly generated per process (`RandomState`). This prevents intentional hash collision attacks designed to degrade hash table performance to $O(N)$.
*   **Dynamic LZ4 Decompression Ceiling**: To prevent decompression bomb attacks (where a small compressed payload expands into gigabytes upon decompression), OffHeap validates the uncompressed size of the LZ4 block before vector allocation. This limit is dynamic: it is restricted to a maximum of 32 MB or **10% of the cache's maximum byte limit** (`maxBytes` * 0.1), whichever is smaller — with a minimum floor of **1 KB** to avoid degenerate limits in low-capacity configurations. This ensures decompression never causes sudden Out Of Memory (OOM) exhaustion.

---

## ✉️ How to Report a Vulnerability

If you discover any security vulnerability in OffHeap, **please do not open a public issue**. Instead, follow the procedure below:

1. Send an email describing the vulnerability in detail to the maintainer: **ryangustav (via GitHub or project contact email)** or create a **Draft Security Advisory** directly in the GitHub repository.
2. Provide as much information as possible, including:
   * Detailed steps to reproduce the issue.
   * Proof of Concept (PoC) or code snippet demonstrating the vulnerability.
   * Possible system impact.

We commit to analyzing the report quickly and responding with a mitigation and fix plan within a timely manner.
