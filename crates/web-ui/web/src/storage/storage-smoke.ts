// Headless-verifiable smoke test for the IndexedDB storage round-trip and the
// SessionsStore atomic dual-store behavior. Builds a backend against a UNIQUE
// database name (so it never collides with the real `hand-ai` database),
// exercises save → get → getAllMetadata → updateTitle → delete, and returns a
// summary of what was observed. rAF-free: it awaits only IndexedDB promises.
//
// Invoke from a browser console after the bundle loads, e.g.
//   import("/src/storage/storage-smoke.ts").then((m) => m.runStorageSmoke())
// or expose it on window during dev and call runStorageSmoke().

import type { SessionData, SessionMetadata } from "./backend";
import { buildStorageConfig } from "./app-storage";
import { CustomProvidersStore } from "./custom-providers-store";
import { IndexedDBStorageBackend } from "./indexeddb-backend";
import { ProviderKeysStore } from "./provider-keys-store";
import { SessionsStore } from "./sessions-store";
import { SettingsStore } from "./settings-store";

export interface StorageSmokeResult {
  /** Whether the session was persisted without error. */
  saved: boolean;
  /** Whether get() returned the same id and message count that was saved. */
  restoredOk: boolean;
  /** Number of metadata records returned by getAllMetadata(). */
  metadataCount: number;
  /** Whether updateTitle() propagated to BOTH data and metadata records. */
  titleUpdated: boolean;
  /** Whether the session was removed from BOTH stores after delete(). */
  deletedOk: boolean;
}

export async function runStorageSmoke(): Promise<StorageSmokeResult> {
  // Unique db name per run so repeated invocations and the real database never
  // collide. IndexedDB versioning is per-db, so a fresh name is a fresh schema.
  const dbName = `hand-ai-smoke-${Date.now()}-${Math.floor(Math.random() * 1e6)}`;

  const settings = new SettingsStore();
  const providerKeys = new ProviderKeysStore();
  const sessions = new SessionsStore();
  const customProviders = new CustomProvidersStore();

  const config = buildStorageConfig(dbName, {
    settings,
    providerKeys,
    sessions,
    customProviders,
  });
  const backend = new IndexedDBStorageBackend(config);
  sessions.setBackend(backend);

  const id = "smoke-session";
  const now = new Date().toISOString();
  const data: SessionData = {
    id,
    title: "Original title",
    model: {
      id: "smoke/model",
      name: "smoke-model",
      api: "openai-completions",
      provider: "smoke",
      reasoning: false,
      input: ["text"],
      contextWindow: 0,
      maxTokens: 0,
    },
    thinkingLevel: "off",
    messages: [
      { role: "user", content: "hello" },
      { role: "assistant", content: [{ type: "text", text: "hi" }] },
    ],
    createdAt: now,
    lastModified: now,
  };
  const metadata: SessionMetadata = {
    id,
    title: "Original title",
    createdAt: now,
    lastModified: now,
    messageCount: data.messages.length,
    usage: {
      input: 0,
      output: 0,
      cacheRead: 0,
      cacheWrite: 0,
      totalTokens: 0,
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
    },
    thinkingLevel: "off",
    preview: "hello",
  };

  // save (atomic dual-store)
  await sessions.save(data, metadata);
  const saved = true;

  // read back via get + getAllMetadata
  const restored = await sessions.get(id);
  const allMetadata = await sessions.getAllMetadata();
  const restoredOk =
    restored?.id === id && restored.messages.length === data.messages.length;
  const metadataCount = allMetadata.length;

  // updateTitle in a single transaction; verify BOTH records changed
  const newTitle = "Renamed title";
  await sessions.updateTitle(id, newTitle);
  const dataAfter = await sessions.get(id);
  const metaAfter = await sessions.getMetadata(id);
  const titleUpdated = dataAfter?.title === newTitle && metaAfter?.title === newTitle;

  // delete (atomic dual-store); verify BOTH records are gone
  await sessions.delete(id);
  const dataGone = (await sessions.get(id)) === null;
  const metaGone = (await sessions.getMetadata(id)) === null;
  const deletedOk = dataGone && metaGone;

  return { saved, restoredOk, metadataCount, titleUpdated, deletedOk };
}
