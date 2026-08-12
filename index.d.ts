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
}

export interface TtlConfig {
  defaultMs?: number;
  /**
   * TTL renewal mode.
   * - `'absolute'` (default): TTL is fixed from the time of `set()`.
   * - `'sliding'`: not yet implemented. Use `touch()` to manually renew TTL.
   * @todo Implement sliding TTL in a future release.
   */
  mode?: 'absolute'; // 'sliding' is declared but not yet implemented
}

export interface MonitoringConfig {
  minIntervalMs?: number;
}

export interface CacheHooks {
  onEvict?: (key: string, value: any, reason: 'evicted' | 'replaced') => void;
  onExpire?: (key: string, value: any) => void;
  onMiss?: (key: string) => void;
}

export interface CacheConfig {
  shards?: number;
  eviction?: EvictionConfig;
  compression?: CompressionConfig;
  l1?: L1Config;
  ttl?: TtlConfig;
  hooks?: CacheHooks;
  monitoring?: MonitoringConfig;

  // Legacy flat options (still supported for backwards compatibility)
  /** @deprecated Use `eviction.policy` instead. */
  policy?: 'lru' | 'arc' | 'tinylfu' | 'w-tinylfu';
  /** @deprecated Use `eviction.capacity` instead. */
  capacity?: number;
  /** @deprecated Use `eviction.maxBytes` instead. */
  maxBytes?: number;
  /** @deprecated Use `l1.capacity` instead. */
  l1Capacity?: number;
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
  };
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
  peek<T = any>(key: string): T | undefined;
  has(key: string): boolean;

  /**
   * Stores a value in the cache.
   * Returns `void`. Use `get()` to retrieve the stored value.
   */
  set<T = any>(key: string, value: T, ttlMsOrOptions?: number | SetOptions): void;

  touch(key: string, ttlMs?: number): boolean;
  delete(key: string): boolean;
  clear(): void;
  stats(): CacheStats;
  keys(): string[];
  increment(key: string, delta?: number, ttlMs?: number): number;
  decrement(key: string, delta?: number, ttlMs?: number): number;

  /**
   * Retrieves multiple keys in a single call.
   * Keys that are not found (misses) will be absent from the result object.
   */
  mget<T = any>(keys: string[]): Partial<Record<string, T>>;

  mset(entries: Record<string, any>, ttlMs?: number): void;
  mdelete(keys: string[]): number;
  dispose(): void;

  /**
   * Returns a cached value if it exists, otherwise calls the factory and caches the result.
   * Concurrent calls for the same key are coalesced — the factory is only called once.
   */
  getOrSet<T>(key: string, factory: () => Promise<T>, ttlMs?: number): Promise<T>;
  getOrSet<T>(key: string, factory: () => T, ttlMs?: number): T;

  /**
   * Starts a periodic monitoring loop that calls `callback` with a stats snapshot.
   *
   * @param callback Called on each monitoring interval with delta ops and rate metrics.
   * @param intervalMs Polling interval in milliseconds.
   *   - Defaults to `monitoring.minIntervalMs` (default: `500ms`).
   *   - Hard floor: `16ms` regardless of configuration.
   * @returns A disposer function — call it to stop the monitoring loop.
   */
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
