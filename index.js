const { Cache: NativeCache, CacheManager: NativeCacheManager } = require('./binding');

const activePromises = new Map();
const finalizer = new FinalizationRegistry((nativeCache) => {
  try {
    nativeCache.dispose();
  } catch (e) {
    // Ignore errors during GC finalization
  }
});

function wrapValue(val) {
  if (typeof val === 'object' && val !== null && !Buffer.isBuffer(val)) {
    return '\0J' + JSON.stringify(val);
  }
  if (Array.isArray(val) || typeof val === 'boolean') {
    return '\0J' + JSON.stringify(val);
  }
  return val;
}

function unwrapValue(val) {
  if (typeof val === 'string' && val.startsWith('\0J')) {
    return JSON.parse(val.substring(2));
  }
  return val;
}

class Cache {
  constructor(nativeCache, config = {}) {
    this._native = nativeCache;
    this._id = Math.random().toString(36).substring(2);
    
    // L1 Cache config (default to 10% of capacity, capped at 10,000 keys)
    const capacity = config.capacity || 10000;
    this._l1Capacity = config.l1Capacity !== undefined 
      ? config.l1Capacity 
      : Math.min(10000, Math.ceil(capacity * 0.1));
      
    this._l1 = new Map();
    finalizer.register(this, nativeCache, this);
  }

  _l1Set(key, value) {
    if (this._l1Capacity <= 0) return;
    this._l1.delete(key);
    this._l1.set(key, value);
    if (this._l1.size > this._l1Capacity) {
      const oldest = this._l1.keys().next().value;
      this._l1.delete(oldest);
    }
  }

  get(key) {
    // 1. Check JS-local L1 cache (FIFO read - zero writes on hit)
    if (this._l1Capacity > 0) {
      const l1Val = this._l1.get(key);
      if (l1Val !== undefined) {
        return l1Val;
      }
    }

    // 2. Check Native L2 Cache
    const nativeVal = unwrapValue(this._native.get(key));
    if (nativeVal !== undefined) {
      this._l1Set(key, nativeVal);
    }
    return nativeVal;
  }

  peek(key) {
    if (this._l1Capacity > 0) {
      const l1Val = this._l1.get(key);
      if (l1Val !== undefined) {
        return l1Val;
      }
    }
    return unwrapValue(this._native.peek(key));
  }

  has(key) {
    if (this._l1Capacity > 0 && this._l1.has(key)) {
      return true;
    }
    return this._native.has(key);
  }

  set(key, value, ttlMs) {
    // Invalidate L1 to prevent stale data
    this._l1.delete(key);
    const wrapped = wrapValue(value);
    this._native.set(key, wrapped, ttlMs);
  }

  touch(key, ttlMs) {
    this._l1.delete(key); // Invalidate L1 on TTL update
    return this._native.touch(key, ttlMs);
  }

  delete(key) {
    this._l1.delete(key);
    return this._native.delete(key);
  }

  clear() {
    this._l1.clear();
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
    this._l1.delete(key); // Invalidate L1 on mutation
    return this._native.increment(key, delta, ttlMs);
  }

  decrement(key, delta = 1, ttlMs) {
    this._l1.delete(key); // Invalidate L1 on mutation
    return this._native.decrement(key, delta, ttlMs);
  }

  mget(keys) {
    const missing = [];
    const result = {};

    for (const key of keys) {
      if (this._l1Capacity > 0) {
        const val = this._l1.get(key);
        if (val !== undefined) {
          result[key] = val;
          continue;
        }
      }
      missing.push(key);
    }

    if (missing.length > 0) {
      const nativeRes = this._native.mget(missing);
      for (const key of missing) {
        const val = unwrapValue(nativeRes[key]);
        if (val !== undefined) {
          result[key] = val;
          this._l1Set(key, val);
        }
      }
    }

    return result;
  }

  mset(entries, ttlMs) {
    if (typeof entries !== 'object' || entries === null) {
      throw new Error('mset requires an object of key-value entries');
    }
    const wrapped = {};
    for (const k in entries) {
      const val = entries[k];
      this._l1.delete(k); // Invalidate L1
      wrapped[k] = wrapValue(val);
    }
    this._native.mset(wrapped, ttlMs);
  }

  mdelete(keys) {
    if (!Array.isArray(keys)) {
      throw new Error('mdelete requires an array of keys');
    }
    for (const k of keys) {
      this._l1.delete(k);
    }
    return this._native.mdelete(keys);
  }

  dispose() {
    this._l1.clear();
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

  createCache(name, config = {}) {
    const rawConfig = {
      policy: config.policy,
      capacity: config.capacity,
      shards: config.shards,
      maxBytes: config.maxBytes,
    };
    const nativeCache = this._native.createCache(name, rawConfig);
    return new Cache(nativeCache, config);
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
