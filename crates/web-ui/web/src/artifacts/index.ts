// Public artifacts-subsystem API. Importing this module defines every artifact
// custom element (side effect) so the panel can instantiate viewers by class.

export { ArtifactElement } from "./artifact-element";
export {
  type Artifact,
  type ArtifactsAgentTool,
  type ArtifactsParams,
  ArtifactsPanel,
} from "./artifacts-panel";
export { ArtifactsToolRenderer } from "./artifacts-tool-renderer";
export { ArtifactPill } from "./artifact-pill";
export { getFileType, type ArtifactFileType } from "./file-type";

export { HtmlArtifact } from "./html-artifact";
export { SvgArtifact } from "./svg-artifact";
export { MarkdownArtifact } from "./markdown-artifact";
export { TextArtifact } from "./text-artifact";
export { ImageArtifact } from "./image-artifact";
export { PdfArtifact } from "./pdf-artifact";
export { DocxArtifact } from "./docx-artifact";
export { ExcelArtifact } from "./excel-artifact";
export { GenericArtifact } from "./generic-artifact";
export { ArtifactConsole, type ArtifactConsoleLog } from "./console";
