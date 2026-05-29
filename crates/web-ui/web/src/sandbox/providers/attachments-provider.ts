// Attachments runtime provider — exposes read-only attachment globals to
// sandboxed code (`listAttachments`, `readTextAttachment`, `readBinaryAttachment`).
//
// Attachments are a read-only snapshot injected via `getData()`, so the runtime
// functions read straight from `window.attachments` and need no host messaging.
// This works identically online and offline.

import type { Attachment } from "../../core/messages";
import type { SandboxRuntimeProvider } from "./provider";

const ATTACHMENTS_RUNTIME_DESCRIPTION = `
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

export class AttachmentsRuntimeProvider implements SandboxRuntimeProvider {
  constructor(private attachments: Attachment[]) {}

  getData(): Record<string, unknown> {
    const attachmentsData = this.attachments.map((a) => ({
      id: a.id,
      fileName: a.fileName,
      mimeType: a.mimeType,
      size: a.size,
      content: a.content,
      extractedText: a.extractedText,
    }));
    return { attachments: attachmentsData };
  }

  getRuntime(): (sandboxId: string) => void {
    // Self-contained: stringified and injected. Reads from window.attachments.
    return (_sandboxId: string) => {
      const w = window as unknown as Record<string, unknown>;
      type WireAttachment = {
        id: string;
        fileName: string;
        mimeType: string;
        size: number;
        content: string;
        extractedText?: string;
      };

      w.listAttachments = () =>
        ((w.attachments as WireAttachment[]) || []).map((a) => ({
          id: a.id,
          fileName: a.fileName,
          mimeType: a.mimeType,
          size: a.size,
        }));

      w.readTextAttachment = (attachmentId: string) => {
        const a = ((w.attachments as WireAttachment[]) || []).find((x) => x.id === attachmentId);
        if (!a) throw new Error(`Attachment not found: ${attachmentId}`);
        if (a.extractedText) return a.extractedText;
        try {
          return atob(a.content);
        } catch {
          throw new Error(`Failed to decode text content for: ${attachmentId}`);
        }
      };

      w.readBinaryAttachment = (attachmentId: string) => {
        const a = ((w.attachments as WireAttachment[]) || []).find((x) => x.id === attachmentId);
        if (!a) throw new Error(`Attachment not found: ${attachmentId}`);
        const bin = atob(a.content);
        const bytes = new Uint8Array(bin.length);
        for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
        return bytes;
      };
    };
  }

  getDescription(): string {
    return ATTACHMENTS_RUNTIME_DESCRIPTION;
  }
}
