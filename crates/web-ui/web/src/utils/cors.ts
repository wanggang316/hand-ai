// CORS-error detection. The only browser-side network call that needs this is
// `extract_document`, which fetches a document directly from the browser and
// must surface a helpful fallback message when the remote server blocks
// cross-origin reads. (Server-side LLM streaming has no client CORS layer.)

/**
 * Heuristically classify an error as a CORS / cross-origin fetch failure.
 *
 * Browsers deliberately hide CORS detail from script, so the standard signal is
 * a `TypeError: Failed to fetch`. We also match the explicit `NetworkError`
 * name and any message mentioning CORS / cross-origin.
 */
export function isCorsError(error: unknown): boolean {
  if (!(error instanceof Error)) {
    return false;
  }

  const message = error.message.toLowerCase();

  // "Failed to fetch" is the standard CORS error in most browsers.
  if (error.name === "TypeError" && message.includes("failed to fetch")) {
    return true;
  }

  // Some browsers report "NetworkError".
  if (error.name === "NetworkError") {
    return true;
  }

  // CORS-specific messages.
  if (message.includes("cors") || message.includes("cross-origin")) {
    return true;
  }

  return false;
}
