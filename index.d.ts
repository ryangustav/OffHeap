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
  mode?: 'absolute' | 'sliding';
}

export interface CacheConfig {
  shards?: number;
  eviction?: EvictionConfig;
  compression?: CompressionConfig;
  l1?: L1Config;
  ttl?: TtlConfig;
  
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

export interface CacheStats {
  hits: number;
  misses: number;
  capacity: number;
  size: number;
  bytesUsed: number;
}

export class Cache {
  get<T = any>(key: string): T | undefined;
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
}

export class CacheManager {
  constructor(globalConfig?: CacheConfig);
  createCache(name: string, config?: CacheConfig): Cache;
  getCache(name: string): Cache | null;
  deleteCache(name: string): boolean;
  clear(): void;
  dispose(): void;
}
