// Artifacts runtime provider — exposes artifact CRUD globals to sandboxed code
// (`listArtifacts`, `getArtifact`, `createOrUpdateArtifact`, `deleteArtifact`).
//
// Works in two modes:
//   - online (live panel): runtime calls round-trip to the host via
//     `sendRuntimeMessage`, which this provider services against the artifacts
//     host (the live panel). Read/write controlled by `readWrite`.
//   - offline (downloaded standalone HTML): no host bridge is present, so the
//     runtime reads a snapshot injected via `getData()` (read-only).
//
// The real artifacts panel lands in M4. To avoid a forward dependency this file
// defines a minimal `ArtifactsHost` interface that M4 will implement; do not
// import the artifacts panel here.

import type { AgentMessage } from "../../core/messages";
import type { SandboxRuntimeProvider } from "./provider";

/**
 * Minimal artifacts panel contract this provider depends on. M4 implements it.
 */
export interface ArtifactsHost {
  /** Live artifact map keyed by filename. */
  artifacts: Map<string, { content: string }>;
  tool: {
    execute(
      toolCallId: string,
      args: { command: string; filename: string; content?: string },
    ): Promise<unknown>;
  };
}

/** Minimal agent contract: mirror artifact ops into the message history. */
export interface ArtifactsAgentHost {
  state: { messages: AgentMessage[] };
}

const ARTIFACTS_RUNTIME_PROVIDER_DESCRIPTION_RW = `
### Artifacts Storage

Create, read, update, and delete files in artifacts storage.

#### When to Use
- Store intermediate results between tool calls
- Save generated files (images, CSVs, processed data) for the user to view and download

#### Do NOT Use For
- Content you author directly, like summaries of content you read (use the artifacts tool instead)

#### Functions
- listArtifacts() - List all artifact filenames, returns Promise<string[]>
- getArtifact(filename) - Read artifact content, returns Promise<string | object>. JSON files auto-parse to objects, binary files return base64 string
- createOrUpdateArtifact(filename, content, mimeType?) - Create or update artifact, returns Promise<void>. JSON files auto-stringify objects, binary requires base64 string with mimeType
- deleteArtifact(filename) - Delete artifact, returns Promise<void>

#### Example
JSON workflow:
\`\`\`javascript
const response = await fetch('https://api.example.com/products');
const products = await response.json();
await createOrUpdateArtifact('products.json', products);

const all = await getArtifact('products.json');
const cheap = all.filter(p => p.price < 100);
await createOrUpdateArtifact('cheap.json', cheap);
\`\`\`

Binary file (image):
\`\`\`javascript
const canvas = document.createElement('canvas');
canvas.width = 800; canvas.height = 600;
const ctx = canvas.getContext('2d');
ctx.fillStyle = 'blue';
ctx.fillRect(0, 0, 800, 600);
const base64 = canvas.toDataURL().split(',')[1];
await createOrUpdateArtifact('chart.png', base64, 'image/png');
\`\`\`
`;

const ARTIFACTS_RUNTIME_PROVIDER_DESCRIPTION_RO = `
### Artifacts Storage

Read files from artifacts storage.

#### When to Use
- Read artifacts created by the REPL or artifacts tool
- Access data from other HTML artifacts
- Load configuration or data files

#### Do NOT Use For
- Creating new artifacts (not available in HTML artifacts)
- Modifying artifacts (read-only access)

#### Functions
- listArtifacts() - List all artifact filenames, returns Promise<string[]>
- getArtifact(filename) - Read artifact content, returns Promise<string | object>. JSON files auto-parse to objects, binary files return base64 string

#### Example
JSON data:
\`\`\`javascript
const products = await getArtifact('products.json');
const html = products.map(p => \`<div>\${p.name}: $\${p.price}</div>\`).join('');
document.body.innerHTML = html;
\`\`\`

Binary image:
\`\`\`javascript
const base64 = await getArtifact('chart.png');
const img = document.createElement('img');
img.src = 'data:image/png;base64,' + base64;
document.body.appendChild(img);
\`\`\`
`;

export class ArtifactsRuntimeProvider implements SandboxRuntimeProvider {
  constructor(
    private artifactsHost: ArtifactsHost,
    private agent?: ArtifactsAgentHost,
    private readWrite: boolean = true,
  ) {}

  getData(): Record<string, unknown> {
    // Snapshot for offline mode.
    const snapshot: Record<string, string> = {};
    this.artifactsHost.artifacts.forEach((artifact, filename) => {
      snapshot[filename] = artifact.content;
    });
    return { artifacts: snapshot };
  }

