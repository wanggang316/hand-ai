// Abstract base for all stores. Each concrete store declares its IndexedDB
// schema via getConfig() and accesses the shared backend through getBackend().
// AppStorage wires the backend into every store after constructing it.

import type { StorageBackend, StoreConfig } from "./backend";

export abstract class Store {
  private backend: StorageBackend | null = null;

  /** Returns the IndexedDB store schema (name, key path, indices). */
  abstract getConfig(): StoreConfig;

  /** Sets the storage backend. Called by AppStorage after backend creation. */
  setBackend(backend: StorageBackend): void {
    this.backend = backend;
  }

  /** Gets the storage backend. Throws if the backend has not been set. */
  protected getBackend(): StorageBackend {
    if (!this.backend) {
      throw new Error(`Backend not set on ${this.constructor.name}`);
    }
    return this.backend;
  }
}
