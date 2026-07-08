# API Reference

All methods exported by the native addon execute **synchronously** to avoid microtask queue scheduling overhead in Node.js, ensuring maximum throughput. Custom high-level utilities like `getOrSet` seamlessly support both synchronous and asynchronous operations.

---

## `CacheManager`

The central manager responsible for provisioning and isolating individual caches.

```javascript
const { CacheManager } = require('offheap');
const manager = new CacheManager();
```

### `new CacheManager()`
Instantiates a new central cache manager.
* **Returns**: `CacheManager` instance.

### `createCache(name, config)`
Creates and returns an isolated cache instance.
```javascript
const cache = manager.createCache('products', {
  policy: 'tinylfu',
  capacity: 10000,
  shards: 16,
  maxBytes: 100 * 1024 * 1024 // 100 MB byte-capacity limit
});
```
* **Parameters**:
  * `name` (`string`): Unique name/namespace for the cache.
  * `config` (`CacheConfig`):
    * `policy` (`"lru" | "arc" | "tinylfu"`): The eviction policy.
    * `capacity` (`number`): The maximum number of entries allowed in the cache.
    * `shards` (`number`, *optional*): Number of internal locks shards. High concurrency workloads benefit from larger shard numbers (e.g. 16 or 32). Default: `8`.
    * `maxBytes` (`number`, *optional*): The maximum memory size of keys and values combined in bytes. When this threshold is crossed, entries are evicted according to the active policy.
* **Returns**: `Cache` instance.
* **Throws**: Error if a cache with the specified `name` already exists.

### `getCache(name)`
Retrieves an existing cache by name.
* **Parameters**:
  * `name` (`string`): The cache namespace.
* **Returns**: `Cache | null` (returns `null` if the cache does not exist).

### `deleteCache(name)`
Deletes a cache instance from the manager.
* **Parameters**:
  * `name` (`string`): The cache namespace to delete.
* **Returns**: `boolean` (true if the cache was successfully deleted).

### `clear()`
Deletes all cache instances managed by this instance.
* **Returns**: `void`

### `dispose()`
Releases all cache instances immediately, releasing all underlying native memory.
* **Returns**: `void`

---

## `Cache`

An isolated, thread-safe cache instance.

### `get(key)`
Retrieves a value from the cache.
```javascript
const value = cache.get('prod_101');
```
* **Parameters**:
  * `key` (`string`): The lookup key.
* **Returns**: `Buffer | string | object | number | boolean | undefined`
  * Returns `Buffer` if the value was stored as a `Buffer` or `Uint8Array`.
  * Returns `string` if the value was stored as a string.
  * Returns `object | array | number | boolean` if the value was stored as a JSON-serializable type.
  * Returns `undefined` if the key is missing or expired.

### `set(key, value, ttl_ms?)`
Stores an entry in the cache. If the key already exists, its value is overwritten.
```javascript
cache.set('key', { data: 'test' }, 60000); // Stores object with 60s TTL
```
* **Parameters**:
  * `key` (`string`): The entry key.
  * `value` (`Buffer | Uint8Array | string | any`): The payload to store.
  * `ttl_ms` (`number`, *optional*): The time-to-live in milliseconds. If omitted, the entry has no expiry.
* **Returns**: `Buffer | string | object | undefined` (returns the old value if it was overwritten, or `undefined`).

### `has(key)`
Checks if a key exists in the cache and is not expired, without deserializing the value.
```javascript
if (cache.has('auth_session')) { ... }
```
* **Parameters**:
  * `key` (`string`): Key to check.
* **Returns**: `boolean` (true if key exists and is valid).

### `peek(key)`
Retrieves a value without updating the eviction metadata (e.g., LRU order or frequency sketch count). Useful for logging, debugging, or health checks.
```javascript
const debugVal = cache.peek('hot_key');
```
* **Parameters**:
  * `key` (`string`): Key to retrieve.
* **Returns**: `Buffer | string | object | undefined`

### `touch(key, ttl_ms)`
Renews or changes the Time-To-Live (TTL) of a key without re-writing the cached value.
```javascript
cache.touch('session_12', 30 * 60 * 1000); // Extend session by 30 min
```
* **Parameters**:
  * `key` (`string`): Key to renew.
  * `ttl_ms` (`number`, *optional*): New time-to-live in milliseconds. Use `undefined` to clear expiry.
