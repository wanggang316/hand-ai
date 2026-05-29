// Store for chat sessions. Dual-store design: full session data lives in the
// `sessions` store while lightweight `sessions-metadata` records back the
// session list (so listing never deserializes full transcripts). Both stores
// key on `id` and index `lastModified` for descending recency ordering.
//
// save() and delete() wrap BOTH writes in a single transaction so the data and
// metadata stores never drift. updateTitle() likewise updates both records in a
// single transaction (a read-modify-write within one readwrite tx), fixing the
// reference implementation's two separate writes.

import type {
  SessionData,
  SessionMetadata,
  StorageTransaction,
  StoreConfig,
} from "./backend";
import { Store } from "./store";

const SESSIONS = "sessions";
const SESSIONS_METADATA = "sessions-metadata";

export class SessionsStore extends Store {
  getConfig(): StoreConfig {
    return {
      name: SESSIONS,
      keyPath: "id",
      indices: [{ name: "lastModified", keyPath: "lastModified" }],
    };
  }

  /**
   * Schema for the companion metadata store. The backend must be created with
   * BOTH this config and getConfig() so the dual-store transactions resolve.
   */
  static getMetadataConfig(): StoreConfig {
    return {
      name: SESSIONS_METADATA,
      keyPath: "id",
      indices: [{ name: "lastModified", keyPath: "lastModified" }],
    };
  }

  /** Atomically persist full data + metadata in a single transaction. */
  async save(data: SessionData, metadata: SessionMetadata): Promise<void> {
    await this.getBackend().transaction(
      [SESSIONS, SESSIONS_METADATA],
      "readwrite",
      async (tx) => {
        await tx.set(SESSIONS, data.id, data);
        await tx.set(SESSIONS_METADATA, metadata.id, metadata);
      },
    );
  }

  async get(id: string): Promise<SessionData | null> {
    return this.getBackend().get<SessionData>(SESSIONS, id);
  }

  async getMetadata(id: string): Promise<SessionMetadata | null> {
    return this.getBackend().get<SessionMetadata>(SESSIONS_METADATA, id);
  }

  /** All metadata, sorted by last-modified descending (most recent first). */
  async getAllMetadata(): Promise<SessionMetadata[]> {
    return this.getBackend().getAllFromIndex<SessionMetadata>(
      SESSIONS_METADATA,
      "lastModified",
      "desc",
    );
  }

  /** Atomically remove the session from BOTH stores in a single transaction. */
  async delete(id: string): Promise<void> {
    await this.getBackend().transaction(
      [SESSIONS, SESSIONS_METADATA],
      "readwrite",
      async (tx) => {
        await tx.delete(SESSIONS, id);
        await tx.delete(SESSIONS_METADATA, id);
      },
    );
  }

  /**
   * Update the session title in BOTH the metadata and data records inside a
   * single transaction so they cannot diverge. No-op for missing records.
   */
  async updateTitle(id: string, title: string): Promise<void> {
    await this.getBackend().transaction(
      [SESSIONS, SESSIONS_METADATA],
      "readwrite",
      async (tx: StorageTransaction) => {
        const metadata = await tx.get<SessionMetadata>(SESSIONS_METADATA, id);
        if (metadata) {
          metadata.title = title;
          await tx.set(SESSIONS_METADATA, id, metadata);
        }
        const data = await tx.get<SessionData>(SESSIONS, id);
        if (data) {
          data.title = title;
          await tx.set(SESSIONS, id, data);
        }
      },
    );
  }

  /** Id of the most recently modified session, or null if there are none. */
  async getLatestSessionId(): Promise<string | null> {
    const allMetadata = await this.getAllMetadata();
    return allMetadata.length > 0 ? allMetadata[0].id : null;
  }
}
