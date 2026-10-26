const binding = require('./binding');
const { version: PACKAGE_VERSION } = require('./package.json');

const { CacheManager: NativeCacheManager } = binding;

// Binaries older than 0.5.0 don't export nativeVersion().
const NATIVE_VERSION = typeof binding.nativeVersion === 'function' ? binding.nativeVersion() : null;
if (NATIVE_VERSION !== PACKAGE_VERSION) {
  process.emitWarning(
    `offheap: native addon ${NATIVE_VERSION || '(< 0.5.0)'} does not match the JS wrapper ${PACKAGE_VERSION}. ` +
    'Reinstall offheap so both come from the same release; features missing from the binary are disabled.',
    { code: 'OFFHEAP_NATIVE_MISMATCH' }
  );
}

const activePromises = new Map();

// Must match L1_GEN_SLOTS in src/cache.rs.
const L1_GEN_SLOTS = 4096;

// Slab slots inspected per shard on each ttl.sweepIntervalMs tick.
const SWEEP_SLOTS_PER_TICK = 1024;

// FNV-1a over UTF-16 code units: the same slot as l1_slot() in src/cache.rs.
function l1Slot(key) {
  let h = 0x811c9dc5 | 0;
  for (let i = 0; i < key.length; i++) {
    h = Math.imul(h ^ key.charCodeAt(i), 16777619);
  }
  return h & (L1_GEN_SLOTS - 1);
}

// A value held in the JS-local L1 layer.
class L1Entry {
  constructor(value, slot, gen, expiresAt, slidingMs) {
    this.value = value;
    this.slot = slot; // L1 generation slot of the key
    this.gen = gen; // generation observed before the value was read from L2
    this.expiresAt = expiresAt; // Date.now() deadline, 0 = never expires
    this.slidingMs = slidingMs; // L1-only sliding TTL window, 0 = absolute
  }
}

