export interface L2Config {
  enabled: boolean;
}

export interface EvictionConfig {
  policy: 'lru' | 'arc' | 'tinylfu' | 'w-tinylfu';
  capacity: number;
  maxBytes?: number;
}

export interface CompressionConfig {
  enabled: boolean;
  algorithm?: 'lz4' | 'none';
  minSizeBytes?: number;
}

export interface L1Config {
  enabled: boolean;
  capacity?: number;
  /**
   * Drop L1 copies of keys written by other threads (or other wrappers of the same cache).
   * Default `true`. Requires a runtime that allows external ArrayBuffers.
   */
  sharedInvalidation?: boolean;
}

export interface TtlConfig {
  defaultMs?: number;
  /** `sliding`: every read restarts the entry's TTL window. Default `absolute`. */
  mode?: 'absolute' | 'sliding';
  /** Periodically removes expired entries that are never read again. Off by default. */
  sweepIntervalMs?: number;
}

export interface MonitoringConfig {
  minIntervalMs?: number;
}

export interface CacheHooks {
  onEvict?: (key: string, value: any, reason: 'evicted' | 'replaced') => void;
  onExpire?: (key: string, value: any) => void;
  onMiss?: (key: string) => void;
}

export interface RedisL3Config {
  client: any;
  keyPrefix?: string;
  onError?: (err: Error, op: 'get' | 'set' | 'delete', key: string) => void;
}

export interface CacheConfig {
  /** Default: 4 per CPU (power of two), reduced so every shard holds at least 64 entries. */
  shards?: number;
  eviction?: EvictionConfig;
  compression?: CompressionConfig;
  l1?: L1Config;
  l2?: L2Config;
  ttl?: TtlConfig;
  hooks?: CacheHooks;
  monitoring?: MonitoringConfig;
  redis?: RedisL3Config;
  
  // Legacy options (Backward Compatibility)
  policy?: 'lru' | 'arc' | 'tinylfu' | 'w-tinylfu';
  capacity?: number;
  maxBytes?: number;
  l1Capacity?: number;
  compressionEnabled?: boolean;
}

export interface SetOptions {
  ttlMs?: number;
  compression?: boolean;
  minSizeBytes?: number;
}

export interface ShardDetail {
  size: number;
  bytesUsed: number;
}

export interface ShardsStats {
  count: number;
  details: ShardDetail[];
  sizeStdDev: number;
}

export interface MemoryStats {
  payloadBytes: number;
  processRss: number;
}

export interface CacheStats {
  hits: number;
  misses: number;
  capacity: number;
  size: number;
  bytesUsed: number;
  sets: number;
  deletes: number;
  evictions: number;
  expirations: number;
  hitRate: number;
  uptimeMs: number;
  shards: ShardsStats;
  memory: MemoryStats;
  l1: {
    size: number;
    capacity: number;
    /** Reads served by L1. They never reach L2, so they are not counted in `hits`. */
    hits: number;
  };
  l2: {
    enabled: boolean;
  };
  l3: {
    connected: boolean;
  } | null;
}

export interface StatusDelta {
  hits: number;
  misses: number;
  sets: number;
  deletes: number;
  evictions: number;
  expirations: number;
}

export interface StatusRates {
  opsPerSec: number;
  hitsPerSec: number;
  missesPerSec: number;
  setsPerSec: number;
  hitRate: number | null;
}

export interface StatusSnapshot {
  totals: StatusDelta;
  delta: StatusDelta;
  rates: StatusRates;
  intervalMs: number;
  timestamp: number;
}

export class Cache {
  get<T = any>(key: string): T | undefined;
  getAsync<T = any>(key: string): Promise<T | undefined>;
  peek<T = any>(key: string): T | undefined;
  has(key: string): boolean;
  set<T = any>(key: string, value: T, ttlMsOrOptions?: number | SetOptions): T | undefined;
  touch(key: string, ttlMs?: number): boolean;
  delete(key: string): boolean;
  clear(): void;
  stats(): CacheStats;
  keys(): string[];
  increment(key: string, delta?: number, ttlMs?: number): number;
  decrement(key: string, delta?: number, ttlMs?: number): number;
  mget(keys: string[]): Record<string, any>;
  mset(entries: Record<string, any>, ttlMs?: number): void;
  mdelete(keys: string[]): number;
  dispose(): void;
  getOrSet<T = any>(key: string, factory: () => T | Promise<T>, ttlMs?: number): T | Promise<T>;
  /** Like `getOrSet`, but always returns a Promise. */
  getOrSetAsync<T = any>(key: string, factory: () => T | Promise<T>, ttlMs?: number): Promise<T>;
  /**
   * Removes expired entries that were never read again, firing `onExpire` for each.
   * Scans up to `maxScanPerShard` slots per shard (default: all). Returns the number removed.
   */
  purgeExpired(maxScanPerShard?: number): number;
  monitor(callback: (snapshot: StatusSnapshot) => void, intervalMs?: number): () => void;
}

export class CacheManager {
  constructor(globalConfig?: CacheConfig);
  createCache(name: string, config?: CacheConfig): Cache;
  getCache(name: string): Cache | null;
  deleteCache(name: string): boolean;
  clear(): void;
  dispose(): void;
}

/** Version of the loaded native addon, or `null` for binaries older than 0.5.0. */
export declare const nativeVersion: string | null;