  getRuntime(): (sandboxId: string) => void {
    // Self-contained: stringified and injected. No outer references.
    return (_sandboxId: string) => {
      const w = window as unknown as Record<string, unknown>;
      const isJsonFile = (filename: string) => filename.endsWith(".json");
      const send = () => w.sendRuntimeMessage as ((m: unknown) => Promise<{ success: boolean; error?: string; result?: unknown }>) | undefined;

      w.listArtifacts = async (): Promise<string[]> => {
        const s = send();
        if (s) {
          const response = await s({ type: "artifact-operation", action: "list" });
          if (!response.success) throw new Error(response.error);
          return response.result as string[];
        }
        return Object.keys((w.artifacts as Record<string, string>) || {});
      };

      w.getArtifact = async (filename: string): Promise<unknown> => {
        let content: string;
        const s = send();
        if (s) {
          const response = await s({ type: "artifact-operation", action: "get", filename });
          if (!response.success) throw new Error(response.error);
          content = response.result as string;
        } else {
          const offline = (w.artifacts as Record<string, string>) || {};
          if (!offline[filename]) {
            throw new Error(`Artifact not found (offline mode): ${filename}`);
          }
          content = offline[filename];
        }

        if (isJsonFile(filename)) {
          try {
            return JSON.parse(content);
          } catch (e) {
            throw new Error(`Failed to parse JSON from ${filename}: ${e}`);
          }
        }
        return content;
      };

      w.createOrUpdateArtifact = async (
        filename: string,
        content: unknown,
        mimeType?: string,
      ): Promise<void> => {
        const s = send();
        if (!s) {
          throw new Error("Cannot create/update artifacts in offline mode (read-only)");
        }
        let finalContent: string;
        if (typeof content !== "string") {
          finalContent = JSON.stringify(content, null, 2);
        } else {
          finalContent = content;
        }
        const response = await s({
          type: "artifact-operation",
          action: "createOrUpdate",
          filename,
          content: finalContent,
          mimeType,
        });
        if (!response.success) throw new Error(response.error);
      };

      w.deleteArtifact = async (filename: string): Promise<void> => {
        const s = send();
        if (!s) {
          throw new Error("Cannot delete artifacts in offline mode (read-only)");
        }
        const response = await s({ type: "artifact-operation", action: "delete", filename });
        if (!response.success) throw new Error(response.error);
      };
    };
  }

  async handleMessage(
    message: unknown,
    respond: (response: Record<string, unknown>) => void,
  ): Promise<void> {
    const msg = message as {
      type?: string;
      action?: string;
      filename?: string;
      content?: string;
    };
    if (msg.type !== "artifact-operation") return;

    const { action, filename, content } = msg;

    try {
      switch (action) {
        case "list": {
          const filenames = Array.from(this.artifactsHost.artifacts.keys());
          respond({ success: true, result: filenames });
          break;
        }

        case "get": {
          const artifact = filename ? this.artifactsHost.artifacts.get(filename) : undefined;
          if (!artifact) {
            respond({ success: false, error: `Artifact not found: ${filename}` });
          } else {
            respond({ success: true, result: artifact.content });
          }
          break;
        }

        case "createOrUpdate": {
          if (!filename) {
            respond({ success: false, error: "filename is required" });
            break;
          }
          try {
            const exists = this.artifactsHost.artifacts.has(filename);
            const command = exists ? "rewrite" : "create";
            const op = exists ? "update" : "create";

            await this.artifactsHost.tool.execute("", { command, filename, content });
            this.agent?.state.messages.push({
              role: "artifact",
              action: op,
              filename,
              content,
              ...(op === "create" && { title: filename }),
              timestamp: new Date().toISOString(),
            });
            respond({ success: true });
          } catch (err) {
            respond({ success: false, error: (err as Error).message });
          }
          break;
        }

        case "delete": {
          if (!filename) {
            respond({ success: false, error: "filename is required" });
            break;
          }
          try {
            await this.artifactsHost.tool.execute("", { command: "delete", filename });
            this.agent?.state.messages.push({
              role: "artifact",
              action: "delete",
              filename,
              timestamp: new Date().toISOString(),
            });
            respond({ success: true });
          } catch (err) {
            respond({ success: false, error: (err as Error).message });
          }
          break;
        }

        default:
          respond({ success: false, error: `Unknown artifact action: ${action}` });
      }
    } catch (error) {
      respond({ success: false, error: (error as Error).message });
    }
  }

  getDescription(): string {
    return this.readWrite
      ? ARTIFACTS_RUNTIME_PROVIDER_DESCRIPTION_RW
      : ARTIFACTS_RUNTIME_PROVIDER_DESCRIPTION_RO;
  }
}
