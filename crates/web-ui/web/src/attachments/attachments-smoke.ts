// Tiny browser-console smoke helper for the attachments subsystem. The controller
// can call `await runAttachmentSmoke()` from the browser console to verify that a
// small text File runs through `loadAttachment` and renders as an
// `<attachment-tile>`.
//
// It side-effect-imports the tile (which registers the element and pulls in the
// overlay), ingests an in-memory `.txt` File, mounts a tile, and reports the
// resulting attachment shape plus the tile's tag name and presence.
//
// rAF-free by design: the controller may test in a backgrounded tab where
// requestAnimationFrame callbacks are throttled or never fire, so this waits on
// `updateComplete` and a macrotask only.

import { loadAttachment } from "./attachment-utils";
import { AttachmentTile } from "./attachment-tile";

export interface AttachmentSmokeResult {
  fileName: string;
  type: string;
  tileTagName: string;
  hasTile: boolean;
}

export async function runAttachmentSmoke(): Promise<AttachmentSmokeResult> {
  const file = new File(["hello-attachment-smoke\nsecond line"], "smoke.txt", {
    type: "text/plain",
  });
  const attachment = await loadAttachment(file);

  const tile = document.createElement("attachment-tile") as AttachmentTile;
  tile.attachment = attachment;

  // Mount a small on-screen tile (off-screen elements can have their update
  // deferred in a backgrounded tab).
  tile.style.position = "fixed";
  tile.style.left = "0";
  tile.style.bottom = "0";
  tile.style.zIndex = "2147483647";
  document.body.appendChild(tile);

  // Wait for the element to render. Use `updateComplete` plus a macrotask rather
  // than requestAnimationFrame so this stays reliable in a backgrounded tab.
  await tile.updateComplete?.catch?.(() => {});
  await new Promise<void>((resolve) => setTimeout(resolve, 0));

  const rendered = tile.querySelector("div.relative.group") !== null;

  return {
    fileName: attachment.fileName,
    type: attachment.type,
    tileTagName: tile.tagName.toLowerCase(),
    hasTile: rendered,
  };
}

// Expose on window for easy invocation from the browser console.
(window as unknown as { runAttachmentSmoke?: typeof runAttachmentSmoke }).runAttachmentSmoke =
  runAttachmentSmoke;
