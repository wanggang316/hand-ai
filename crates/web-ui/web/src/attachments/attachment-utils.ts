// Attachment ingestion core. `loadAttachment` turns a File / Blob / ArrayBuffer
// / URL into a fully-populated `Attachment` (the type lives in core/messages):
// base64 content plus, per format, page/sheet/slide-tagged extracted text and
// (for PDFs and images) a preview image.
//
// Per-format processors:
// - PDF:   page-tagged text + a 160x160 first-page thumbnail (pdfjs-dist).
// - DOCX:  walk the docx-preview AST, emitting paragraph / table text.
// - PPTX:  unzip with jszip, scrape `<a:t>` runs from slides + notes.
// - Excel: xlsx workbook -> one CSV block per sheet.
// - image: base64 content doubles as the preview.
// - text:  TextDecoder, gated by a MIME / extension allowlist.
//
// Binary -> base64 uses a fixed 0x8000 chunk so very large buffers do not blow
// the call stack via `String.fromCharCode(...)`. The pdfjs worker is configured
// exactly like the M4 pdf-artifact (Vite `?url` static asset).

import { parseAsync } from "docx-preview";
import JSZip from "jszip";
import type { PDFDocumentProxy } from "pdfjs-dist";
import * as pdfjsLib from "pdfjs-dist";
// Vite resolves `?url` to the emitted worker asset URL (same config as M4's
// pdf-artifact); a side-effect runtime import keeps the worker bundled.
import pdfWorkerUrl from "pdfjs-dist/build/pdf.worker.min.mjs?url";
import * as XLSX from "xlsx";
import type { Attachment } from "../core/messages";
import { i18n } from "../utils/i18n";

pdfjsLib.GlobalWorkerOptions.workerSrc = pdfWorkerUrl;

/** Chunk size for base64 encoding large buffers without stack overflow. */
const BASE64_CHUNK_SIZE = 0x8000;

const DOCX_MIME = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
const PPTX_MIME = "application/vnd.openxmlformats-officedocument.presentationml.presentation";
const XLSX_MIME = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
const XLS_MIME = "application/vnd.ms-excel";

/** Extensions treated as plain text when no recognized MIME type is present. */
const TEXT_EXTENSIONS = [
  ".txt",
  ".md",
  ".json",
  ".xml",
  ".html",
  ".css",
  ".js",
  ".ts",
  ".jsx",
  ".tsx",
  ".yml",
  ".yaml",
] as const;

/** Encode raw bytes to base64 in fixed chunks (avoids call-stack overflow). */
function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  for (let i = 0; i < bytes.length; i += BASE64_CHUNK_SIZE) {
    binary += String.fromCharCode(...bytes.subarray(i, i + BASE64_CHUNK_SIZE));
  }
  return btoa(binary);
}

function endsWithAny(name: string, exts: readonly string[]): boolean {
  const lower = name.toLowerCase();
  return exts.some((ext) => lower.endsWith(ext));
}

/**
 * Load an attachment from a URL string, `File`, `Blob`, or `ArrayBuffer`.
 *
 * @param source URL string, File, Blob, or ArrayBuffer.
 * @param fileName Optional filename override (used for type detection).
 * @throws if the source cannot be read or the type is unsupported.
 */
