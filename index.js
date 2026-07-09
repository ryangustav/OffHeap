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

const DEFAULT_CONFIG = {
  shards: 8,
  eviction: {
    policy: 'tinylfu',
    capacity: 10000,
    maxBytes: undefined,
  },
  compression: {
    enabled: false,
    algorithm: 'lz4',
    minSizeBytes: 1024,
  },
  l1: {
    enabled: true,
    capacity: undefined,
  },
  ttl: {
    defaultMs: undefined,
    mode: 'absolute', // absolute or sliding
  }
};

function mergeConfigs(parent, child) {
  const result = { ...parent };
  for (const k in child) {
    if (child[k] && typeof child[k] === 'object' && !Array.isArray(child[k])) {
      result[k] = mergeConfigs(parent[k] || {}, child[k]);
    } else if (child[k] !== undefined) {
      result[k] = child[k];
    }
  }
  return result;
}

function normalizeConfig(config) {
  if (!config) return {};
  const normalized = { ...config };
  
  // Eviction normalization
  if (config.policy !== undefined || config.capacity !== undefined || config.maxBytes !== undefined) {
    normalized.eviction = {
      policy: config.policy,
      capacity: config.capacity,
      maxBytes: config.maxBytes,
      ...config.eviction
    };
  }
  
  // Compression normalization
  if (config.compression !== undefined) {
    normalized.compression = typeof config.compression === 'object'
      ? config.compression
      : { enabled: !!config.compression };
  }
  
  // L1 normalization
  if (config.l1Capacity !== undefined) {
    normalized.l1 = {
      enabled: config.l1Capacity > 0,
      capacity: config.l1Capacity,
      ...config.l1
    };
  }

  return normalized;
}

class Cache {
  constructor(nativeCache, config = {}) {
    this._native = nativeCache;
    this._id = Math.random().toString(36).substring(2);
    this._config = config;
    
    // Set up L1 Cache
    const capacity = (config.eviction && config.eviction.capacity) || 10000;
    this._l1Capacity = (config.l1 && config.l1.enabled)
      ? (config.l1.capacity !== undefined ? config.l1.capacity : Math.min(10000, Math.ceil(capacity * 0.1)))
      : 0;
      
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

  set(key, value, ttlMsOrOptions) {
    let ttlMs = this._config.ttl ? this._config.ttl.defaultMs : undefined;
    let compression = this._config.compression ? this._config.compression.enabled : false;
    let minSizeBytes = this._config.compression ? (this._config.compression.minSizeBytes || 1024) : 1024;

    if (typeof ttlMsOrOptions === 'number') {
      ttlMs = ttlMsOrOptions;
    } else if (ttlMsOrOptions && typeof ttlMsOrOptions === 'object') {
      if (ttlMsOrOptions.ttlMs !== undefined) ttlMs = ttlMsOrOptions.ttlMs;
      if (ttlMsOrOptions.compression !== undefined) compression = ttlMsOrOptions.compression;
      if (ttlMsOrOptions.minSizeBytes !== undefined) minSizeBytes = ttlMsOrOptions.minSizeBytes;
    }

    // Invalidate L1 to prevent stale data
    this._l1.delete(key);

    const wrapped = wrapValue(value);

    // ⚠️ CRITICAL PATH: Do NOT introduce slow checks or traversals (like Buffer.byteLength)
    // inside this block if compression is disabled. The default path (compression: false)
    // is measured to maintain 757k+ write ops/sec. Only perform length queries when compression is active.
    let forceCompression = undefined;
    if (typeof wrapped === 'string' && wrapped.startsWith('\0J')) {
      if (!compression) {
        forceCompression = false;
      } else {
        const payloadSize = Buffer.byteLength(wrapped) - 2; // Subtract 2 bytes for '\0J' prefix
        forceCompression = payloadSize >= minSizeBytes;
      }
    }

    this._native.set(key, wrapped, ttlMs, forceCompression);
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
      const nativeValues = this._native.mget(missing);
      for (let i = 0; i < missing.length; i++) {
        const val = unwrapValue(nativeValues[i]);
        if (val !== undefined) {
          const key = missing[i];
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
  constructor(globalConfig = {}) {
    this._native = new NativeCacheManager();
    this._globalConfig = mergeConfigs(DEFAULT_CONFIG, normalizeConfig(globalConfig));
    activeManagers.add(this);
  }

  createCache(name, config = {}) {
    const merged = mergeConfigs(this._globalConfig, normalizeConfig(config));
    const rawConfig = {
      policy: merged.eviction.policy,
      capacity: merged.eviction.capacity,
      shards: merged.shards,
      maxBytes: merged.eviction.maxBytes,
      compression: merged.compression.enabled,
    };
    const nativeCache = this._native.createCache(name, rawConfig);
    return new Cache(nativeCache, merged);
  }

  getCache(name) {
    const nativeCache = this._native.getCache(name);
    if (!nativeCache) return null;
    return new Cache(nativeCache, this._globalConfig);
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
