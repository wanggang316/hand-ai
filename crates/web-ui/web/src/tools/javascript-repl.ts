// `javascript_repl` — a browser-executed tool. The server DECLARES it; the
// browser RUNS it: given `{ code }`, the code executes in a transient
// `<sandbox-iframe>` with the supplied runtime providers (attachments +
// file-download), and the result string is assembled from captured console
// output, the returned value, and any files returned via
// `returnDownloadableFile` (base64-encoded into `details.files`).
//
// `createJavaScriptReplTool()` exposes a dynamic `description` built from its
// `runtimeProvidersFactory()` so the model sees exactly which sandbox globals
// are available. `makeJavaScriptReplExecutor()` adapts the tool to the
// BrowserToolExecutor contract that RemoteAgent.registerBrowserTool expects.

import { html } from "lit";
import { createRef, ref } from "lit/directives/ref.js";
import { Code } from "lucide";
import type { BrowserToolResult } from "../client/remote-agent";
import type { Attachment, ToolResultContent, ToolResultMessage } from "../core/messages";
import { JAVASCRIPT_REPL_TOOL_DESCRIPTION } from "../prompts/prompts";
import {
  type SandboxFile,
  SandboxIframe,
  type SandboxResult,
  type SandboxRuntimeProvider,
  encodeFileContent,
} from "../sandbox";
// Side-effect import: pulling a runtime value from the sandbox registers
// <sandbox-iframe> (a type-only import would be elided under isolatedModules
// and the element would never register).
import "../sandbox/sandboxed-iframe";
// Side-effect import registers <attachment-tile> (a type-only import would be
// elided under isolatedModules and the element would never register).
import "../attachments/attachment-tile";
import { i18n } from "../utils/i18n";
import {
  registerToolRenderer,
  renderCollapsibleHeader,
  renderHeader,
  type ToolRenderer,
  type ToolRenderResult,
} from "./renderer-registry";

export const JAVASCRIPT_REPL_TOOL_NAME = "javascript_repl";

/** LLM-facing parameter shape. */
export interface JavaScriptReplParams {
  code?: string;
}

/** A returned file serialized for transport (base64 payload). */
export interface JavaScriptReplFile {
  fileName: string;
  mimeType: string;
  size: number;
  contentBase64: string;
}

/** `details` payload attached to the tool result. */
export interface JavaScriptReplResult {
  files?: JavaScriptReplFile[];
}

/**
 * Run JavaScript code in a transient hidden sandbox iframe and assemble a plain
 * text response from console output + return value + returned-file notices.
 */
export async function executeJavaScript(
  code: string,
  runtimeProviders: SandboxRuntimeProvider[],
  signal?: AbortSignal,
  sandboxUrlProvider?: () => string,
): Promise<{ output: string; files: SandboxFile[] }> {
  if (!code) {
    throw new Error("Code parameter is required");
  }
  if (signal?.aborted) {
    throw new Error("Execution aborted");
  }

  const sandbox = new SandboxIframe();
  if (sandboxUrlProvider) {
    sandbox.sandboxUrlProvider = sandboxUrlProvider;
  }
  sandbox.style.display = "none";
  document.body.appendChild(sandbox);

  try {
    const sandboxId = `repl-${Date.now()}-${Math.random().toString(36).substring(7)}`;
    const result: SandboxResult = await sandbox.execute(
      sandboxId,
      code,
      runtimeProviders,
      [],
      signal,
    );

    let output = "";

    for (const entry of result.consoleLogs) {
      output += `${entry.text}\n`;
    }

    if (result.error) {
      if (output) output += "\n";
      output += `Error: ${result.error.message || "Unknown error"}\n${result.error.stack || ""}`;
      throw new Error(output.trim());
    }

    if (result.returnValue !== undefined) {
      if (output) output += "\n";
      output += `=> ${
        typeof result.returnValue === "object"
          ? JSON.stringify(result.returnValue, null, 2)
          : String(result.returnValue)
      }`;
    }

    const files = result.files ?? [];
    if (files.length > 0) {
      output += `\n[Files returned: ${files.length}]\n`;
      for (const file of files) {
        output += `  - ${file.fileName} (${file.mimeType})\n`;
      }
    }

    return {
      output: output.trim() || "Code executed successfully (no output)",
      files,
    };
  } finally {
    sandbox.remove();
  }
}

/** Encode collected sandbox files to JSON-safe base64 payloads. */
function encodeFiles(files: SandboxFile[]): JavaScriptReplFile[] {
  return files.map((f) => {
    const { base64, size } = encodeFileContent(f.content);
    return {
      fileName: f.fileName || "file",
      mimeType: f.mimeType || "application/octet-stream",
      size,
      contentBase64: base64,
    };
  });
}

/** The client REPL tool: dynamic description + execute returning content+details. */
export interface JavaScriptReplTool {
  label: string;
  name: string;
  runtimeProvidersFactory: () => SandboxRuntimeProvider[];
  sandboxUrlProvider?: () => string;
  readonly description: string;
  parameters: unknown;
  execute(
    toolCallId: string,
    args: JavaScriptReplParams,
    signal?: AbortSignal,
  ): Promise<{ content: ToolResultContent[]; details: JavaScriptReplResult }>;
}

