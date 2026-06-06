// Minimal, dependency-free, XSS-safe Markdown -> HTML renderer.
//
// Strategy: every piece of source text is HTML-escaped BEFORE any transform, so
// no markup from the source can survive into the output — the renderer only ever
// EMITS a bounded set of known-safe tags. Block structure is detected on the raw
// lines (so `#`, `>`, `|`, `-`, etc. are seen literally), and only the text
// CONTENT of each block is escaped + inline-formatted.
//
// Supported: ATX headings (`#`..`######`), fenced (``` / ~~~) and inline code,
// `**bold**`/`__bold__`, `*italic*`/`_italic_`, `~~strikethrough~~`, links
// (http/https/mailto/relative only — `javascript:`/`data:` are dropped),
// unordered/ordered lists, blockquotes (nested), horizontal rules, GitHub-style
// tables, and paragraphs. Nested lists are flattened to a single level (an
// acceptable simplification for chat/artifact content).

const HEADING_CLASSES: Record<number, string> = {
  1: "text-2xl font-bold mt-4 mb-2",
  2: "text-xl font-bold mt-4 mb-2",
  3: "text-lg font-semibold mt-3 mb-1.5",
  4: "text-base font-semibold mt-3 mb-1.5",
  5: "text-sm font-semibold mt-2 mb-1",
  6: "text-sm font-semibold text-muted-foreground mt-2 mb-1",
};

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

/**
 * Validate an (already HTML-escaped) URL: allow only http(s), mailto, and
 * relative/anchor targets. Everything else (notably `javascript:` and `data:`)
 * yields an empty string so the caller renders plain text instead of a link.
 */
function sanitizeUrl(url: string): string {
  const trimmed = url.trim();
  if (/^(https?:|mailto:)/i.test(trimmed)) return trimmed;
  // Relative path, root-relative, or in-page anchor — no scheme.
  if (/^[#/.]/.test(trimmed) || /^[a-z0-9_-]+(\/|$)/i.test(trimmed)) {
    if (!/^[a-z][a-z0-9+.-]*:/i.test(trimmed)) return trimmed;
  }
  return "";
}

/** Apply emphasis/link inline transforms to an already-escaped, code-free run. */
function applyEmphasis(text: string): string {
  // Links [label](url). URL is already escaped; validate the scheme.
  let out = text.replace(/\[([^\]]+)\]\(([^)\s]+)\)/g, (_m, label: string, url: string) => {
    const safe = sanitizeUrl(url);
    if (!safe) return label;
    return `<a href="${safe}" target="_blank" rel="noopener noreferrer" class="text-primary underline underline-offset-2 hover:opacity-80">${label}</a>`;
  });
  // Bold, then italic, then strikethrough.
  out = out
    .replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>")
    .replace(/__([^_]+)__/g, "<strong>$1</strong>");
  out = out
    .replace(/(^|[^*])\*([^*\n]+)\*/g, "$1<em>$2</em>")
    .replace(/(^|[^_\w])_([^_\n]+)_/g, "$1<em>$2</em>");
  out = out.replace(/~~([^~]+)~~/g, "<del>$1</del>");
  return out;
}

/**
 * Apply inline formatting to an already-HTML-escaped fragment. Splitting on the
 * inline-code pattern isolates code spans (odd indices) from prose (even
 * indices) with no placeholder sentinel — emphasis/link passes never touch a
 * code span, and there is no sentinel that could collide with real text.
 */
