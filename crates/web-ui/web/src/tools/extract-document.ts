// `extract_document` — a browser-executed tool. The server DECLARES it; the
// browser RUNS it: given `{ url }`, it fetches the document (50MB cap), and on
// success delegates to `loadAttachment(blob, url)` to produce page/sheet/slide-
// tagged extracted text. A CORS failure surfaces a neutral fallback message
// instructing the user to attach the file manually (no brand strings).

import { html } from "lit";
import { createRef, ref } from "lit/directives/ref.js";
import { FileText } from "lucide";
import { loadAttachment } from "../attachments/attachment-utils";
import type { BrowserToolResult } from "../client/remote-agent";
import type { ToolResultContent, ToolResultMessage } from "../core/messages";
import { EXTRACT_DOCUMENT_DESCRIPTION } from "../prompts/prompts";
import { isCorsError, resolveDocumentFetchUrl } from "../utils/cors";
import { i18n } from "../utils/i18n";
import {
  registerToolRenderer,
  renderCollapsibleHeader,
  renderHeader,
  type ToolRenderer,
  type ToolRenderResult,
} from "./renderer-registry";

export const EXTRACT_DOCUMENT_TOOL_NAME = "extract_document";

/** Maximum document size accepted by the extractor. */
const MAX_SIZE = 50 * 1024 * 1024;

/** Neutral message shown when a cross-origin fetch is blocked. */
const CORS_FALLBACK_MESSAGE =
  "TELL USER: Unable to fetch the document due to cross-origin (CORS) restrictions; " +
  "the server hosting the file blocks browser downloads.\n\n" +
  "INSTRUCT USER: Please download the file manually and attach it to your message " +
  "using the attachment button (paperclip icon) in the message input area. I can " +
  "then extract the text from the attached file.";

/** LLM-facing parameter shape. */
export interface ExtractDocumentParams {
  url?: string;
}

/** `details` payload attached to the tool result. */
export interface ExtractDocumentResult {
  extractedText: string;
  format: string;
  fileName: string;
  size: number;
}

const extractDocumentParamsSchema = {
  type: "object",
  properties: {
    url: {
      type: "string",
      description: "URL of the document to extract text from (PDF, DOCX, XLSX, or PPTX)",
    },
  },
  required: ["url"],
} as const;

/** The client extract-document tool: fixed description + execute. */
export interface ExtractDocumentTool {
  label: string;
  name: string;
  readonly description: string;
  parameters: unknown;
  execute(
    toolCallId: string,
    args: ExtractDocumentParams,
    signal?: AbortSignal,
  ): Promise<{ content: ToolResultContent[]; details: ExtractDocumentResult }>;
}

function formatFromMime(mimeType: string): string {
  if (mimeType.includes("pdf")) return "pdf";
  if (mimeType.includes("wordprocessingml")) return "docx";
  if (mimeType.includes("spreadsheetml") || mimeType.includes("ms-excel")) return "xlsx";
  if (mimeType.includes("presentationml")) return "pptx";
  if (mimeType.startsWith("text/")) return "text";
  return "unknown";
}

