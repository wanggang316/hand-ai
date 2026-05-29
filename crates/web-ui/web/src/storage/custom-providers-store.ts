// Store for custom LLM providers (auto-discovery servers + manual providers),
// keyed by UUID. Auto-discovery providers fetch their models on demand; manual
// providers persist their models on the record. The store uses out-of-line
// keys with the provider's `id` as the key.

import type { CustomProvider, StoreConfig } from "./backend";
import { Store } from "./store";

const CUSTOM_PROVIDERS = "custom-providers";

export class CustomProvidersStore extends Store {
  getConfig(): StoreConfig {
    return { name: CUSTOM_PROVIDERS };
  }

  async get(id: string): Promise<CustomProvider | null> {
    return this.getBackend().get<CustomProvider>(CUSTOM_PROVIDERS, id);
  }

  async set(provider: CustomProvider): Promise<void> {
    await this.getBackend().set(CUSTOM_PROVIDERS, provider.id, provider);
  }

  async delete(id: string): Promise<void> {
    await this.getBackend().delete(CUSTOM_PROVIDERS, id);
  }

  async getAll(): Promise<CustomProvider[]> {
    const keys = await this.getBackend().keys(CUSTOM_PROVIDERS);
    const providers: CustomProvider[] = [];
    for (const key of keys) {
      const provider = await this.get(key);
      if (provider) providers.push(provider);
    }
    return providers;
  }

  async has(id: string): Promise<boolean> {
    return this.getBackend().has(CUSTOM_PROVIDERS, id);
  }
}
