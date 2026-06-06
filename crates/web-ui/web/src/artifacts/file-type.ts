// File-type dispatch for artifacts. Maps a filename extension to the artifact
// viewer kind the panel should instantiate.

export type ArtifactFileType =
  | "html"
  | "svg"
  | "markdown"
  | "image"
  | "pdf"
  | "excel"
  | "docx"
  | "text"
  | "generic";

const TEXT_EXTENSIONS = new Set([
  "txt",
  "json",
  "xml",
  "yaml",
  "yml",
  "csv",
  "js",
  "ts",
  "jsx",
  "tsx",
  "py",
  "java",
  "c",
  "cpp",
  "h",
  "css",
  "scss",
  "sass",
  "less",
  "sh",
]);

const IMAGE_EXTENSIONS = new Set(["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico"]);

/** Determine the artifact viewer kind from a filename. */
export function getFileType(filename: string): ArtifactFileType {
  const ext = filename.split(".").pop()?.toLowerCase();
  if (!ext) return "generic";
  if (ext === "html") return "html";
  if (ext === "svg") return "svg";
  if (ext === "md" || ext === "markdown") return "markdown";
  if (ext === "pdf") return "pdf";
  if (ext === "xlsx" || ext === "xls") return "excel";
  if (ext === "docx") return "docx";
  if (IMAGE_EXTENSIONS.has(ext)) return "image";
  if (TEXT_EXTENSIONS.has(ext)) return "text";
  return "generic";
}