export function createExtractDocumentTool(): ExtractDocumentTool {
  return {
    label: "Extract Document",
    name: EXTRACT_DOCUMENT_TOOL_NAME,
    description: EXTRACT_DOCUMENT_DESCRIPTION,
    parameters: extractDocumentParamsSchema,
    async execute(_toolCallId: string, args: ExtractDocumentParams, signal?: AbortSignal) {
      if (signal?.aborted) {
        throw new Error("Extract document aborted");
      }

      const url = (args.url ?? "").trim();
      if (!url) {
        throw new Error("URL is required");
      }
      try {
        new URL(url);
      } catch {
        throw new Error(`Invalid URL: ${url}`);
      }

      // Route through the document-fetch proxy when enabled (CORS bypass for
      // remote hosts that block browser reads); falls back to the direct URL.
      const fetchUrl = await resolveDocumentFetchUrl(url);

      let blob: Blob;
      try {
        const response = await fetch(fetchUrl, { signal });
        if (!response.ok) {
          throw new Error(
            `TELL USER: Unable to download the document (${response.status} ${response.statusText}). ` +
              `The site likely blocks automated downloads.\n\n` +
              `INSTRUCT USER: Please download the file manually and attach it to your message using the ` +
              `attachment button (paperclip icon) in the message input area. I can then extract the text ` +
              `from the attached file.`,
          );
        }

        const contentLength = response.headers.get("content-length");
        if (contentLength) {
          const declared = Number.parseInt(contentLength, 10);
          if (Number.isFinite(declared) && declared > MAX_SIZE) {
            throw new Error(
              `Document is too large (${(declared / 1024 / 1024).toFixed(1)}MB). Maximum supported size is 50MB.`,
            );
          }
        }

        const arrayBuffer = await response.arrayBuffer();
        if (arrayBuffer.byteLength > MAX_SIZE) {
          throw new Error(
            `Document is too large (${(arrayBuffer.byteLength / 1024 / 1024).toFixed(1)}MB). Maximum supported size is 50MB.`,
          );
        }
        blob = new Blob([arrayBuffer], {
          type: response.headers.get("content-type") || "application/octet-stream",
        });
      } catch (fetchError) {
        if (isCorsError(fetchError)) {
          throw new Error(CORS_FALLBACK_MESSAGE);
        }
        throw fetchError;
      }

      // Derive a filename from the URL so format detection can use the extension.
      const urlParts = url.split("/");
      let fileName = urlParts[urlParts.length - 1]?.split("?")[0] || "document";
      if (url.startsWith("https://arxiv.org/")) {
        fileName = `${fileName}.pdf`;
      }

      const attachment = await loadAttachment(blob, fileName);
      if (!attachment.extractedText) {
        throw new Error(
          `Document format not supported. Supported formats:\n` +
            `- PDF (.pdf)\n- Word (.docx)\n- Excel (.xlsx, .xls)\n- PowerPoint (.pptx)`,
        );
      }

      return {
        content: [{ type: "text", text: attachment.extractedText }],
        details: {
          extractedText: attachment.extractedText,
          format: formatFromMime(attachment.mimeType),
          fileName: attachment.fileName,
          size: attachment.size,
        },
      };
    },
  };
}

export const extractDocumentTool = createExtractDocumentTool();

/** Adapt the tool to the BrowserToolExecutor contract; errors become error results. */
export function makeExtractDocumentExecutor(
  tool: ExtractDocumentTool,
): (toolCallId: string, args: unknown) => Promise<BrowserToolResult> {
  return async (toolCallId, args) => {
    try {
      const result = await tool.execute(toolCallId, (args ?? {}) as ExtractDocumentParams);
      return { content: result.content, isError: false, details: result.details };
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      return { content: [{ type: "text", text: message }], isError: true };
    }
  };
}

function resultText(result: ToolResultMessage): string {
  return (
    result.content
      ?.filter((c) => c.type === "text")
      .map((c) => c.text ?? "")
      .join("\n") || ""
  );
}

export const extractDocumentRenderer: ToolRenderer<ExtractDocumentParams, ExtractDocumentResult> = {
  render(params, result, isStreaming): ToolRenderResult {
    const state = result ? (result.isError ? "error" : "complete") : isStreaming ? "inprogress" : "complete";

    const contentRef = createRef<HTMLDivElement>();
    const chevronRef = createRef<HTMLSpanElement>();

    if (result && params) {
      const details = result.details;
      const title = details
        ? result.isError
          ? `Failed to extract ${details.fileName || "document"}`
          : `Extracted text from ${details.fileName} (${details.format.toUpperCase()}, ${(details.size / 1024).toFixed(1)}KB)`
        : result.isError
          ? i18n("Failed to extract document")
          : i18n("Extracted text from document");

      const output = resultText(result);

      return {
        content: html`
          <div>
            ${renderCollapsibleHeader(state, FileText, title, contentRef, chevronRef, false)}
            <div ${ref(contentRef)} class="max-h-0 overflow-hidden transition-all duration-300 space-y-3">
              ${params.url
                ? html`<div class="text-sm text-muted-foreground"><strong>URL:</strong> ${params.url}</div>`
                : ""}
              ${output && !result.isError
                ? html`<code-block .code=${output} language="plaintext"></code-block>`
                : ""}
              ${result.isError && output
                ? html`<console-block .content=${output} .variant=${"error"}></console-block>`
                : ""}
            </div>
          </div>
        `,
        isCustom: false,
      };
    }

    if (params) {
      return {
        content: html`
          <div>
            ${renderCollapsibleHeader(state, FileText, "Extracting document...", contentRef, chevronRef, false)}
            <div ${ref(contentRef)} class="max-h-0 overflow-hidden transition-all duration-300">
              <div class="text-sm text-muted-foreground"><strong>URL:</strong> ${params.url}</div>
            </div>
          </div>
        `,
        isCustom: false,
      };
    }

    return { content: renderHeader(state, FileText, "Preparing extraction..."), isCustom: false };
  },
};

registerToolRenderer(EXTRACT_DOCUMENT_TOOL_NAME, extractDocumentRenderer);
