// IndexedDB implementation of StorageBackend. Multi-store key-value storage
// with index traversal, prefix scans, atomic cross-store transactions, and
// quota/persistence helpers that degrade gracefully on browsers lacking the
// StorageManager API. The database opens lazily on first access and the schema
// is created from the supplied IndexedDBConfig in `onupgradeneeded`.

import type {
  IndexedDBConfig,
  QuotaInfo,
  StorageBackend,
  StorageTransaction,
} from "./backend";

export class IndexedDBStorageBackend implements StorageBackend {
  private dbPromise: Promise<IDBDatabase> | null = null;

  constructor(private readonly config: IndexedDBConfig) {}

  /** Lazily open the database, creating object stores + indices on upgrade. */
  private async getDB(): Promise<IDBDatabase> {
    if (!this.dbPromise) {
      this.dbPromise = new Promise<IDBDatabase>((resolve, reject) => {
        const request = indexedDB.open(this.config.dbName, this.config.version);

        request.onerror = () => reject(request.error);
        request.onsuccess = () => resolve(request.result);

        request.onupgradeneeded = () => {
          const db = request.result;
          for (const storeConfig of this.config.stores) {
            if (db.objectStoreNames.contains(storeConfig.name)) continue;
            const store = db.createObjectStore(storeConfig.name, {
              keyPath: storeConfig.keyPath,
              autoIncrement: storeConfig.autoIncrement,
            });
            for (const indexConfig of storeConfig.indices ?? []) {
              store.createIndex(indexConfig.name, indexConfig.keyPath, {
                unique: indexConfig.unique,
              });
            }
          }
        };
      });
    }
    return this.dbPromise;
  }

  private promisifyRequest<T>(request: IDBRequest<T>): Promise<T> {
    return new Promise<T>((resolve, reject) => {
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
  }

  async get<T = unknown>(storeName: string, key: string): Promise<T | null> {
    const db = await this.getDB();
    const tx = db.transaction(storeName, "readonly");
    const store = tx.objectStore(storeName);
    const result = await this.promisifyRequest(store.get(key));
    return (result ?? null) as T | null;
  }

  async set<T = unknown>(storeName: string, key: string, value: T): Promise<void> {
    const db = await this.getDB();
    const tx = db.transaction(storeName, "readwrite");
    const store = tx.objectStore(storeName);
    // In-line key (keyPath present) → pass value only; out-of-line → pass key.
    if (store.keyPath) {
      await this.promisifyRequest(store.put(value));
    } else {
      await this.promisifyRequest(store.put(value, key));
    }
  }

  async delete(storeName: string, key: string): Promise<void> {
    const db = await this.getDB();
    const tx = db.transaction(storeName, "readwrite");
    const store = tx.objectStore(storeName);
    await this.promisifyRequest(store.delete(key));
  }

  async keys(storeName: string, prefix?: string): Promise<string[]> {
    const db = await this.getDB();
    const tx = db.transaction(storeName, "readonly");
    const store = tx.objectStore(storeName);
    if (prefix) {
      // Half-open key range covering every key that starts with `prefix`.
      const range = IDBKeyRange.bound(prefix, `${prefix}￿`, false, false);
      const keys = await this.promisifyRequest(store.getAllKeys(range));
      return keys.map((k) => String(k));
    }
    const keys = await this.promisifyRequest(store.getAllKeys());
    return keys.map((k) => String(k));
  }

  async getAllFromIndex<T = unknown>(
    storeName: string,
    indexName: string,
    direction: "asc" | "desc" = "asc",
  ): Promise<T[]> {
    const db = await this.getDB();
    const tx = db.transaction(storeName, "readonly");
    const store = tx.objectStore(storeName);
    const index = store.index(indexName);

    return new Promise<T[]>((resolve, reject) => {
      const results: T[] = [];
      const request = index.openCursor(null, direction === "desc" ? "prev" : "next");
      request.onsuccess = () => {
        const cursor = request.result;
        if (cursor) {
          results.push(cursor.value as T);
          cursor.continue();
        } else {
          resolve(results);
        }
      };
      request.onerror = () => reject(request.error);
    });
  }

  async clear(storeName: string): Promise<void> {
    const db = await this.getDB();
    const tx = db.transaction(storeName, "readwrite");
    const store = tx.objectStore(storeName);
    await this.promisifyRequest(store.clear());
  }

  async has(storeName: string, key: string): Promise<boolean> {
    const db = await this.getDB();
    const tx = db.transaction(storeName, "readonly");
    const store = tx.objectStore(storeName);
    const result = await this.promisifyRequest(store.getKey(key));
    return result !== undefined;
  }

  async transaction<T>(
    storeNames: string[],
    mode: "readonly" | "readwrite",
    operation: (tx: StorageTransaction) => Promise<T>,
  ): Promise<T> {
    const db = await this.getDB();
    const idbTx = db.transaction(storeNames, mode);

    const storageTx: StorageTransaction = {
      get: async <V>(storeName: string, key: string) => {
        const store = idbTx.objectStore(storeName);
        const result = await this.promisifyRequest(store.get(key));
        return (result ?? null) as V | null;
      },
      set: async <V>(storeName: string, key: string, value: V) => {
        const store = idbTx.objectStore(storeName);
        if (store.keyPath) {
          await this.promisifyRequest(store.put(value));
        } else {
          await this.promisifyRequest(store.put(value, key));
        }
      },
      delete: async (storeName: string, key: string) => {
        const store = idbTx.objectStore(storeName);
        await this.promisifyRequest(store.delete(key));
      },
    };

    return operation(storageTx);
  }

  async getQuotaInfo(): Promise<QuotaInfo> {
    if (navigator.storage?.estimate) {
      const estimate = await navigator.storage.estimate();
      const usage = estimate.usage ?? 0;
      const quota = estimate.quota ?? 0;
      return { usage, quota, percent: quota ? (usage / quota) * 100 : 0 };
    }
    return { usage: 0, quota: 0, percent: 0 };
  }

  async requestPersistentStorage(): Promise<boolean> {
    if (navigator.storage?.persist) {
      return navigator.storage.persist();
    }
    return false;
  }
}