function wrapValue(val) {
  if (typeof val === 'string') {
    // "\0J" marks JSON on the native side, so strings starting with "\0" travel JSON-encoded
    // and can never be mistaken for a serialized object.
    return val.charCodeAt(0) === 0 ? '\0J' + JSON.stringify(val) : val;
  }
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

class RedisL3 {
  constructor(client, effectivePrefix, onError) {
    this._client = client;
    this._prefix = (key) => effectivePrefix + key;
    this._onError = onError || null;
  }

  async get(key) {
    const prefixed = this._prefix(key);
    try {
      // Pipeline grouped: GET + PTTL in a single round-trip.
      // Not MULTI/EXEC - there is no isolation guarantee between the two commands,
      // but the race window is irrelevant for approximate TTL back-population.
      const [[errGet, raw], [errTtl, pttl]] = await this._client.pipeline()
        .getBuffer(prefixed)
        .pttl(prefixed)
        .exec();

      if (errGet) {
        if (this._onError) this._onError(errGet, 'get', key);
        return { value: undefined, ttlMs: undefined };
      }
      if (errTtl) {
        if (this._onError) this._onError(errTtl, 'get', key);
        return { value: undefined, ttlMs: undefined };
      }
      if (raw === null || raw.length === 0) {
        return { value: undefined, ttlMs: undefined };
      }

      // pttl === -2: key expired between GET and PTTL -> treat as miss, do not back-populate L2 with negative TTL.
      // pttl === -1: key exists but has no TTL -> back-populate without TTL (undefined, not -1ms).
      // pttl > 0: remaining TTL in ms.
      if (pttl === -2) {
        return { value: undefined, ttlMs: undefined };
      }
      const remainingTtl = pttl === -1 ? undefined : pttl;

      const tag = raw[0];
      const payload = raw.subarray(1);
      let value;

      if (tag === 1) {
        value = payload;
      } else if (tag === 2) {
        const str = payload.toString('utf8');
        value = unwrapValue(str);
      } else if (tag === 3) {
        value = Number(payload.toString('utf8'));
      } else {
        value = unwrapValue(payload.toString('utf8'));
      }

      return { value, ttlMs: remainingTtl };
    } catch (err) {
      if (this._onError) this._onError(err, 'get', key);
      return { value: undefined, ttlMs: undefined };
    }
  }

  async setRaw(key, wrappedValue, ttlMs) {
    const prefixed = this._prefix(key);
    let buffer;
    if (Buffer.isBuffer(wrappedValue)) {
      buffer = Buffer.concat([Buffer.from([1]), wrappedValue]);
    } else if (typeof wrappedValue === 'string') {
      buffer = Buffer.concat([Buffer.from([2]), Buffer.from(wrappedValue, 'utf8')]);
    } else if (typeof wrappedValue === 'number') {
      buffer = Buffer.concat([Buffer.from([3]), Buffer.from(wrappedValue.toString(), 'utf8')]);
    } else {
      buffer = Buffer.concat([Buffer.from([2]), Buffer.from(String(wrappedValue), 'utf8')]);
    }

    try {
      if (typeof ttlMs === 'number' && ttlMs > 0) {
        await this._client.set(prefixed, buffer, 'PX', ttlMs);
      } else {
        await this._client.set(prefixed, buffer);
      }
    } catch (err) {
      if (this._onError) this._onError(err, 'set', key);
    }
  }

  async delete(key) {
    const prefixed = this._prefix(key);
    try {
      await this._client.del(prefixed);
    } catch (err) {
      if (this._onError) this._onError(err, 'delete', key);
    }
  }

  isReady() {
    return this._client && (this._client.status === 'ready' || typeof this._client.ping === 'function');
  }
}

const DEFAULT_CONFIG = {
  shards: undefined, // native default: 4 per CPU, capped so each shard holds >= 64 entries
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
  l2: {
    enabled: true,
  },
  ttl: {
    defaultMs: undefined,
    mode: 'absolute', // absolute or sliding
    sweepIntervalMs: undefined, // periodic active expiration of never-read entries
  },
  monitoring: {
    minIntervalMs: 500,
  }
};

function mergeConfigs(parent, child) {
  const result = { ...parent };
  for (const k in child) {
    if (k === 'redis') {
      result[k] = { ...parent[k], ...child[k] };
      continue;
    }
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

// Decides the per-call compression override for an already wrapped value.
// ⚠️ CRITICAL PATH: Do NOT introduce slow checks or traversals (like Buffer.byteLength)
// when compression is disabled. The default path (compression: false) is measured to
// maintain 757k+ write ops/sec. Only perform length queries when compression is active.
function forceCompressionFor(wrapped, compression, minSizeBytes) {
  if (typeof wrapped === 'string' && wrapped.startsWith('\0J')) {
    if (!compression) {
      return false;
    }
    const payloadSize = Buffer.byteLength(wrapped) - 2; // Subtract 2 bytes for '\0J' prefix
    return payloadSize >= minSizeBytes;
  }
  return undefined;
}

function validateKey(key) {
  if (typeof key !== 'string') {
    throw new TypeError('Key must be a string');
  }
  // A UTF-16 code unit takes at most 3 UTF-8 bytes, so short keys skip the byte count.
  if (key.length > 2730 && Buffer.byteLength(key, 'utf8') > 8192) {
    throw new RangeError('Key length exceeds safety limit of 8192 bytes');
  }
}

class Cache {
  constructor(nativeCache, config = {}, redis = null) {
    this._native = nativeCache;
    this._id = Math.random().toString(36).substring(2);
    this._config = config;
    this._redis = redis;
    
    // Set up L1 Cache
    const capacity = (config.eviction && config.eviction.capacity) || 10000;
    const l2Enabled = !(config.l2 && config.l2.enabled === false);
    this._l2Enabled = l2Enabled;
    this._l1Capacity = (config.l1 && config.l1.enabled)
      ? (config.l1.capacity !== undefined ? config.l1.capacity : (l2Enabled ? Math.min(10000, Math.ceil(capacity * 0.1)) : capacity))
      : 0;

    this._l1 = new Map(); // key -> L1Entry
    this._l1Hits = 0;

    // Binaries built before getWithTtl/mgetWithTtl cannot report the remaining TTL of an
    // L2 entry. With those, L1 is only filled from L2 while no TTL has been used, so it
    // never serves a value past its expiry.
    this._ttlAware = typeof nativeCache.getWithTtl === 'function'
      && typeof nativeCache.mgetWithTtl === 'function';
    this._ttlInUse = !!(config.ttl && config.ttl.defaultMs !== undefined);
    this._sliding = !!(config.ttl && config.ttl.mode === 'sliding');

    // Cross-thread L1 invalidation: native writes bump `_gens[l1Slot(key)]` in every
    // thread's table, and an L1 entry is only served while its slot generation is unchanged.
    this._gens = null;
    const sharedInvalidation = !(config.l1 && config.l1.sharedInvalidation === false);
    if (l2Enabled && this._l1Capacity > 0 && sharedInvalidation && typeof nativeCache.l1Generations === 'function') {
      try {
        this._gens = new Uint32Array(nativeCache.l1Generations());
      } catch {
        // Runtimes that forbid external ArrayBuffers (e.g. Electron's V8 sandbox): L1 stays
        // eventually consistent across threads, as in earlier versions.
      }
    }

    // Active expiration of entries that are never read again.
    this._sweepTimer = null;
    const sweepIntervalMs = config.ttl && config.ttl.sweepIntervalMs;
    if (l2Enabled && sweepIntervalMs > 0 && typeof nativeCache.purgeExpired === 'function') {
      this._sweepTimer = setInterval(() => this.purgeExpired(SWEEP_SLOTS_PER_TICK), sweepIntervalMs);
      this._sweepTimer.unref();
    }
  }

  _l1Get(key) {
    const ent = this._l1.get(key);
    if (ent === undefined) return undefined;
    if (ent.expiresAt !== 0) {
      const now = Date.now();
      if (ent.expiresAt <= now) {
        this._l1.delete(key);
        return undefined;
      }
      if (ent.slidingMs !== 0) ent.expiresAt = now + ent.slidingMs;
    }
    if (this._gens !== null && this._gens[ent.slot] !== ent.gen) {
      this._l1.delete(key); // written by this or another thread since it was read
      return undefined;
    }
    this._l1Hits++;
    return ent.value;
  }

  // `ttlMs` is the remaining lifetime of the value; undefined means it never expires.
  // `slot`/`gen` must be read with _l1Slot()/_l1Gen() *before* the value was read from L2.
  _l1Set(key, value, ttlMs, slot = 0, gen = 0, slidingMs = 0) {
    if (this._l1Capacity <= 0) return;
    this._l1.delete(key);
    this._l1.set(key, new L1Entry(value, slot, gen, ttlMs === undefined ? 0 : Date.now() + ttlMs, slidingMs));
    if (this._l1.size > this._l1Capacity) {
      this._l1.delete(this._l1.keys().next().value);
    }
  }

  // L1-only mode: the value lives only here, with its own (optionally sliding) TTL.
  _l1SetLocal(key, value, ttlMs) {
    const hasTtl = ttlMs > 0;
    this._l1Set(key, value, hasTtl ? ttlMs : undefined, 0, 0, hasTtl && this._sliding ? ttlMs : 0);
  }

  _l1Slot(key) {
    return this._gens === null || this._l1Capacity <= 0 ? 0 : l1Slot(key);
  }

  _l1Gen(slot) {
    return this._gens === null ? 0 : this._gens[slot];
  }

  // Fills L1 with a value read from L2/L3. `remainingTtl` comes from the layer it was read from.
  _l1Fill(key, value, remainingTtl, slot, gen) {
    if (remainingTtl === undefined) {
      if (this._ttlInUse && !this._ttlAware) return;
    } else if (this._sliding) {
      return; // L1 hits never reach L2, so they could not extend a sliding TTL
    }
    this._l1Set(key, value, remainingTtl, slot, gen);
  }

  _noteTtl(ttlMs) {
    if (ttlMs !== undefined && ttlMs !== null) this._ttlInUse = true;
  }

  _nativeGet(key) {
    return this._ttlAware ? this._native.getWithTtl(key) : this._native.get(key);
  }

  _onExpired(key, expired, fireMiss) {
    this._l1.delete(key);
    const hooks = this._config.hooks;
    if (hooks) {
      if (hooks.onExpire) {
        hooks.onExpire(key, unwrapValue(expired.value));
      }
      if (fireMiss && hooks.onMiss) {
        hooks.onMiss(key);
      }
    }
  }

  _emitEvictions(evictions) {
    const hooks = this._config.hooks;
    if (!hooks || !Array.isArray(evictions)) return;
    for (const ev of evictions) {
      if (ev.reason === 'expired') {
        if (hooks.onExpire) hooks.onExpire(ev.key, unwrapValue(ev.value));
      } else if (hooks.onEvict) {
        hooks.onEvict(ev.key, unwrapValue(ev.value), ev.reason);
      }
    }
  }

  _reportRedisError(err, op, key) {
    if (this._config.redis && typeof this._config.redis.onError === 'function') {
      this._config.redis.onError(err, op, key);
    }
  }

  // Writes a value found in L3 back into L2 (keeping its remaining TTL) and L1.
  _backfillFromL3(key, value, remainingTtl) {
    if (this._l2Enabled) {
      const compression = this._config.compression ? this._config.compression.enabled : false;
      const minSizeBytes = this._config.compression ? (this._config.compression.minSizeBytes || 1024) : 1024;
      const wrapped = wrapValue(value);
      this._noteTtl(remainingTtl);
      this._emitEvictions(this._native.set(key, wrapped, remainingTtl, forceCompressionFor(wrapped, compression, minSizeBytes)));
    }
    // Snapshot taken after our own write, which bumps the slot.
    const slot = this._l1Slot(key);
    this._l1Fill(key, value, remainingTtl, slot, this._l1Gen(slot));
  }

  get(key) {
    validateKey(key);
    // 1. Check JS-local L1 cache (FIFO read - zero writes on hit)
    if (this._l1Capacity > 0) {
      const l1Val = this._l1Get(key);
      if (l1Val !== undefined) {
        return l1Val;
      }
    }

    if (!this._l2Enabled) {
      if (this._config.hooks && this._config.hooks.onMiss) {
        this._config.hooks.onMiss(key);
      }
      return undefined;
    }

    // 2. Check Native L2 Cache
    const slot = this._l1Slot(key);
    const gen = this._l1Gen(slot);
    let nativeVal = this._nativeGet(key);

    // Check if expired
    if (nativeVal && typeof nativeVal === 'object' && nativeVal.__expired) {
      this._onExpired(key, nativeVal, true);
      return undefined;
    }

    let remainingTtl;
    if (Array.isArray(nativeVal)) {
      remainingTtl = nativeVal[1];
      nativeVal = nativeVal[0];
    }

    const unwrapped = unwrapValue(nativeVal);
    if (unwrapped !== undefined) {
      this._l1Fill(key, unwrapped, remainingTtl, slot, gen);
    } else {
      if (this._config.hooks && this._config.hooks.onMiss) {
        this._config.hooks.onMiss(key);
      }
    }
    return unwrapped;
  }

  async getAsync(key) {
    validateKey(key);
    // 1. Check JS-local L1 cache (FIFO read - zero writes on hit)
    if (this._l1Capacity > 0) {
      const l1Val = this._l1Get(key);
      if (l1Val !== undefined) {
        return l1Val;
      }
    }

    if (this._l2Enabled) {
      // 2. Check Native L2 Cache
      const slot = this._l1Slot(key);
      const gen = this._l1Gen(slot);
      let nativeVal = this._nativeGet(key);

      // Check if expired
      if (nativeVal && typeof nativeVal === 'object' && nativeVal.__expired) {
        this._onExpired(key, nativeVal, true);
        return undefined;
      }

      let remainingTtl;
      if (Array.isArray(nativeVal)) {
        remainingTtl = nativeVal[1];
        nativeVal = nativeVal[0];
      }

      const unwrapped = unwrapValue(nativeVal);
      if (unwrapped !== undefined) {
        this._l1Fill(key, unwrapped, remainingTtl, slot, gen);
        return unwrapped;
      }
    }

    // 3. Check L3 Cache (Redis)
    if (this._redis) {
      const { value: redisVal, ttlMs: remainingTtl } = await this._redis.get(key);
      if (redisVal !== undefined) {
        this._backfillFromL3(key, redisVal, remainingTtl);
        return redisVal;
      }
    }

    if (this._config.hooks && this._config.hooks.onMiss) {
      this._config.hooks.onMiss(key);
    }
    return undefined;
  }

  peek(key) {
    validateKey(key);
    if (this._l1Capacity > 0) {
      const l1Val = this._l1Get(key);
      if (l1Val !== undefined) {
        return l1Val;
      }
    }
    if (!this._l2Enabled) {
      return undefined;
    }
    const nativeVal = this._native.peek(key);
    if (nativeVal && typeof nativeVal === 'object' && nativeVal.__expired) {
      this._onExpired(key, nativeVal, false);
      return undefined;
    }
    return unwrapValue(nativeVal);
  }

  has(key) {
    validateKey(key);
    if (this._l1Capacity > 0 && this._l1Get(key) !== undefined) {
      return true;
    }
    if (!this._l2Enabled) {
      return false;
    }
    const nativeVal = this._native.has(key);
    if (nativeVal && typeof nativeVal === 'object' && nativeVal.__expired) {
      this._onExpired(key, nativeVal, false);
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

    if (this._l2Enabled) {
      this._noteTtl(ttlMs);
      const forceCompression = forceCompressionFor(wrapped, compression, minSizeBytes);
      this._emitEvictions(this._native.set(key, wrapped, ttlMs, forceCompression));
    } else {
      // In L1-only mode, we set the value directly in L1 on write
      this._l1SetLocal(key, value, ttlMs);
    }

    if (this._redis) {
      this._redis.setRaw(key, wrapped, ttlMs).catch(err => this._reportRedisError(err, 'set', key));
    }
  }

  touch(key, ttlMs) {
    validateKey(key);
    if (!this._l2Enabled) {
      if (this._l1Get(key) === undefined) return false;
      const ent = this._l1.get(key);
      ent.expiresAt = ttlMs > 0 ? Date.now() + ttlMs : 0;
      ent.slidingMs = this._sliding && ttlMs > 0 ? ttlMs : 0;
      return true;
    }
    this._l1.delete(key); // Invalidate L1 on TTL update
    this._noteTtl(ttlMs);
    const nativeVal = this._native.touch(key, ttlMs);
    if (nativeVal && typeof nativeVal === 'object' && nativeVal.__expired) {
      this._onExpired(key, nativeVal, false);
      return false;
    }
    return !!nativeVal;
  }

  delete(key) {
    validateKey(key);
    const l1Deleted = this._l1.delete(key);
    let result = false;
    if (this._l2Enabled) {
      result = this._native.delete(key);
    } else {
      result = l1Deleted;
    }
    if (this._redis) {
      this._redis.delete(key).catch(err => this._reportRedisError(err, 'delete', key));
    }
    return result;
  }

  clear() {
    this._l1.clear();
    if (this._l2Enabled) {
      this._native.clear();
    }
  }

  stats() {
    if (!this._l2Enabled) {
      return {
        hits: 0,
        misses: 0,
        capacity: 0,
        size: 0,
        bytesUsed: 0,
        sets: 0,
        deletes: 0,
        evictions: 0,
        expirations: 0,
        hitRate: 0,
        uptimeMs: 0,
        shards: {
          count: 0,
          details: [],
          sizeStdDev: 0,
        },
        memory: {
          payloadBytes: 0,
          processRss: process.memoryUsage().rss,
        },
        l1: {
          size: this._l1.size,
          capacity: this._l1Capacity,
          hits: this._l1Hits,
        },
        l2: {
          enabled: false,
        },
        l3: this._redis ? { connected: this._redis.isReady() } : null,
      };
    }

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
        hits: this._l1Hits,
      },
      l2: {
        enabled: true,
      },
      l3: this._redis ? { connected: this._redis.isReady() } : null,
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
    if (!this._l2Enabled) {
      return Array.from(this._l1.keys());
    }
    return this._native.keys();
  }

  // L1-only counters keep the key's current deadline unless a new TTL is given.
  _l1Counter(key, delta, ttlMs) {
    // Note: When L2 is disabled (l2.enabled = false), increment/decrement operations run locally
    // in V8 heap Map. State is thread-local and NOT shared across worker_threads.
    const current = this._l1Get(key);
    const val = Number(current !== undefined ? current : 0) + delta;
    if (ttlMs > 0 || current === undefined) {
      this._l1SetLocal(key, val, ttlMs);
    } else {
      const prev = this._l1.get(key);
      const remaining = prev.expiresAt === 0 ? undefined : prev.expiresAt - Date.now();
      this._l1Set(key, val, remaining, 0, 0, prev.slidingMs);
    }
    return val;
  }

  // L2 now holds a value L3 doesn't know about; drop the L3 copy so it can't be served stale.
  _invalidateL3(key) {
    if (this._redis) {
      this._redis.delete(key).catch(err => this._reportRedisError(err, 'delete', key));
    }
  }

  increment(key, delta = 1, ttlMs) {
    validateKey(key);
    if (!this._l2Enabled) {
      return this._l1Counter(key, delta, ttlMs);
    }
    this._l1.delete(key); // Invalidate L1 on mutation
    this._noteTtl(ttlMs);
    const val = this._native.increment(key, delta, ttlMs);
    this._invalidateL3(key);
    return val;
  }

  decrement(key, delta = 1, ttlMs) {
    validateKey(key);
    if (!this._l2Enabled) {
      return this._l1Counter(key, -delta, ttlMs);
    }
    this._l1.delete(key); // Invalidate L1 on mutation
    this._noteTtl(ttlMs);
    const val = this._native.decrement(key, delta, ttlMs);
    this._invalidateL3(key);
    return val;
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
        const val = this._l1Get(key);
        if (val !== undefined) {
          result[key] = val;
          continue;
        }
      }
      missing.push(key);
    }

    if (this._l2Enabled && missing.length > 0) {
      // Generation snapshots must precede the native read (see _l1Set).
      const slots = this._gens === null ? null : missing.map((key) => this._l1Slot(key));
      const gens = slots === null ? null : slots.map((slot) => this._l1Gen(slot));
      const nativeValues = this._ttlAware ? this._native.mgetWithTtl(missing) : this._native.mget(missing);
      for (let i = 0; i < missing.length; i++) {
        const key = missing[i];
        let valRaw = nativeValues[i];

        if (valRaw && typeof valRaw === 'object' && valRaw.__expired) {
          this._onExpired(key, valRaw, true);
        } else {
          let remainingTtl;
          if (Array.isArray(valRaw)) {
            remainingTtl = valRaw[1];
            valRaw = valRaw[0];
          }
          const val = unwrapValue(valRaw);
          if (val !== undefined) {
            result[key] = val;
            this._l1Fill(key, val, remainingTtl, slots === null ? 0 : slots[i], gens === null ? 0 : gens[i]);
          } else {
            if (this._config.hooks && this._config.hooks.onMiss) {
              this._config.hooks.onMiss(key);
            }
          }
        }
      }
    } else if (missing.length > 0) {
      // If L2 is disabled, any missing key is a miss
      if (this._config.hooks && this._config.hooks.onMiss) {
        for (const key of missing) {
          this._config.hooks.onMiss(key);
        }
      }
    }

    return result;
  }

  mset(entries, ttlMs) {
    if (typeof entries !== 'object' || entries === null) {
      throw new Error('mset requires an object of key-value entries');
    }
    
    // Same default TTL as set()
    if (ttlMs === undefined && this._config.ttl) {
      ttlMs = this._config.ttl.defaultMs;
    }

    // Serialized only when a layer outside the V8 heap needs it
    const needsWrap = this._l2Enabled || this._redis;
    const wrapped = {};
    for (const k in entries) {
      validateKey(k);
      if (needsWrap) wrapped[k] = wrapValue(entries[k]);
    }

    if (this._l2Enabled) {
      for (const k in wrapped) {
        this._l1.delete(k); // Invalidate L1
      }
      this._noteTtl(ttlMs);
      this._emitEvictions(this._native.mset(wrapped, ttlMs));
    } else {
      for (const k in entries) {
        this._l1SetLocal(k, entries[k], ttlMs);
      }
    }

    if (this._redis) {
      for (const k in wrapped) {
        this._redis.setRaw(k, wrapped[k], ttlMs).catch(err => this._reportRedisError(err, 'set', k));
      }
    }
  }

  mdelete(keys) {
    if (!Array.isArray(keys)) {
      throw new Error('mdelete requires an array of keys');
    }
    let count = 0;
    for (const k of keys) {
      validateKey(k);
      if (this._l1.delete(k)) {
        count++;
      }
      if (this._redis) {
        this._redis.delete(k).catch(err => this._reportRedisError(err, 'delete', k));
      }
    }
    if (this._l2Enabled) {
      return this._native.mdelete(keys);
    }
    return count;
  }

  // Removes expired entries that were never read again. Scans up to `maxScanPerShard` slab
  // slots per shard (default: all) and fires onExpire for each. Returns how many were removed.
  purgeExpired(maxScanPerShard) {
    if (!this._l2Enabled) {
      const now = Date.now();
      let count = 0;
      for (const [key, ent] of this._l1) {
        if (ent.expiresAt !== 0 && ent.expiresAt <= now) {
          this._l1.delete(key);
          count++;
        }
      }
      return count;
    }
    if (typeof this._native.purgeExpired !== 'function') return 0;
    const collect = !!(this._config.hooks && this._config.hooks.onExpire);
    const res = this._native.purgeExpired(maxScanPerShard, collect);
    if (!collect) return res;
    this._emitEvictions(res);
    return res.length;
  }

  _stopSweep() {
    if (this._sweepTimer !== null) {
      clearInterval(this._sweepTimer);
      this._sweepTimer = null;
    }
  }

  dispose() {
    this._stopSweep();
    this._l1.clear();
    if (this._l2Enabled) {
      this._native.dispose();
    }
  }

  // Like getOrSet(), but always returns a Promise, whether the value was cached or the
  // factory is synchronous.
  async getOrSetAsync(key, factory, ttlMs) {
    return this.getOrSet(key, factory, ttlMs);
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

    // If L3 Redis is not configured, run the original fast-path
    if (!this._redis) {
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

    // L3 is configured: Single-flight covering both L3 check + factory
    const p = (async () => {
      // 3a. L3 check
      const { value: redisVal, ttlMs: remainingTtl } = await this._redis.get(key);
      if (redisVal !== undefined) {
        this._backfillFromL3(key, redisVal, remainingTtl);
        return redisVal;
      }

      // 3b. Factory execution (async / sync)
      const res = await (typeof factory === 'function' ? factory() : Promise.resolve(factory));
      if (res !== undefined) {
        this.set(key, res, ttlMs);
      }
      return res;
    })()
      .finally(() => activePromises.delete(promiseKey));

    activePromises.set(promiseKey, p);
    return p;
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
    const ttlMode = merged.ttl && merged.ttl.mode;
    if (ttlMode !== undefined && ttlMode !== 'absolute' && ttlMode !== 'sliding') {
      throw new RangeError(`ttl.mode must be 'absolute' or 'sliding', got '${ttlMode}'`);
    }
    const rawConfig = {
      policy: merged.eviction.policy,
      capacity: merged.eviction.capacity,
      shards: merged.shards,
      maxBytes: merged.eviction.maxBytes,
      compression: merged.compression.enabled,
      slidingTtl: ttlMode === 'sliding',
    };
    const nativeCache = this._native.createCache(name, rawConfig);

    let redisL3 = null;
    if (merged.redis && merged.redis.client) {
      const globalPrefix = merged.redis.keyPrefix ?? 'offheap:';
      const effectivePrefix = `${globalPrefix}${name}:`;
      redisL3 = new RedisL3(merged.redis.client, effectivePrefix, merged.redis.onError);
    }

    const cache = new Cache(nativeCache, merged, redisL3);
    this._caches.set(name, cache);
    return cache;
  }

  getCache(name) {
    if (this._caches.has(name)) {
      return this._caches.get(name);
    }
    const nativeCache = this._native.getCache(name);
    if (!nativeCache) return null;

    let redisL3 = null;
    if (this._globalConfig.redis && this._globalConfig.redis.client) {
      const globalPrefix = this._globalConfig.redis.keyPrefix ?? 'offheap:';
      const effectivePrefix = `${globalPrefix}${name}:`;
      redisL3 = new RedisL3(this._globalConfig.redis.client, effectivePrefix, this._globalConfig.redis.onError);
    }

    const cache = new Cache(nativeCache, this._globalConfig, redisL3);
    this._caches.set(name, cache);
    return cache;
  }

  deleteCache(name) {
    const cache = this._caches.get(name);
    if (cache) cache._stopSweep();
    this._caches.delete(name);
    return this._native.deleteCache(name);
  }

  clear() {
    for (const cache of this._caches.values()) cache._stopSweep();
    this._caches.clear();
    this._native.clear();
  }

  dispose() {
    this.clear();
  }
}

module.exports = {
  Cache,
  CacheManager,
  /** Version of the loaded native addon (null for binaries older than 0.5.0). */
  nativeVersion: NATIVE_VERSION,
};
