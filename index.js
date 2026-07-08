const { Cache: NativeCache, CacheManager: NativeCacheManager } = require('./binding');

const activePromises = new Map();
const finalizer = new FinalizationRegistry((nativeCache) => {
  try {
    nativeCache.dispose();
  } catch (e) {
    // Ignore errors during GC finalization
  }
});

class Cache {
  constructor(nativeCache) {
    this._native = nativeCache;
    this._id = Math.random().toString(36).substring(2);
    finalizer.register(this, nativeCache, this);
  }

  get(key) {
    return this._native.get(key);
  }

  peek(key) {
    return this._native.peek(key);
  }

  has(key) {
    return this._native.has(key);
  }

  set(key, value, ttlMs) {
    return this._native.set(key, value, ttlMs);
  }

  touch(key, ttlMs) {
    return this._native.touch(key, ttlMs);
  }

  delete(key) {
    return this._native.delete(key);
  }

  clear() {
    this._native.clear();
  }

  stats() {
    const rawStats = this._native.stats();
    return {
      hits: rawStats.hits,
      misses: rawStats.misses,
      capacity: rawStats.capacity,
      size: rawStats.size,
      bytesUsed: rawStats.bytesUsed,
    };
  }

  keys() {
    return this._native.keys();
  }

  increment(key, delta = 1, ttlMs) {
    return this._native.increment(key, delta, ttlMs);
  }

  decrement(key, delta = 1, ttlMs) {
    return this._native.decrement(key, delta, ttlMs);
  }

  mget(keys) {
    return this._native.mget(keys);
  }

  mset(entries, ttlMs) {
    if (typeof entries !== 'object' || entries === null) {
      throw new Error('mset requires an object of key-value entries');
    }
    this._native.mset(entries, ttlMs);
  }

  mdelete(keys) {
    if (!Array.isArray(keys)) {
      throw new Error('mdelete requires an array of keys');
    }
    return this._native.mdelete(keys);
  }

  dispose() {
    finalizer.unregister(this);
    this._native.dispose();
  }

  getOrSet(key, factory, ttlMs) {
    const val = this.get(key);
    if (val !== undefined) {
      return val;
    }

    const promiseKey = `${this._id}:${key}`;
    if (activePromises.has(promiseKey)) {
      return activePromises.get(promiseKey);
    }

    try {
      const res = factory();
      if (res instanceof Promise) {
        const p = res
          .then((resolvedVal) => {
            activePromises.delete(promiseKey);
            if (resolvedVal !== undefined) {
              this.set(key, resolvedVal, ttlMs);
            }
            return resolvedVal;
          })
          .catch((err) => {
            activePromises.delete(promiseKey);
            throw err;
          });
        activePromises.set(promiseKey, p);
        return p;
      } else {
        if (res !== undefined) {
          this.set(key, res, ttlMs);
        }
        return res;
      }
    } catch (err) {
      activePromises.delete(promiseKey);
      throw err;
    }
  }
}

const activeManagers = new Set();

class CacheManager {
  constructor() {
    this._native = new NativeCacheManager();
    activeManagers.add(this);
  }

  createCache(name, config) {
    const rawConfig = {
      policy: config.policy,
      capacity: config.capacity,
      shards: config.shards,
      maxBytes: config.maxBytes,
    };
    const nativeCache = this._native.createCache(name, rawConfig);
    return new Cache(nativeCache);
  }

  getCache(name) {
    const nativeCache = this._native.getCache(name);
    if (!nativeCache) return null;
    return new Cache(nativeCache);
  }

  deleteCache(name) {
    return this._native.deleteCache(name);
  }

  clear() {
    this._native.clear();
  }

  dispose() {
    activeManagers.delete(this);
    this._native.clear();
  }
}

process.on('exit', () => {
  for (const manager of activeManagers) {
    try {
      manager.dispose();
    } catch (e) {
      // Ignore cleanup failures on process exit
    }
  }
});

module.exports = {
  Cache,
  CacheManager,
};
