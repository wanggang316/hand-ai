// `Diff({ oldText, newText })` — a minimal line-level unified diff renderer used
// by the artifacts tool renderer to show `update` (old_str → new_str) edits.
// It computes a longest-common-subsequence over lines and renders added /
// removed / context lines with colored gutters. This is a brand-neutral
// reimplementation of the shared Diff helper the reference UI imported; it does
// not depend on any external diff library.

import { html, type TemplateResult } from "lit";

export interface DiffProps {
  oldText: string;
  newText: string;
}

type DiffOp = { kind: "context" | "add" | "remove"; text: string };

/** Compute a line-level diff via an LCS table. */
function computeLineDiff(oldText: string, newText: string): DiffOp[] {
  const a = oldText.split("\n");
  const b = newText.split("\n");
  const n = a.length;
  const m = b.length;

  // LCS length table.
  const lcs: number[][] = Array.from({ length: n + 1 }, () => new Array<number>(m + 1).fill(0));
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      lcs[i][j] = a[i] === b[j] ? lcs[i + 1][j + 1] + 1 : Math.max(lcs[i + 1][j], lcs[i][j + 1]);
    }
  }

  const ops: DiffOp[] = [];
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (a[i] === b[j]) {
      ops.push({ kind: "context", text: a[i] });
      i++;
      j++;
    } else if (lcs[i + 1][j] >= lcs[i][j + 1]) {
      ops.push({ kind: "remove", text: a[i] });
      i++;
    } else {
      ops.push({ kind: "add", text: b[j] });
      j++;
    }
  }
  while (i < n) {
    ops.push({ kind: "remove", text: a[i] });
    i++;
  }
  while (j < m) {
    ops.push({ kind: "add", text: b[j] });
    j++;
  }
  return ops;
}

export function Diff(props: DiffProps): TemplateResult {
  const ops = computeLineDiff(props.oldText ?? "", props.newText ?? "");

  return html`
    <div class="border border-border rounded-lg overflow-hidden bg-background">
      <div class="overflow-auto max-h-96">
        <pre class="m-0 text-xs font-mono leading-relaxed"><code>${ops.map((op) => {
          const cls =
            op.kind === "add"
              ? "block px-3 bg-green-500/10 text-green-700 dark:text-green-400"
              : op.kind === "remove"
                ? "block px-3 bg-red-500/10 text-red-700 dark:text-red-400"
                : "block px-3 text-muted-foreground";
          const prefix = op.kind === "add" ? "+ " : op.kind === "remove" ? "- " : "  ";
          return html`<span class=${cls}>${prefix}${op.text}\n</span>`;
        })}</code></pre>
      </div>
    </div>
  `;
}