* **Returns**: `boolean` (true if the key existed and TTL was updated).

### `increment(key, delta?, ttl_ms?)`
Atomically increments a numeric counter key in memory (Tag 4). Ideal for rate limiters.
```javascript
const requestCount = cache.increment('rate_limit:ip_127.0.0.1', 1, 60000);
```
* **Parameters**:
  * `key` (`string`): Key of the counter.
  * `delta` (`number`, *optional*): Value to increment by. Default: `1`.
  * `ttl_ms` (`number`, *optional*): Time-to-live for the counter if it's created.
* **Returns**: `number` (the newly incremented counter value).

### `decrement(key, delta?, ttl_ms?)`
Atomically decrements a numeric counter key in memory (Tag 4).
```javascript
const remainingTokens = cache.decrement('api_tokens:user_88', 1);
```
* **Parameters**:
  * `key` (`string`): Key of the counter.
  * `delta` (`number`, *optional*): Value to decrement by. Default: `1`.
  * `ttl_ms` (`number`, *optional*): Time-to-live.
* **Returns**: `number` (the newly decremented counter value).

### `mget(keys)`
Performs a batch lookup for multiple keys in a single FFI boundary crossing, significantly improving throughput for multi-key lookups.
```javascript
const items = cache.mget(['k1', 'k2', 'k3']); // Returns { k1: val1, k2: val2 }
```
* **Parameters**:
  * `keys` (`string[]`): Array of keys to retrieve.
* **Returns**: `Record<string, any>` (Object mapping found keys to their deserialized values).

### `mset(entries, ttl_ms?)`
Performs a batch write of multiple key-value entries in a single FFI crossing.
```javascript
cache.mset({ a: 1, b: 'hello', c: Buffer.from([1, 2]) }, 60000);
```
* **Parameters**:
  * `entries` (`Record<string, any>`): Object representing key-value entries to store.
  * `ttl_ms` (`number`, *optional*): Time-to-live in milliseconds for all written entries.

### `mdelete(keys)`
Performs a batch delete of multiple keys in a single FFI crossing.
```javascript
const deletedCount = cache.mdelete(['a', 'b', 'c']);
```
* **Parameters**:
  * `keys` (`string[]`): Array of keys to delete.
* **Returns**: `number` (number of deleted keys).

### `getOrSet(key, factory, ttl_ms?)`
Implements a coalesced compute-on-miss cache access pattern. If two concurrent requests lookup the same missing key, they will await the **same** factory promise, preventing cache stampedes.
```javascript
const product = await cache.getOrSet('prod_101', async () => {
  return await db.fetchProduct(101);
}, 60000);
```
* **Parameters**:
  * `key` (`string`): Key.
  * `factory` (`() => any | Promise<any>`): A callback that computes the value if missing. Can return a Promise or a synchronous value.
  * `ttl_ms` (`number`, *optional*): TTL in milliseconds for the computed value.
* **Returns**: `any` (the cached value, or the resolved promise result).

### `delete(key)`
Deletes a specific key from the cache.
* **Parameters**:
  * `key` (`string`): The key to remove.
* **Returns**: `boolean` (true if the key existed and was deleted).

### `clear()`
Removes all keys and resets stats for this cache instance.
* **Returns**: `void`

### `keys()`
Returns an array of all active (non-expired) keys in the cache.
* **Returns**: `string[]`

### `stats()`
Returns telemetry statistics for the cache.
```javascript
const telemetry = cache.stats();
// Telemetry format:
// {
//   hits: 1452,
//   misses: 92,
//   capacity: 10000,
//   size: 4210,
//   bytesUsed: 541029 // total size of keys + values in bytes
// }
```
* **Returns**: `CacheStats` object:
  * `hits` (`number`): Number of successful read queries.
  * `misses` (`number`): Number of queries for missing or expired keys.
  * `capacity` (`number`): Sized capacity.
  * `size` (`number`): Current count of active entries.
  * `bytesUsed` (`number`): Current byte-capacity usage of stored keys and values.

### `dispose()`
Explicitly disposes of the native sub-caches immediately, freeing all of its memory back to the OS.
* **Returns**: `void`
