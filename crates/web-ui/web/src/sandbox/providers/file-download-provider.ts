// File download runtime provider — exposes `returnDownloadableFile(fileName,
// content, mimeType?)` to sandboxed code.
//
// Online (live host present): the runtime forwards the file to the host via
// `sendRuntimeMessage`, and this provider collects it for the caller.
// Offline (downloaded standalone HTML): the runtime triggers a browser download
// directly via an object URL.
//
// Returned-file payloads (which may exceed 1MB) are encoded to base64 with a
// fixed 0x8000 chunk size so `String.fromCharCode(...bytes)` never overflows the
// call stack on large buffers.

import type { SandboxRuntimeProvider } from "./provider";

/** Chunk size for base64 encoding large buffers without stack overflow. */
export const BASE64_CHUNK_SIZE = 0x8000;

export interface DownloadableFile {
  fileName: string;
  content: string | Uint8Array;
  mimeType: string;
}

/** A returned file serialized to a JSON-safe base64 payload. */
export interface EncodedDownloadableFile {
  fileName: string;
  mimeType: string;
  size: number;
  contentBase64: string;
}

export class FileDownloadRuntimeProvider implements SandboxRuntimeProvider {
  private files: DownloadableFile[] = [];

  getData(): Record<string, unknown> {
    return {};
  }

  getRuntime(): (sandboxId: string) => void {
    // Self-contained: stringified and injected. No outer references.
    return (_sandboxId: string) => {
      const w = window as unknown as Record<string, unknown>;

      w.returnDownloadableFile = async (
        fileName: string,
        content: unknown,
        mimeType?: string,
      ) => {
        let finalContent: string | Uint8Array;
        let finalMimeType: string;

        if (content instanceof Blob) {
          const arrayBuffer = await content.arrayBuffer();
          finalContent = new Uint8Array(arrayBuffer);
          if (!mimeType && !content.type) {
            throw new Error(
              "returnDownloadableFile: MIME type is required for Blob content. Please provide a mimeType parameter (e.g., 'image/png').",
            );
          }
          finalMimeType = mimeType || content.type || "application/octet-stream";
        } else if (content instanceof Uint8Array) {
          finalContent = content;
          if (!mimeType) {
            throw new Error(
              "returnDownloadableFile: MIME type is required for Uint8Array content. Please provide a mimeType parameter (e.g., 'image/png').",
            );
          }
          finalMimeType = mimeType;
        } else if (typeof content === "string") {
          finalContent = content;
          finalMimeType = mimeType || "text/plain";
        } else {
          finalContent = JSON.stringify(content, null, 2);
          finalMimeType = mimeType || "application/json";
        }

        const send = w.sendRuntimeMessage as
          | ((m: unknown) => Promise<{ error?: string }>)
          | undefined;
        if (send) {
          // Online mode: hand the file to the host (structured clone preserves
          // the Uint8Array; the host base64-encodes it for retrieval).
          const response = await send({
            type: "file-returned",
            fileName,
            content: finalContent,
            mimeType: finalMimeType,
          });
          if (response.error) throw new Error(response.error);
        } else {
          // Offline mode: download directly.
          const blob = new Blob([finalContent as BlobPart], { type: finalMimeType });
          const url = URL.createObjectURL(blob);
          const a = document.createElement("a");
          a.href = url;
          a.download = fileName;
          a.click();
          URL.revokeObjectURL(url);
        }
      };
    };
  }

  async handleMessage(
    message: unknown,
    respond: (response: Record<string, unknown>) => void,
  ): Promise<void> {
    const msg = message as {
      type?: string;
      fileName?: string;
      content?: string | Uint8Array;
      mimeType?: string;
    };
    if (msg.type === "file-returned") {
      this.files.push({
        fileName: msg.fileName ?? "file",
        content: msg.content ?? "",
        mimeType: msg.mimeType ?? "application/octet-stream",
      });
      respond({ success: true });
    }
  }

  /** Collected returned files (raw content). */
  getFiles(): DownloadableFile[] {
    return this.files;
  }

  /** Reset state for reuse. */
  reset(): void {
    this.files = [];
  }

  getDescription(): string {
    return "returnDownloadableFile(filename, content, mimeType?) - Create downloadable file for user (one-time download, not accessible later)";
  }
}

/**
 * Encode arbitrary file content to a JSON-safe base64 payload using a fixed
 * 0x8000 chunk size, so files larger than 1MB round-trip without exhausting the
 * call stack via `String.fromCharCode(...bytes)`.
 */
export function encodeFileContent(content: string | Uint8Array): {
  base64: string;
  size: number;
} {
  let bytes: Uint8Array;
  if (content instanceof Uint8Array) {
    bytes = content;
  } else {
    bytes = new TextEncoder().encode(content);
  }

  let binary = "";
  for (let i = 0; i < bytes.length; i += BASE64_CHUNK_SIZE) {
    binary += String.fromCharCode(...bytes.subarray(i, i + BASE64_CHUNK_SIZE));
  }
  return { base64: btoa(binary), size: bytes.length };
}

/** Encode a collected downloadable file to its JSON-safe form. */
export function encodeDownloadableFile(file: DownloadableFile): EncodedDownloadableFile {
  const { base64, size } = encodeFileContent(file.content);
  return {
    fileName: file.fileName || "file",
    mimeType: file.mimeType || "application/octet-stream",
    size,
    contentBase64: base64,
  };
}