export async function loadAttachment(
  source: string | File | Blob | ArrayBuffer,
  fileName?: string,
): Promise<Attachment> {
  let arrayBuffer: ArrayBuffer;
  let detectedFileName = fileName || "unnamed";
  let mimeType = "application/octet-stream";
  let size = 0;

  if (typeof source === "string") {
    const response = await fetch(source);
    if (!response.ok) {
      throw new Error(i18n("Failed to fetch file"));
    }
    arrayBuffer = await response.arrayBuffer();
    size = arrayBuffer.byteLength;
    mimeType = response.headers.get("content-type") || mimeType;
    if (!fileName) {
      const urlParts = source.split("/");
      detectedFileName = urlParts[urlParts.length - 1] || "document";
    }
  } else if (source instanceof File) {
    arrayBuffer = await source.arrayBuffer();
    size = source.size;
    mimeType = source.type || mimeType;
    detectedFileName = fileName || source.name;
  } else if (source instanceof Blob) {
    arrayBuffer = await source.arrayBuffer();
    size = source.size;
    mimeType = source.type || mimeType;
  } else if (source instanceof ArrayBuffer) {
    arrayBuffer = source;
    size = source.byteLength;
  } else {
    throw new Error(i18n("Invalid source type"));
  }

  const base64Content = bytesToBase64(new Uint8Array(arrayBuffer));
  const id = `${detectedFileName}_${Date.now()}_${Math.random()}`;

  // PDF.
  if (mimeType === "application/pdf" || detectedFileName.toLowerCase().endsWith(".pdf")) {
    const { extractedText, preview } = await processPdf(arrayBuffer, detectedFileName);
    return {
      id,
      type: "document",
      fileName: detectedFileName,
      mimeType: "application/pdf",
      size,
      content: base64Content,
      extractedText,
      preview,
    };
  }

  // DOCX.
  if (mimeType === DOCX_MIME || detectedFileName.toLowerCase().endsWith(".docx")) {
    const { extractedText } = await processDocx(arrayBuffer, detectedFileName);
    return {
      id,
      type: "document",
      fileName: detectedFileName,
      mimeType: DOCX_MIME,
      size,
      content: base64Content,
      extractedText,
    };
  }

  // PPTX.
  if (mimeType === PPTX_MIME || detectedFileName.toLowerCase().endsWith(".pptx")) {
    const { extractedText } = await processPptx(arrayBuffer, detectedFileName);
    return {
      id,
      type: "document",
      fileName: detectedFileName,
      mimeType: PPTX_MIME,
      size,
      content: base64Content,
      extractedText,
    };
  }

  // Excel (XLSX / XLS).
  if (
    mimeType === XLSX_MIME ||
    mimeType === XLS_MIME ||
    endsWithAny(detectedFileName, [".xlsx", ".xls"])
  ) {
    const { extractedText } = await processExcel(arrayBuffer, detectedFileName);
    return {
      id,
      type: "document",
      fileName: detectedFileName,
      mimeType: mimeType.startsWith("application/vnd") ? mimeType : XLSX_MIME,
      size,
      content: base64Content,
      extractedText,
    };
  }

  // Image.
  if (mimeType.startsWith("image/")) {
    return {
      id,
      type: "image",
      fileName: detectedFileName,
      mimeType,
      size,
      content: base64Content,
      preview: base64Content,
    };
  }

  // Plain text (MIME or extension allowlist).
  const isTextFile =
    mimeType.startsWith("text/") || endsWithAny(detectedFileName, TEXT_EXTENSIONS);
  if (isTextFile) {
    const text = new TextDecoder().decode(arrayBuffer);
    return {
      id,
      type: "document",
      fileName: detectedFileName,
      mimeType: mimeType.startsWith("text/") ? mimeType : "text/plain",
      size,
      content: base64Content,
      extractedText: text,
    };
  }

  throw new Error(`Unsupported file type: ${mimeType}`);
}

async function processPdf(
  arrayBuffer: ArrayBuffer,
  fileName: string,
): Promise<{ extractedText: string; preview?: string }> {
  let pdf: PDFDocumentProxy | null = null;
  try {
    pdf = await pdfjsLib.getDocument({ data: arrayBuffer }).promise;

    let extractedText = `<pdf filename="${fileName}">`;
    for (let i = 1; i <= pdf.numPages; i++) {
      const page = await pdf.getPage(i);
      const textContent = await page.getTextContent();
      const pageText = textContent.items
        .map((item) => ("str" in item ? item.str : ""))
        .filter((str) => str.trim())
        .join(" ");
      extractedText += `\n<page number="${i}">\n${pageText}\n</page>`;
    }
    extractedText += "\n</pdf>";

    const preview = await generatePdfPreview(pdf);
    return { extractedText, preview };
  } catch (error) {
    console.error("Error processing PDF:", error);
    throw new Error(`Failed to process PDF: ${String(error)}`);
  } finally {
    if (pdf) pdf.destroy();
  }
}

async function generatePdfPreview(pdf: PDFDocumentProxy): Promise<string | undefined> {
  try {
    const page = await pdf.getPage(1);
    const viewport = page.getViewport({ scale: 1.0 });

    // Fit a 160x160 thumbnail box.
    const scale = Math.min(160 / viewport.width, 160 / viewport.height);
    const scaledViewport = page.getViewport({ scale });

    const canvas = document.createElement("canvas");
    const context = canvas.getContext("2d");
    if (!context) return undefined;

    canvas.height = scaledViewport.height;
    canvas.width = scaledViewport.width;

    await page.render({ canvasContext: context, viewport: scaledViewport, canvas }).promise;

    // base64 without the data URL prefix.
    return canvas.toDataURL("image/png").split(",")[1];
  } catch (error) {
    console.error("Error generating PDF preview:", error);
    return undefined;
  }
}

// docx-preview AST nodes are loosely typed; this walker is intentionally
// structural over `type` / `children` / `text`.
interface DocxNode {
  type?: string;
  text?: string;
  children?: DocxNode[];
}

async function processDocx(
  arrayBuffer: ArrayBuffer,
  fileName: string,
): Promise<{ extractedText: string }> {
  try {
    const wordDoc = await parseAsync(arrayBuffer);

    let extractedText = `<docx filename="${fileName}">\n<page number="1">\n`;

    const body = (wordDoc as { documentPart?: { body?: DocxNode } }).documentPart?.body;
    if (body?.children) {
      const texts: string[] = [];
      for (const element of body.children) {
        const text = extractTextFromElement(element);
        if (text) texts.push(text);
      }
      extractedText += texts.join("\n");
    }

    extractedText += `\n</page>\n</docx>`;
    return { extractedText };
  } catch (error) {
    console.error("Error processing DOCX:", error);
    throw new Error(`Failed to process DOCX: ${String(error)}`);
  }
}

