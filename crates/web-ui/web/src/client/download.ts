// Out-of-band download helper for server-produced artifacts (e.g. the
// `export_html` output file). The server writes the file to the session cwd and
// returns its path; `registerDownload` maps that path to a download id, and
// `triggerBrowserDownload` fetches `GET /download/:id` and saves it locally.

/** Register a server-produced file path for download; returns the download id. */
export async function registerDownload(path: string): Promise<string> {
  const response = await fetch("/download/register", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ path }),
  });
  if (!response.ok) {
    const detail = await response.text().catch(() => "");
    throw new Error(
      `Failed to prepare download (${response.status}): ${detail || response.statusText}`,
    );
  }
  const data = (await response.json()) as { id: string };
  return data.id;
}

/**
 * Fetch `GET /download/:id` and save the bytes via a temporary anchor. The
 * server sets `Content-Disposition: attachment`, so a direct anchor navigation
 * would also work; fetching first lets us surface transport errors and derive a
 * filename when the server-suggested one is unavailable.
 */
export async function triggerBrowserDownload(id: string, fileName?: string): Promise<void> {
  const response = await fetch(`/download/${encodeURIComponent(id)}`);
  if (!response.ok) {
    throw new Error(`Download failed (${response.status}): ${response.statusText}`);
  }
  const blob = await response.blob();
  const objectUrl = URL.createObjectURL(blob);
  try {
    const anchor = document.createElement("a");
    anchor.href = objectUrl;
    anchor.download = fileName ?? deriveFileName(response) ?? "download";
    document.body.appendChild(anchor);
    anchor.click();
    anchor.remove();
  } finally {
    URL.revokeObjectURL(objectUrl);
  }
}

/**
 * Register a server export path and immediately trigger its browser download.
 * Convenience wrapper for the `export_html` response → download flow.
 */
export async function downloadServerFile(path: string): Promise<void> {
  const id = await registerDownload(path);
  const fileName = path.split(/[/\\]/).pop() || undefined;
  await triggerBrowserDownload(id, fileName);
}

/** Parse a filename out of a `Content-Disposition` response header, if present. */
function deriveFileName(response: Response): string | undefined {
  const disposition = response.headers.get("content-disposition");
  if (!disposition) return undefined;
  const match = disposition.match(/filename="?([^"]+)"?/i);
  return match?.[1];
}