function renderInline(escaped: string): string {
  const parts = escaped.split(/`([^`]+)`/);
  let out = "";
  for (let k = 0; k < parts.length; k++) {
    if (k % 2 === 1) {
      out += `<code class="px-1 py-0.5 rounded bg-muted font-mono text-[0.9em]">${parts[k]}</code>`;
    } else {
      out += applyEmphasis(parts[k]);
    }
  }
  return out;
}

/** Escape + inline-format a raw fragment. */
function inline(raw: string): string {
  return renderInline(escapeHtml(raw));
}

function parseRow(line: string): string[] {
  return line
    .replace(/^\s*\|/, "")
    .replace(/\|\s*$/, "")
    .split("|")
    .map((c) => c.trim());
}

/** Render a Markdown string to a safe HTML string. */
export function renderMarkdown(src: string): string {
  if (!src) return "";
  const lines = src.replace(/\r\n?/g, "\n").split("\n");
  const out: string[] = [];
  let para: string[] = [];
  let i = 0;

  const flushPara = (): void => {
    if (para.length) {
      out.push(`<p class="my-2 leading-relaxed">${inline(para.join(" "))}</p>`);
      para = [];
    }
  };

  while (i < lines.length) {
    const line = lines[i];

    // Fenced code block.
    const fence = line.match(/^(```|~~~)(.*)$/);
    if (fence) {
      flushPara();
      const marker = fence[1];
      const lang = fence[2].trim().replace(/[^a-zA-Z0-9_+#-]/g, "");
      const code: string[] = [];
      i++;
      while (i < lines.length && !lines[i].startsWith(marker)) {
        code.push(lines[i]);
        i++;
      }
      if (i < lines.length) i++; // consume closing fence
      const attr = lang ? ` data-lang="${lang}"` : "";
      out.push(
        `<pre class="my-2 p-3 rounded bg-muted overflow-x-auto"><code class="font-mono text-sm"${attr}>${escapeHtml(
          code.join("\n"),
        )}</code></pre>`,
      );
      continue;
    }

    // Blank line.
    if (/^\s*$/.test(line)) {
      flushPara();
      i++;
      continue;
    }

    // ATX heading.
    const h = line.match(/^(#{1,6})\s+(.*)$/);
    if (h) {
      flushPara();
      const level = h[1].length;
      out.push(
        `<h${level} class="${HEADING_CLASSES[level]}">${inline(h[2].replace(/\s+#+\s*$/, "").trim())}</h${level}>`,
      );
      i++;
      continue;
    }

    // Horizontal rule.
    if (/^\s*([-*_])(\s*\1){2,}\s*$/.test(line)) {
      flushPara();
      out.push(`<hr class="my-3 border-border" />`);
      i++;
      continue;
    }

    // Blockquote (one or more consecutive `>` lines; rendered recursively).
    if (/^\s*>\s?/.test(line)) {
      flushPara();
      const quote: string[] = [];
      while (i < lines.length && /^\s*>\s?/.test(lines[i])) {
        quote.push(lines[i].replace(/^\s*>\s?/, ""));
        i++;
      }
      out.push(
        `<blockquote class="my-2 pl-3 border-l-2 border-border text-muted-foreground">${renderMarkdown(
          quote.join("\n"),
        )}</blockquote>`,
      );
      continue;
    }

    // GitHub-style table: a `|` row followed by a `---` separator row.
    if (
      line.includes("|") &&
      i + 1 < lines.length &&
      lines[i + 1].includes("-") &&
      /^\s*\|?[\s:|-]+\|?\s*$/.test(lines[i + 1])
    ) {
      flushPara();
      const header = parseRow(line);
      const align = parseRow(lines[i + 1]).map((s) => {
        const l = s.startsWith(":");
        const r = s.endsWith(":");
        return l && r ? "center" : r ? "right" : l ? "left" : "";
      });
      i += 2;
      const body: string[][] = [];
      while (i < lines.length && lines[i].includes("|") && !/^\s*$/.test(lines[i])) {
        body.push(parseRow(lines[i]));
        i++;
      }
      const cell = (content: string, tag: "th" | "td", idx: number): string => {
        const style = align[idx] ? ` style="text-align:${align[idx]}"` : "";
        return `<${tag} class="border border-border px-2 py-1"${style}>${inline(content)}</${tag}>`;
      };
      let table = `<table class="my-2 border-collapse text-sm"><thead><tr>`;
      header.forEach((c, idx) => (table += cell(c, "th", idx)));
      table += `</tr></thead><tbody>`;
      for (const row of body) {
        table += `<tr>`;
        header.forEach((_, idx) => (table += cell(row[idx] ?? "", "td", idx)));
        table += `</tr>`;
      }
      table += `</tbody></table>`;
      out.push(table);
      continue;
    }

    // Lists (unordered or ordered). Consecutive items of the same kind group
    // into one list; nested items are flattened to a single level.
    const isUl = /^\s*[-*+]\s+/.test(line);
    const isOl = /^\s*\d+[.)]\s+/.test(line);
    if (isUl || isOl) {
      flushPara();
      const ordered = isOl;
      const tag = ordered ? "ol" : "ul";
      const cls = ordered ? "list-decimal" : "list-disc";
      out.push(`<${tag} class="my-2 pl-6 ${cls} space-y-1">`);
      while (i < lines.length) {
        const m = ordered
          ? lines[i].match(/^\s*\d+[.)]\s+(.*)$/)
          : lines[i].match(/^\s*[-*+]\s+(.*)$/);
        if (!m) break;
        out.push(`<li>${inline(m[1])}</li>`);
        i++;
      }
      out.push(`</${tag}>`);
      continue;
    }

    // Default: paragraph text (soft-wrapped lines join with a space).
    para.push(line.trim());
    i++;
  }

  flushPara();
  return out.join("\n");
}
