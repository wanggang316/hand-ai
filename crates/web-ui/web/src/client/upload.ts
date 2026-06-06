// Out-of-band attachment upload helper. Posts attachment bytes to the Rust
// server's `POST /upload` endpoint, which stores them under a content id and
// returns `{ id, size }`. The id is then referenced from the subsequent
// `prompt` frame instead of inlining large base64, keeping WS frames small.
//
// The server accepts both multipart and raw bodies; we use multipart so the
// filename and content-type travel as form metadata (matching `curl -F`).

import type { Attachment } from "../core/messages";

/** Result of a successful upload: the content id and stored byte length. */
export interface UploadResult {
  id: string;
  size: number;
}

/** Decode a base64 string into a `Uint8Array` (chunked to avoid stack limits). */
function base64ToBytes(base64: string): Uint8Array {
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

/**
 * Upload an attachment's raw bytes to the server. `attachment.content` is the
 * base64-encoded payload produced by `loadAttachment`; it is decoded back to
 * bytes and posted as a multipart `file` field.
 *
 * @throws if the server rejects the upload (size cap, transport error).
 */
export async function uploadAttachment(attachment: Attachment): Promise<UploadResult> {
  const bytes = base64ToBytes(attachment.content);
  const blob = new Blob([bytes.buffer as ArrayBuffer], {
    type: attachment.mimeType || "application/octet-stream",
  });

  const form = new FormData();
  form.append("file", blob, attachment.fileName || "attachment");

  const response = await fetch("/upload", { method: "POST", body: form });
  if (!response.ok) {
    const detail = await response.text().catch(() => "");
    throw new Error(`Upload failed (${response.status}): ${detail || response.statusText}`);
  }
  return (await response.json()) as UploadResult;
}