const javascriptReplParamsSchema = {
  type: "object",
  properties: {
    code: { type: "string", description: "JavaScript code to execute" },
  },
  required: ["code"],
} as const;

/**
 * Build the client REPL tool. `runtimeProvidersFactory` supplies the sandbox
 * runtime providers per run (e.g. attachments + file-download) and feeds the
 * dynamic description so the model sees the available globals.
 */
export function createJavaScriptReplTool(): JavaScriptReplTool {
  return {
    label: "JavaScript REPL",
    name: JAVASCRIPT_REPL_TOOL_NAME,
    runtimeProvidersFactory: () => [],
    sandboxUrlProvider: undefined,
    get description() {
      const descriptions = this.runtimeProvidersFactory()
        .map((p) => p.getDescription())
        .filter((d) => d.trim().length > 0);
      return JAVASCRIPT_REPL_TOOL_DESCRIPTION(descriptions);
    },
    parameters: javascriptReplParamsSchema,
    async execute(_toolCallId: string, args: JavaScriptReplParams, signal?: AbortSignal) {
      const result = await executeJavaScript(
        args.code ?? "",
        this.runtimeProvidersFactory(),
        signal,
        this.sandboxUrlProvider,
      );
      return {
        content: [{ type: "text", text: result.output }],
        details: { files: encodeFiles(result.files) },
      };
    },
  };
}

export const javascriptReplTool = createJavaScriptReplTool();

/**
 * Adapt a REPL tool to the BrowserToolExecutor contract. Errors thrown by the
 * sandbox become error tool results so the agent loop never hangs.
 */
export function makeJavaScriptReplExecutor(
  tool: JavaScriptReplTool,
): (toolCallId: string, args: unknown) => Promise<BrowserToolResult> {
  return async (toolCallId, args) => {
    try {
      const result = await tool.execute(toolCallId, (args ?? {}) as JavaScriptReplParams);
      return { content: result.content, isError: false, details: result.details };
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      return { content: [{ type: "text", text: message }], isError: true };
    }
  };
}

/** Decode a returned file's text content for inline display, if it is textual. */
function decodeTextFile(file: JavaScriptReplFile): string | undefined {
  const isTextBased =
    file.mimeType.startsWith("text/") ||
    file.mimeType === "application/json" ||
    file.mimeType === "application/javascript" ||
    file.mimeType.includes("xml");
  if (!isTextBased || !file.contentBase64) return undefined;
  try {
    return atob(file.contentBase64);
  } catch {
    return undefined;
  }
}

function filesToAttachments(files: JavaScriptReplFile[]): Attachment[] {
  return files.map((f, i) => ({
    id: `repl-${Date.now()}-${i}`,
    type: f.mimeType.startsWith("image/") ? "image" : "document",
    fileName: f.fileName || `file-${i}`,
    mimeType: f.mimeType || "application/octet-stream",
    size: f.size,
    content: f.contentBase64,
    preview: f.mimeType.startsWith("image/") ? f.contentBase64 : undefined,
    extractedText: decodeTextFile(f),
  }));
}

function resultText(result: ToolResultMessage): string {
  return (
    result.content
      ?.filter((c) => c.type === "text")
      .map((c) => c.text ?? "")
      .join("\n") || ""
  );
}

/** A returned downloadable file, shown as a read-only attachment tile. */
function fileChip(att: Attachment) {
  return html`<attachment-tile .attachment=${att}></attachment-tile>`;
}

export const javascriptReplRenderer: ToolRenderer<JavaScriptReplParams, JavaScriptReplResult> = {
  render(params, result, isStreaming): ToolRenderResult {
    const state = result ? (result.isError ? "error" : "complete") : isStreaming ? "inprogress" : "complete";

    const codeContentRef = createRef<HTMLDivElement>();
    const codeChevronRef = createRef<HTMLSpanElement>();

    if (result && params) {
      const output = resultText(result);
      const files = result.details?.files ?? [];
      const attachments = filesToAttachments(files);

      return {
        content: html`
          <div>
            ${renderCollapsibleHeader(state, Code, i18n("Executing JavaScript"), codeContentRef, codeChevronRef, false)}
            <div ${ref(codeContentRef)} class="max-h-0 overflow-hidden transition-all duration-300 space-y-3">
              <code-block .code=${params.code || ""} language="javascript"></code-block>
              ${output
                ? html`<console-block .content=${output} .variant=${result.isError ? "error" : "default"}></console-block>`
                : ""}
            </div>
            ${attachments.length
              ? html`<div class="flex flex-wrap gap-2 mt-3">${attachments.map(fileChip)}</div>`
              : ""}
          </div>
        `,
        isCustom: false,
      };
    }

    if (params) {
      return {
        content: html`
          <div>
            ${renderCollapsibleHeader(state, Code, i18n("Executing JavaScript"), codeContentRef, codeChevronRef, false)}
            <div ${ref(codeContentRef)} class="max-h-0 overflow-hidden transition-all duration-300">
              ${params.code ? html`<code-block .code=${params.code} language="javascript"></code-block>` : ""}
            </div>
          </div>
        `,
        isCustom: false,
      };
    }

    return { content: renderHeader(state, Code, i18n("Preparing JavaScript...")), isCustom: false };
  },
};

registerToolRenderer(JAVASCRIPT_REPL_TOOL_NAME, javascriptReplRenderer);
