// Tool-description prompt constants. Reproduced verbatim (content-wise) from the
// analyzed reference, brand-neutral. The artifacts tool description is a function
// so the dynamic HTML-artifact runtime-provider descriptions can be appended.
//
// These descriptions are the client-side source of truth for the artifacts tool.
// The server declares the tool (schema + description) for the system prompt as a
// SEPARATE server milestone; this module is what the client AgentTool reports
// for its own `description`.

export const ARTIFACTS_TOOL_DESCRIPTION = (runtimeProviderDescriptions: string[]) => `# Artifacts

Create and manage persistent files that live alongside the conversation.

## When to Use - Artifacts Tool vs REPL

**Use artifacts tool when YOU are the author:**
- Writing research summaries, analysis, ideas, documentation
- Creating markdown notes for user to read
- Building HTML applications/visualizations that present data
- Creating HTML artifacts that render charts from programmatically generated data

**Use repl + artifact storage functions when CODE processes data:**
- Scraping workflows that extract and store data
- Processing CSV/Excel files programmatically
- Data transformation pipelines
- Binary file generation requiring libraries (PDF, DOCX)

**Pattern: REPL generates data → Artifacts tool creates HTML that visualizes it**
Example: repl scrapes products → stores products.json → you author dashboard.html that reads products.json and renders Chart.js visualizations

## Input
- { action: "create", filename: "notes.md", content: "..." } - Create new file
- { action: "update", filename: "notes.md", old_str: "...", new_str: "..." } - Update part of file (PREFERRED)
- { action: "rewrite", filename: "notes.md", content: "..." } - Replace entire file (LAST RESORT)
- { action: "get", filename: "data.json" } - Retrieve file content
- { action: "delete", filename: "old.csv" } - Delete file
- { action: "htmlArtifactLogs", filename: "app.html" } - Get console logs from HTML artifact

## Returns
Depends on action:
- create/update/rewrite/delete: Success status or error
- get: File content
- htmlArtifactLogs: Console logs and errors

## Supported File Types
✅ Text-based files you author: .md, .txt, .html, .js, .css, .json, .csv, .svg
❌ Binary files requiring libraries (use repl): .pdf, .docx

## Critical - Prefer Update Over Rewrite
❌ NEVER: get entire file + rewrite to change small sections
✅ ALWAYS: update for targeted edits (token efficient)
✅ Ask: Can I describe the change as old_str → new_str? Use update.

---

## HTML Artifacts

Interactive HTML applications that can visualize data from other artifacts.

### Data Access
- Can read artifacts created by repl and user attachments
- Use to build dashboards, visualizations, interactive tools
- See Helper Functions section below for available functions

### Requirements
- Self-contained single file
- Import ES modules from esm.sh: <script type="module">import X from 'https://esm.sh/pkg';</script>
- Use Tailwind CDN: <script src="https://cdn.tailwindcss.com"></script>
- Can embed images from any domain: <img src="https://example.com/image.jpg">
- MUST set background color explicitly (avoid transparent)
- Inline CSS or Tailwind utility classes
- No localStorage/sessionStorage

### Styling
- Use Tailwind utility classes for clean, functional designs
- Ensure responsive layout (iframe may be resized)
- Avoid purple gradients, AI aesthetic clichés, and emojis

### Helper Functions (Automatically Available)

These functions are injected into HTML artifact sandbox:

${runtimeProviderDescriptions.join("\n\n")}
`;

export const ARTIFACTS_RUNTIME_PROVIDER_DESCRIPTION_RO = `
### Artifacts Storage

Read files from artifacts storage.

#### When to Use
- Read artifacts created by REPL or artifacts tool
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

export const ATTACHMENTS_RUNTIME_DESCRIPTION = `
### User Attachments

Read files the user uploaded to the conversation.

#### When to Use
- Process user-uploaded files (CSV, JSON, Excel, images, PDFs)

#### Functions
- listAttachments() - List all attachments, returns array of {id, fileName, mimeType, size}
- readTextAttachment(id) - Read attachment as text, returns string
- readBinaryAttachment(id) - Read attachment as binary data, returns Uint8Array

#### Example
CSV file:
\`\`\`javascript
const files = listAttachments();
const csvFile = files.find(f => f.fileName.endsWith('.csv'));
const csvData = readTextAttachment(csvFile.id);
const rows = csvData.split('\\n').map(row => row.split(','));
\`\`\`
`;
