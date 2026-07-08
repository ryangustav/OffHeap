# API Reference

All methods exported by the native addon execute **synchronously** to avoid microtask queue scheduling overhead in Node.js, ensuring maximum throughput.

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
  capacity: 10000
});
```
* **Parameters**:
  * `name` (`string`): Unique name/namespace for the cache.
  * `config` (`CacheConfig`):
    * `policy` (`"lru" | "arc" | "tinylfu"`): The eviction policy.
    * `capacity` (`number`): The maximum number of entries allowed in the cache.
* **Returns**: `Cache` instance.
* **Throws**: Error if a cache with the specified `name` already exists.

### `getCache(name)`
Retrieves an existing cache by name.
* **Parameters**:
  * `name` (`string`): The cache namespace.
* **Returns**: `Cache | null` (returns `null` if the cache does not exist).

### `deleteCache(name)`
Deletes a cache instance from the manager. The memory is reclaimed once all JS references to the returned `Cache` are garbage collected.
* **Parameters**:
  * `name` (`string`): The cache namespace to delete.
* **Returns**: `boolean` (true if the cache was successfully deleted).

### `clear()`
Deletes all cache instances managed by this instance.
* **Returns**: `void`

---

## `Cache`

An isolated cache instance. `Cache` is cheap to clone and thread-safe.

### `get(key)`
Retrieves a value from the cache.
```javascript
const value = cache.get('prod_101');
```
* **Parameters**:
  * `key` (`string`): The lookup key.
* **Returns**: `Buffer | string | object | undefined`
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
//   size: 4210
// }
```
* **Returns**: `CacheStats` object:
  * `hits` (`number`): Number of successful read queries.
  * `misses` (`number`): Number of queries for missing or expired keys.
  * `capacity` (`number`): Sized capacity.
  * `size` (`number`): Current count of active entries.
