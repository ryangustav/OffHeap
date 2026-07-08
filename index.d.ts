export interface CacheConfig {
  policy: 'lru' | 'arc' | 'tinylfu';
  capacity: number;
  shards?: number;
  maxBytes?: number;
  l1Capacity?: number;
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
  set<T = any>(key: string, value: T, ttlMs?: number): T | undefined;
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
  constructor();
  createCache(name: string, config: CacheConfig): Cache;
  getCache(name: string): Cache | null;
  deleteCache(name: string): boolean;
  clear(): void;
  dispose(): void;
}
