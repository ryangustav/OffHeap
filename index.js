const { Cache: NativeCache, CacheManager: NativeCacheManager } = require('./binding');

const activePromises = new Map();

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
    policy: 'w-tinylfu',
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
  },
  monitoring: {
    minIntervalMs: 500,
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

  // Monitoring normalization
  if (config.monitoring !== undefined) {
    normalized.monitoring = typeof config.monitoring === 'object'
      ? config.monitoring
      : { minIntervalMs: config.monitoring };
  }

  return normalized;
}

function validateKey(key) {
  if (typeof key !== 'string') {
    throw new TypeError('Key must be a string');
  }
  const byteLength = Buffer.byteLength(key, 'utf8');
  if (byteLength > 8192) {
    throw new RangeError('Key length exceeds safety limit of 8192 bytes');
  }
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
    validateKey(key);
    // 1. Check JS-local L1 cache (FIFO read - zero writes on hit)
    if (this._l1Capacity > 0) {
      const l1Val = this._l1.get(key);
      if (l1Val !== undefined) {
        return l1Val;
      }
    }

    // 2. Check Native L2 Cache
    const nativeVal = this._native.get(key);
    
    // Check if expired
    if (nativeVal && typeof nativeVal === 'object' && nativeVal.__expired) {
      this._l1.delete(key);
      if (this._config.hooks) {
        if (this._config.hooks.onExpire) {
          this._config.hooks.onExpire(key, unwrapValue(nativeVal.value));
        }
        if (this._config.hooks.onMiss) {
          this._config.hooks.onMiss(key);
        }
      }
      return undefined;
    }

    const unwrapped = unwrapValue(nativeVal);
    if (unwrapped !== undefined) {
      this._l1Set(key, unwrapped);
    } else {
      if (this._config.hooks && this._config.hooks.onMiss) {
        this._config.hooks.onMiss(key);
      }
    }
    return unwrapped;
  }

  peek(key) {
    validateKey(key);
    if (this._l1Capacity > 0) {
      const l1Val = this._l1.get(key);
      if (l1Val !== undefined) {
        return l1Val;
      }
    }
    const nativeVal = this._native.peek(key);
    if (nativeVal && typeof nativeVal === 'object' && nativeVal.__expired) {
      this._l1.delete(key);
      if (this._config.hooks && this._config.hooks.onExpire) {
        this._config.hooks.onExpire(key, unwrapValue(nativeVal.value));
      }
      return undefined;
    }
    return unwrapValue(nativeVal);
  }

  has(key) {
    validateKey(key);
    if (this._l1Capacity > 0 && this._l1.has(key)) {
      return true;
    }
    const nativeVal = this._native.has(key);
    if (nativeVal && typeof nativeVal === 'object' && nativeVal.__expired) {
      this._l1.delete(key);
      if (this._config.hooks && this._config.hooks.onExpire) {
        this._config.hooks.onExpire(key, unwrapValue(nativeVal.value));
      }
      return false;
    }
    return !!nativeVal;
  }

  set(key, value, ttlMsOrOptions) {
    validateKey(key);
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

    const evictions = this._native.set(key, wrapped, ttlMs, forceCompression);
    if (Array.isArray(evictions)) {
      for (const ev of evictions) {
        if (ev.reason === 'expired') {
          if (this._config.hooks && this._config.hooks.onExpire) {
            this._config.hooks.onExpire(ev.key, unwrapValue(ev.value));
          }
        } else {
          if (this._config.hooks && this._config.hooks.onEvict) {
            this._config.hooks.onEvict(ev.key, unwrapValue(ev.value), ev.reason);
          }
        }
      }
    }
  }

  touch(key, ttlMs) {
    validateKey(key);
    this._l1.delete(key); // Invalidate L1 on TTL update
    const nativeVal = this._native.touch(key, ttlMs);
    if (nativeVal && typeof nativeVal === 'object' && nativeVal.__expired) {
      if (this._config.hooks && this._config.hooks.onExpire) {
        this._config.hooks.onExpire(key, unwrapValue(nativeVal.value));
      }
      return false;
    }
    return !!nativeVal;
  }

  delete(key) {
    validateKey(key);
    this._l1.delete(key);
    return this._native.delete(key);
  }

  clear() {
    this._l1.clear();
    this._native.clear();
  }

  stats() {
    const rawStats = this._native.stats();

    // Shards analysis
    const shardSizes = rawStats.shards.map(s => s.size);
    const shardCount = shardSizes.length;
    const sizeMean = shardSizes.reduce((a, b) => a + b, 0) / shardCount;
    const sizeStdDev = Math.sqrt(
      shardSizes.reduce((a, b) => a + Math.pow(b - sizeMean, 2), 0) / shardCount
    );

    return {
      hits: rawStats.hits,
      misses: rawStats.misses,
      capacity: rawStats.capacity,
      size: rawStats.size,
      bytesUsed: rawStats.bytesUsed,
      sets: rawStats.sets,
      deletes: rawStats.deletes,
      evictions: rawStats.evictions,
      expirations: rawStats.expirations,
      hitRate: rawStats.hitRate,
      uptimeMs: rawStats.uptimeMs,
      shards: {
        count: shardCount,
        details: rawStats.shards.map(s => ({
          size: s.size,
          bytesUsed: s.bytes_used || s.bytesUsed,
        })),
        sizeStdDev: Math.round(sizeStdDev * 100) / 100,
      },
      memory: {
        payloadBytes: rawStats.bytesUsed,
        processRss: process.memoryUsage().rss,
      },
      l1: {
        size: this._l1.size,
        capacity: this._l1Capacity,
      },
    };
  }

  monitor(callback, intervalMs) {
    if (typeof callback !== 'function') {
      throw new TypeError('monitor() requires a callback function');
    }

    // Compatibility guard: statsCounters() was added after some 0.4.0 binaries
    // were published. Fall back to stats() for older native builds.
    const hasStatsCounters = typeof this._native.statsCounters === 'function';
    const hasStats = typeof this._native.stats === 'function';
    if (!hasStatsCounters && !hasStats) {
      throw new Error('Native cache does not support monitoring stats');
    }

    const _getCounters = hasStatsCounters
      ? () => this._native.statsCounters()
      : () => {
          const s = this._native.stats();
          return { hits: s.hits, misses: s.misses, sets: s.sets, deletes: s.deletes, evictions: s.evictions, expirations: s.expirations };
        };

    const HARD_FLOOR = 16;
    const configuredMin = (this._config.monitoring && this._config.monitoring.minIntervalMs) !== undefined
      ? this._config.monitoring.minIntervalMs
      : 500;
    const effectiveMin = Math.max(HARD_FLOOR, configuredMin);
    const interval = intervalMs !== undefined ? intervalMs : effectiveMin;

    if (interval < HARD_FLOOR) {
      throw new RangeError(
        `intervalMs ${interval} is below the absolute minimum of ${HARD_FLOOR}ms`
      );
    }
    if (interval < effectiveMin) {
      throw new RangeError(
        `intervalMs ${interval} is below configured minIntervalMs ${effectiveMin}`
      );
    }

    let prev = _getCounters();
    let prevTime = Date.now();

    const timer = setInterval(() => {
      const now = Date.now();
      const curr = _getCounters();
      const elapsed = (now - prevTime) / 1000;

      const delta = {
        hits: curr.hits - prev.hits,
        misses: curr.misses - prev.misses,
        sets: curr.sets - prev.sets,
        deletes: curr.deletes - prev.deletes,
        evictions: curr.evictions - prev.evictions,
        expirations: curr.expirations - prev.expirations,
      };

      const totalOps = delta.hits + delta.misses + delta.sets + delta.deletes;

      const snapshot = {
        totals: {
          hits: curr.hits,
          misses: curr.misses,
          sets: curr.sets,
          deletes: curr.deletes,
          evictions: curr.evictions,
          expirations: curr.expirations,
        },
        delta,
        rates: {
          opsPerSec: elapsed > 0 ? Math.round(totalOps / elapsed) : 0,
          hitsPerSec: elapsed > 0 ? Math.round(delta.hits / elapsed) : 0,
          missesPerSec: elapsed > 0 ? Math.round(delta.misses / elapsed) : 0,
          setsPerSec: elapsed > 0 ? Math.round(delta.sets / elapsed) : 0,
          hitRate: (delta.hits + delta.misses) > 0
            ? delta.hits / (delta.hits + delta.misses)
            : null,
        },
        intervalMs: now - prevTime,
        timestamp: now,
      };

      prev = curr;
      prevTime = now;

      callback(snapshot);
    }, interval);

    return () => clearInterval(timer);
  }

  keys() {
    return this._native.keys();
  }

  increment(key, delta = 1, ttlMs) {
    validateKey(key);
    this._l1.delete(key); // Invalidate L1 on mutation
    return this._native.increment(key, delta, ttlMs);
  }

  decrement(key, delta = 1, ttlMs) {
    validateKey(key);
    this._l1.delete(key); // Invalidate L1 on mutation
    return this._native.decrement(key, delta, ttlMs);
  }

  mget(keys) {
    if (!Array.isArray(keys)) {
      throw new Error('mget requires an array of keys');
    }
    const missing = [];
    const result = {};

    for (const key of keys) {
      validateKey(key);
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
        const key = missing[i];
        const valRaw = nativeValues[i];
        
        if (valRaw && typeof valRaw === 'object' && valRaw.__expired) {
          this._l1.delete(key);
          if (this._config.hooks) {
            if (this._config.hooks.onExpire) {
              this._config.hooks.onExpire(key, unwrapValue(valRaw.value));
            }
            if (this._config.hooks.onMiss) {
              this._config.hooks.onMiss(key);
            }
          }
        } else {
          const val = unwrapValue(valRaw);
          if (val !== undefined) {
            result[key] = val;
            this._l1Set(key, val);
          } else {
            if (this._config.hooks && this._config.hooks.onMiss) {
              this._config.hooks.onMiss(key);
            }
          }
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
      validateKey(k);
      const val = entries[k];
      this._l1.delete(k); // Invalidate L1
      wrapped[k] = wrapValue(val);
    }
    const evictions = this._native.mset(wrapped, ttlMs);
    if (Array.isArray(evictions)) {
      for (const ev of evictions) {
        if (ev.reason === 'expired') {
          if (this._config.hooks && this._config.hooks.onExpire) {
            this._config.hooks.onExpire(ev.key, unwrapValue(ev.value));
          }
        } else {
          if (this._config.hooks && this._config.hooks.onEvict) {
            this._config.hooks.onEvict(ev.key, unwrapValue(ev.value), ev.reason);
          }
        }
      }
    }
  }
  mdelete(keys) {
    if (!Array.isArray(keys)) {
      throw new Error('mdelete requires an array of keys');
    }
    for (const k of keys) {
      validateKey(k);
      this._l1.delete(k);
    }
    return this._native.mdelete(keys);
  }

  dispose() {
    this._l1.clear();
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

class CacheManager {
  constructor(globalConfig = {}) {
    this._native = new NativeCacheManager();
    this._globalConfig = mergeConfigs(DEFAULT_CONFIG, normalizeConfig(globalConfig));
    this._caches = new Map();
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
    const cache = new Cache(nativeCache, merged);
    this._caches.set(name, cache);
    return cache;
  }

  getCache(name) {
    if (this._caches.has(name)) {
      return this._caches.get(name);
    }
    const nativeCache = this._native.getCache(name);
    if (!nativeCache) return null;
    const cache = new Cache(nativeCache, this._globalConfig);
    this._caches.set(name, cache);
    return cache;
  }

  deleteCache(name) {
    this._caches.delete(name);
    return this._native.deleteCache(name);
  }

  clear() {
    this._caches.clear();
    this._native.clear();
  }

  dispose() {
    this._caches.clear();
    this._native.clear();
  }
}

module.exports = {
  Cache,
  CacheManager,
};
