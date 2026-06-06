// `ArtifactPill(filename, panel)` — an inline clickable badge that opens the
// named artifact in the panel. Rendered inside the artifacts tool renderer's
// status header so the user can jump straight to the file.

import { html, type TemplateResult } from "lit";
import { FileCode2 } from "lucide";
import { icon } from "../ui/icons";
import type { ArtifactsPanel } from "./artifacts-panel";

export function ArtifactPill(filename: string, artifactsPanel?: ArtifactsPanel): TemplateResult {
  const handleClick = (e: Event) => {
    if (!artifactsPanel) return;
    e.preventDefault();
    e.stopPropagation();
    artifactsPanel.openArtifact(filename);
  };

  return html`
    <span
      class="inline-flex items-center gap-1 px-2 py-0.5 text-xs bg-muted/50 border border-border rounded ${
        artifactsPanel ? "cursor-pointer hover:bg-muted transition-colors" : ""
      }"
      @click=${artifactsPanel ? handleClick : null}
    >
      ${icon(FileCode2, "sm")}
      <span class="text-foreground">${filename}</span>
    </span>
  `;
}
