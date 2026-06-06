// CORS-error detection plus the document-fetch proxy resolver. The only
// browser-side network call that needs either is `extract_document`, which
// fetches a document directly from the browser and must (a) optionally route
// through a user-configured CORS proxy and (b) surface a helpful fallback
// message when the remote server blocks cross-origin reads. (Server-side LLM
// streaming has no client CORS layer.)

import { getAppStorage } from "../storage/app-storage";

// SettingsStore keys for the document-fetch proxy, mirrored from <proxy-tab>.
// Inlined (rather than imported) so this util does not pull the dialog module's
// custom-element registration side-effects into the tools bundle.
const PROXY_ENABLED_KEY = "proxy.enabled";
const PROXY_URL_KEY = "proxy.url";

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

/**
 * Resolve the URL the `extract_document` tool should fetch, honoring the
 * document-fetch proxy configured in settings. When `proxy.enabled` is true and
 * `proxy.url` is set, the target is wrapped as `<proxy-url>/?url=<encoded>` (the
 * format the ProxyTab documents); otherwise the original URL is returned.
 *
 * Failures reading settings degrade gracefully to the original URL.
 */
export async function resolveDocumentFetchUrl(targetUrl: string): Promise<string> {
  try {
    const { settings } = getAppStorage();
    const enabled = await settings.get<boolean>(PROXY_ENABLED_KEY);
    if (!enabled) return targetUrl;
    const proxyUrl = (await settings.get<string>(PROXY_URL_KEY))?.trim();
    if (!proxyUrl) return targetUrl;
    return `${proxyUrl.replace(/\/$/, "")}/?url=${encodeURIComponent(targetUrl)}`;
  } catch {
    return targetUrl;
  }
}
