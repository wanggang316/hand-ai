// High-level storage facade exposing the four domain stores plus quota and
// persistence helpers, with a process-global singleton accessor. The bootstrap
// constructs the stores and an IndexedDBStorageBackend, wires the backend into
// every store, and registers the AppStorage via setAppStorage().

import type { IndexedDBConfig, QuotaInfo, StorageBackend } from "./backend";
import type { CustomProvidersStore } from "./custom-providers-store";
import type { ProviderKeysStore } from "./provider-keys-store";
import { SessionsStore } from "./sessions-store";
import type { SettingsStore } from "./settings-store";

export class AppStorage {
  readonly backend: StorageBackend;
  readonly settings: SettingsStore;
  readonly providerKeys: ProviderKeysStore;
  readonly sessions: SessionsStore;
  readonly customProviders: CustomProvidersStore;

  constructor(
    settings: SettingsStore,
    providerKeys: ProviderKeysStore,
    sessions: SessionsStore,
    customProviders: CustomProvidersStore,
    backend: StorageBackend,
  ) {
    this.settings = settings;
    this.providerKeys = providerKeys;
    this.sessions = sessions;
    this.customProviders = customProviders;
    this.backend = backend;
  }

  async getQuotaInfo(): Promise<QuotaInfo> {
    return this.backend.getQuotaInfo();
  }

  async requestPersistentStorage(): Promise<boolean> {
    return this.backend.requestPersistentStorage();
  }
}

/**
 * Assemble the full IndexedDB schema config from the four stores. The sessions
 * store contributes BOTH its primary config and the companion metadata-store
 * config so the dual-store transactions resolve. Bump `version` to add stores.
 */
export function buildStorageConfig(
  dbName: string,
  stores: {
    settings: SettingsStore;
    providerKeys: ProviderKeysStore;
    sessions: SessionsStore;
    customProviders: CustomProvidersStore;
  },
  version = 1,
): IndexedDBConfig {
  return {
    dbName,
    version,
    stores: [
      stores.settings.getConfig(),
      stores.providerKeys.getConfig(),
      stores.sessions.getConfig(),
      SessionsStore.getMetadataConfig(),
      stores.customProviders.getConfig(),
    ],
  };
}

// ---- global singleton -------------------------------------------------------

let globalAppStorage: AppStorage | null = null;

/** Get the global AppStorage instance. Throws if not yet initialized. */
export function getAppStorage(): AppStorage {
  if (!globalAppStorage) {
    throw new Error("AppStorage not initialized. Call setAppStorage() first.");
  }
  return globalAppStorage;
}

/** Set the global AppStorage instance. */
export function setAppStorage(storage: AppStorage): void {
  globalAppStorage = storage;
}
