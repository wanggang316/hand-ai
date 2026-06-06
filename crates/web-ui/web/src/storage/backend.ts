// Storage backend abstraction. A multi-store key-value interface that can be
// implemented by IndexedDB (the only backend the web app ships), a remote API,
// or any other multi-collection storage system. This module also defines the
// schema-config types and the persisted domain shapes (session data/metadata,
// custom providers) so the stores and the bootstrap can share them.

import type { AgentMessage } from "../core/messages";
import type { Model, ThinkingLevel } from "../core/model";

/**
 * Transaction interface for atomic operations across stores. Mirrors the
 * backend's get/set/delete but scoped to a single underlying transaction so a
 * group of writes either all commit or all roll back.
 */
export interface StorageTransaction {
  /** Get a value by key from a specific store. */
  get<T = unknown>(storeName: string, key: string): Promise<T | null>;
  /** Set a value for a key in a specific store. */
  set<T = unknown>(storeName: string, key: string, value: T): Promise<void>;
  /** Delete a key from a specific store. */
  delete(storeName: string, key: string): Promise<void>;
}

/**
 * Base interface for all storage backends. Multi-store key-value storage
 * abstraction that can be implemented by IndexedDB, remote APIs, or any other
 * multi-collection storage system.
 */
export interface StorageBackend {
  /** Get a value by key from a specific store. Returns null if absent. */
  get<T = unknown>(storeName: string, key: string): Promise<T | null>;
  /** Set a value for a key in a specific store. */
  set<T = unknown>(storeName: string, key: string, value: T): Promise<void>;
  /** Delete a key from a specific store. */
  delete(storeName: string, key: string): Promise<void>;
  /** Get all keys from a specific store, optionally filtered by prefix. */
  keys(storeName: string, prefix?: string): Promise<string[]>;
  /**
   * Get all values from a specific store, ordered by an index.
   * @param storeName - The store to query.
   * @param indexName - The index to traverse for ordering.
   * @param direction - Sort direction ("asc" or "desc").
   */
  getAllFromIndex<T = unknown>(
    storeName: string,
    indexName: string,
    direction?: "asc" | "desc",
  ): Promise<T[]>;
  /** Clear all data from a specific store. */
  clear(storeName: string): Promise<void>;
  /** Check if a key exists in a specific store. */
  has(storeName: string, key: string): Promise<boolean>;
  /** Execute atomic operations across multiple stores in one transaction. */
  transaction<T>(
    storeNames: string[],
    mode: "readonly" | "readwrite",
    operation: (tx: StorageTransaction) => Promise<T>,
  ): Promise<T>;
  /**
   * Get storage quota information. Used to warn users approaching limits.
   * Returns zeros gracefully on browsers without the StorageManager API.
   */
  getQuotaInfo(): Promise<QuotaInfo>;
  /**
   * Request persistent storage (prevents eviction). Returns true if granted,
   * false if denied or unsupported.
   */
  requestPersistentStorage(): Promise<boolean>;
}

/** Storage usage estimate. `percent` is usage/quota * 100, 0 when unknown. */
export interface QuotaInfo {
  usage: number;
  quota: number;
  percent: number;
}

// ---- schema configuration ---------------------------------------------------

/** Configuration for the IndexedDB backend: database name, version, stores. */
export interface IndexedDBConfig {
  /** Database name. */
  dbName: string;
  /** Database version; bump to trigger an upgrade that creates new stores. */
  version: number;
  /** Object stores to create. */
  stores: StoreConfig[];
}

/** Configuration for a single IndexedDB object store. */
export interface StoreConfig {
  /** Store name. */
  name: string;
  /** Key path (optional, for auto-extracting in-line keys from objects). */
  keyPath?: string;
  /** Auto-increment keys (optional). */
  autoIncrement?: boolean;
  /** Indices to create on this store. */
  indices?: IndexConfig[];
}

/** Configuration for an IndexedDB index. */
export interface IndexConfig {
  /** Index name. */
  name: string;
  /** Key path to index on. */
  keyPath: string;
  /** Unique constraint (optional). */
  unique?: boolean;
}

// ---- persisted domain shapes ------------------------------------------------

/** Cumulative cost breakdown carried in session metadata. */
export interface SessionUsageCost {
  input: number;
  output: number;
  cacheRead: number;
  cacheWrite: number;
  total: number;
}

/** Cumulative usage statistics carried in session metadata. */
export interface SessionUsage {
  input: number;
  output: number;
  cacheRead: number;
  cacheWrite: number;
  totalTokens: number;
  cost: SessionUsageCost;
}

/**
 * Lightweight session metadata for listing and searching. Stored separately
 * from full session data so the session list loads without deserializing every
 * transcript.
 */
export interface SessionMetadata {
  /** Unique session identifier (UUID v4). */
  id: string;
  /** User-defined title or auto-generated from the first message. */
  title: string;
  /** ISO 8601 UTC timestamp of creation. */
  createdAt: string;
  /** ISO 8601 UTC timestamp of last modification. */
  lastModified: string;
  /** Total number of messages (user + assistant + tool results). */
  messageCount: number;
  /** Cumulative usage statistics. */
  usage: SessionUsage;
  /** Last used thinking level. */
  thinkingLevel: ThinkingLevel;
  /**
   * Preview text for search and display: leading conversation text
   * (user + assistant messages); tool calls and tool results are excluded.
   */
  preview: string;
}

/** Full session data including all messages. Loaded only when a session opens. */
export interface SessionData {
  /** Unique session identifier (UUID v4). */
  id: string;
  /** User-defined title or auto-generated from the first message. */
  title: string;
  /** Last selected model. */
  model: Model;
  /** Last selected thinking level. */
  thinkingLevel: ThinkingLevel;
  /** Full conversation history (with attachments inline). */
  messages: AgentMessage[];
  /** ISO 8601 UTC timestamp of creation. */
  createdAt: string;
  /** ISO 8601 UTC timestamp of last modification. */
  lastModified: string;
}

/** Custom provider kinds: auto-discovery servers + manual API shapes. */
export type AutoDiscoveryProviderType = "ollama" | "llama.cpp" | "vllm" | "lmstudio";

export type CustomProviderType =
  | AutoDiscoveryProviderType
  | "openai-completions"
  | "openai-responses"
  | "anthropic-messages";

/** A user-configured custom LLM provider, keyed by UUID. */
export interface CustomProvider {
  /** UUID. */
  id: string;
  /** Display name; also used as `Model.provider`. */
  name: string;
  type: CustomProviderType;
  baseUrl: string;
  /** Optional API key applied to all of the provider's models. */
  apiKey?: string;
  /**
   * Manual-type providers store their models directly. Auto-discovery types
   * fetch models on demand and never persist them here.
   */
  models?: Model[];
}
