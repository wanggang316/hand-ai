// Store for LLM provider API keys, keyed per provider. `list()` returns only
// the provider names that have a key, never the key values themselves, so the
// UI can show a stored/not-stored indicator without exposing secrets. Keys are
// stored unencrypted at rest in the browser (documented architecture decision);
// the server resolves keys from its own environment for real LLM calls.

import type { StoreConfig } from "./backend";
import { Store } from "./store";

export class ProviderKeysStore extends Store {
  getConfig(): StoreConfig {
    return { name: "provider-keys" };
  }

  async get(provider: string): Promise<string | null> {
    return this.getBackend().get<string>("provider-keys", provider);
  }

  async set(provider: string, key: string): Promise<void> {
    await this.getBackend().set("provider-keys", provider, key);
  }

  async delete(provider: string): Promise<void> {
    await this.getBackend().delete("provider-keys", provider);
  }

  /** Provider names that have a stored key. Never returns key values. */
  async list(): Promise<string[]> {
    return this.getBackend().keys("provider-keys");
  }

  async has(provider: string): Promise<boolean> {
    return this.getBackend().has("provider-keys", provider);
  }
}