function extractTextFromElement(element: DocxNode): string {
  let text = "";
  const elementType = element.type?.toLowerCase() || "";

  if (elementType === "paragraph" && element.children) {
    for (const child of element.children) {
      const childType = child.type?.toLowerCase() || "";
      if (childType === "run" && child.children) {
        for (const textChild of child.children) {
          if ((textChild.type?.toLowerCase() || "") === "text") {
            text += textChild.text || "";
          }
        }
      } else if (childType === "text") {
        text += child.text || "";
      }
    }
  } else if (elementType === "table" && element.children) {
    const tableTexts: string[] = [];
    for (const row of element.children) {
      if ((row.type?.toLowerCase() || "") === "tablerow" && row.children) {
        const rowTexts: string[] = [];
        for (const cell of row.children) {
          if ((cell.type?.toLowerCase() || "") === "tablecell" && cell.children) {
            const cellTexts: string[] = [];
            for (const cellElement of cell.children) {
              const cellText = extractTextFromElement(cellElement);
              if (cellText) cellTexts.push(cellText);
            }
            if (cellTexts.length > 0) rowTexts.push(cellTexts.join(" "));
          }
        }
        if (rowTexts.length > 0) tableTexts.push(rowTexts.join(" | "));
      }
    }
    if (tableTexts.length > 0) {
      text = `\n[Table]\n${tableTexts.join("\n")}\n[/Table]\n`;
    }
  } else if (element.children && Array.isArray(element.children)) {
    const childTexts: string[] = [];
    for (const child of element.children) {
      const childText = extractTextFromElement(child);
      if (childText) childTexts.push(childText);
    }
    text = childTexts.join(" ");
  }

  return text.trim();
}

/** Pull text from `<a:t>` runs in a PPTX slide / notes XML blob. */
function extractPptxRunText(xml: string): string[] {
  const matches = xml.match(/<a:t[^>]*>([^<]+)<\/a:t>/g);
  if (!matches) return [];
  return matches
    .map((match) => match.match(/<a:t[^>]*>([^<]+)<\/a:t>/)?.[1] ?? "")
    .filter((t) => t.trim());
}

function sortByTrailingNumber(re: RegExp): (a: string, b: string) => number {
  return (a, b) => {
    const numA = Number.parseInt(a.match(re)?.[1] || "0", 10);
    const numB = Number.parseInt(b.match(re)?.[1] || "0", 10);
    return numA - numB;
  };
}

async function processPptx(
  arrayBuffer: ArrayBuffer,
  fileName: string,
): Promise<{ extractedText: string }> {
  try {
    const zip = await JSZip.loadAsync(arrayBuffer);

    let extractedText = `<pptx filename="${fileName}">`;

    const slideFiles = Object.keys(zip.files)
      .filter((name) => name.match(/ppt\/slides\/slide\d+\.xml$/))
      .sort(sortByTrailingNumber(/slide(\d+)\.xml$/));

    for (let i = 0; i < slideFiles.length; i++) {
      const slideFile = zip.file(slideFiles[i]);
      if (!slideFile) continue;
      const slideXml = await slideFile.async("text");
      const slideTexts = extractPptxRunText(slideXml);
      if (slideTexts.length > 0) {
        extractedText += `\n<slide number="${i + 1}">\n${slideTexts.join("\n")}\n</slide>`;
      } else {
        extractedText += `\n<slide number="${i + 1}">\n</slide>`;
      }
    }

    const notesFiles = Object.keys(zip.files)
      .filter((name) => name.match(/ppt\/notesSlides\/notesSlide\d+\.xml$/))
      .sort(sortByTrailingNumber(/notesSlide(\d+)\.xml$/));

    if (notesFiles.length > 0) {
      extractedText += "\n<notes>";
      for (const noteFile of notesFiles) {
        const file = zip.file(noteFile);
        if (!file) continue;
        const noteXml = await file.async("text");
        const noteTexts = extractPptxRunText(noteXml);
        if (noteTexts.length > 0) {
          const slideNum = noteFile.match(/notesSlide(\d+)\.xml$/)?.[1];
          extractedText += `\n[Slide ${slideNum} notes]: ${noteTexts.join(" ")}`;
        }
      }
      extractedText += "\n</notes>";
    }

    extractedText += "\n</pptx>";
    return { extractedText };
  } catch (error) {
    console.error("Error processing PPTX:", error);
    throw new Error(`Failed to process PPTX: ${String(error)}`);
  }
}

async function processExcel(
  arrayBuffer: ArrayBuffer,
  fileName: string,
): Promise<{ extractedText: string }> {
  try {
    const workbook = XLSX.read(arrayBuffer, { type: "array" });

    let extractedText = `<excel filename="${fileName}">`;
    for (const [index, sheetName] of workbook.SheetNames.entries()) {
      const worksheet = workbook.Sheets[sheetName];
      const csvText = XLSX.utils.sheet_to_csv(worksheet);
      extractedText += `\n<sheet name="${sheetName}" index="${index + 1}">\n${csvText}\n</sheet>`;
    }
    extractedText += "\n</excel>";
    return { extractedText };
  } catch (error) {
    console.error("Error processing Excel:", error);
    throw new Error(`Failed to process Excel: ${String(error)}`);
  }
}
