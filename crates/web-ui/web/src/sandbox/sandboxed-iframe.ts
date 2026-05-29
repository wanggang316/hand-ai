// <sandbox-iframe> — the browser sandbox element underpinning HTML artifacts and
// the JavaScript REPL.
//
// Two modes:
//   - execute(): a TRANSIENT hidden iframe. Injects the runtime bridge +
//     providers, runs user code, captures console output and the return value,
//     resolves a SandboxResult, enforces a 120s timeout, honours an AbortSignal,
//     then removes the iframe.
//   - loadContent(): a PERSISTENT visible iframe for live HTML artifacts.
//
// prepareHtmlDocument() is public so the artifacts layer can assemble a
// standalone HTML document (runtime injected) for download.
//
// Delivery is `srcdoc` by default, or via a `sandboxUrlProvider` URL (for
// browser-extension CSP). The iframe sandbox attribute is exactly
// `allow-scripts allow-modals`.

import { LitElement } from "lit";
import { customElement, property } from "lit/decorators.js";
import { ConsoleRuntimeProvider, type ConsoleLog } from "./providers/console-provider";
import type { SandboxRuntimeProvider } from "./providers/provider";
import { RuntimeMessageBridge } from "./runtime-message-bridge";
import { type MessageConsumer, RUNTIME_MESSAGE_ROUTER } from "./runtime-message-router";

/** Hard timeout (ms) for transient `execute()` runs. */
export const SANDBOX_EXECUTE_TIMEOUT_MS = 120000;

export interface SandboxFile {
  fileName: string;
  content: string | Uint8Array;
  mimeType: string;
}

/**
 * Result of a transient `execute()` run.
 *
 * `consoleLogs` carries the captured `console.*` output; `returnValue` is the
 * value the user code returned; `files` are any files returned via
 * `returnDownloadableFile`; `error` is set when execution failed.
 */
export interface SandboxResult {
  returnValue?: unknown;
  consoleLogs: ConsoleLog[];
  files?: SandboxFile[];
  error?: { message: string; stack: string };
}

/**
 * Returns the URL of a sandbox HTML host page. Used in browser extensions to
 * load the sandbox via a packaged URL instead of `srcdoc` (strict CSP).
 */
export type SandboxUrlProvider = () => string;

export interface PrepareHtmlOptions {
  /** True for HTML artifacts (inject into existing HTML); false for the REPL (wrap in HTML). */
  isHtmlArtifact: boolean;
  /** True for a standalone download (no runtime bridge, no navigation interceptor). */
  isStandalone?: boolean;
}

/** Escape `</script` so injected code cannot prematurely close the script tag. */
function escapeScriptContent(code: string): string {
  return code.replace(/<\/script/gi, "<\\/script");
}

@customElement("sandbox-iframe")
export class SandboxIframe extends LitElement {
  private iframe?: HTMLIFrameElement;

  /**
   * Optional: a function returning the sandbox host URL. When set, the iframe
   * loads that URL (and posts content via `sandbox-load`) instead of using
   * `srcdoc`. Required for browser extensions with strict CSP.
   */
  @property({ attribute: false }) sandboxUrlProvider?: SandboxUrlProvider;

  // Render into the light DOM: the imperative iframe insertion is load-bearing.
  override createRenderRoot() {
    return this;
  }

  override disconnectedCallback() {
    super.disconnectedCallback();
    // For loadContent() the caller owns sandbox lifecycle; for execute() the
    // sandbox is unregistered in cleanup(). Either way, drop the iframe.
    this.iframe?.remove();
  }

  /**
   * Load HTML content into a persistent visible iframe (HTML artifacts).
   */
  public loadContent(
    sandboxId: string,
    htmlContent: string,
    providers: SandboxRuntimeProvider[] = [],
    consumers: MessageConsumer[] = [],
  ): void {
    try {
      RUNTIME_MESSAGE_ROUTER.unregisterSandbox(sandboxId);
    } catch {
      // Not registered yet; fine.
    }

    providers = [new ConsoleRuntimeProvider(), ...providers];
    RUNTIME_MESSAGE_ROUTER.registerSandbox(sandboxId, providers, consumers);

    const completeHtml = this.prepareHtmlDocument(sandboxId, htmlContent, providers, {
      isHtmlArtifact: true,
      isStandalone: false,
    });

    const validationError = this.validateHtml(completeHtml);
    if (validationError) {
      console.error("HTML validation failed:", validationError);
      this.renderValidationError(validationError);
      return;
    }

    this.iframe?.remove();

    if (this.sandboxUrlProvider) {
      this.loadViaSandboxUrl(sandboxId, completeHtml);
    } else {
      this.loadViaSrcdoc(sandboxId, completeHtml);
    }
  }

  /** Render an HTML validation error page in place of the artifact. */
  private renderValidationError(validationError: string): void {
    this.iframe?.remove();
    this.iframe = document.createElement("iframe");
    this.iframe.style.cssText = "width: 100%; height: 100%; border: none;";
    this.iframe.srcdoc = `
      <html>
      <body style="font-family: monospace; padding: 20px; background: #fff; color: #000;">
        <h3 style="color: #c00;">HTML Validation Error</h3>
        <pre style="background: #f5f5f5; padding: 10px; border-radius: 4px; overflow-x: auto; white-space: pre-wrap;">${validationError}</pre>
      </body>
      </html>
    `;
    this.appendChild(this.iframe);
  }

  private loadViaSandboxUrl(sandboxId: string, completeHtml: string): void {
    this.iframe = document.createElement("iframe");
    this.iframe.sandbox.add("allow-scripts");
    this.iframe.sandbox.add("allow-modals");
    this.iframe.style.width = "100%";
    this.iframe.style.height = "100%";
    this.iframe.style.border = "none";
    this.iframe.src = this.sandboxUrlProvider!();

    RUNTIME_MESSAGE_ROUTER.setSandboxIframe(sandboxId, this.iframe);

    // Open external links/forms in a new tab instead of navigating the iframe.
    const externalUrlHandler = (e: MessageEvent) => {
      if (e.data?.type === "open-external-url" && e.source === this.iframe?.contentWindow) {
        window.open(e.data.url, "_blank");
      }
    };
    window.addEventListener("message", externalUrlHandler);

    // Hand the content to the host page once it announces readiness.
    const readyHandler = (e: MessageEvent) => {
      if (e.data?.type === "sandbox-ready" && e.source === this.iframe?.contentWindow) {
        window.removeEventListener("message", readyHandler);
        window.removeEventListener("message", errorHandler);
        this.iframe?.contentWindow?.postMessage(
          { type: "sandbox-load", sandboxId, code: completeHtml },
          "*",
        );
      }
    };

    const errorHandler = (e: MessageEvent) => {
      if (e.data?.type === "sandbox-error" && e.source === this.iframe?.contentWindow) {
        window.removeEventListener("message", readyHandler);
        window.removeEventListener("message", errorHandler);
        // Convert into an execution-error the execute() consumer understands.
        window.postMessage(
          {
            sandboxId,
            type: "execution-error",
            error: { message: e.data.error, stack: e.data.stack },
          },
          "*",
        );
      }
    };

    window.addEventListener("message", readyHandler);
    window.addEventListener("message", errorHandler);

    this.appendChild(this.iframe);
  }

  private loadViaSrcdoc(sandboxId: string, completeHtml: string): void {
    this.iframe = document.createElement("iframe");
    this.iframe.sandbox.add("allow-scripts");
    this.iframe.sandbox.add("allow-modals");
    this.iframe.style.width = "100%";
    this.iframe.style.height = "100%";
    this.iframe.style.border = "none";
    this.iframe.srcdoc = completeHtml;

    RUNTIME_MESSAGE_ROUTER.setSandboxIframe(sandboxId, this.iframe);

    const externalUrlHandler = (e: MessageEvent) => {
      if (e.data?.type === "open-external-url" && e.source === this.iframe?.contentWindow) {
        window.open(e.data.url, "_blank");
      }
    };
    window.addEventListener("message", externalUrlHandler);

    this.appendChild(this.iframe);
  }

  /**
   * Execute code in a transient hidden iframe and resolve a SandboxResult.
   *
   * @param sandboxId Unique id for this execution.
   * @param code User code (plain JS for the REPL, or full HTML for artifacts).
   * @param providers Runtime providers (ConsoleRuntimeProvider is prepended).
   * @param consumers Additional message consumers (execute adds its own).
   * @param signal Optional abort signal.
   * @param isHtmlArtifact When true, treat `code` as HTML to inject runtime into.
   */
  public async execute(
    sandboxId: string,
    code: string,
    providers: SandboxRuntimeProvider[] = [],
    consumers: MessageConsumer[] = [],
    signal?: AbortSignal,
    isHtmlArtifact: boolean = false,
  ): Promise<SandboxResult> {
    if (signal?.aborted) {
      throw new Error("Execution aborted");
    }

    const consoleProvider = new ConsoleRuntimeProvider();
    providers = [consoleProvider, ...providers];
    RUNTIME_MESSAGE_ROUTER.registerSandbox(sandboxId, providers, consumers);

    for (const provider of providers) {
      provider.onExecutionStart?.(sandboxId, signal);
    }

    const files: SandboxFile[] = [];
    let completed = false;

    return new Promise<SandboxResult>((resolve, reject) => {
      const executionConsumer: MessageConsumer = {
        async handleMessage(message: unknown): Promise<void> {
          const m = message as {
            type?: string;
            fileName?: string;
            content?: string | Uint8Array;
            mimeType?: string;
            returnValue?: unknown;
            error?: { message: string; stack: string };
          };
          if (m.type === "file-returned") {
            files.push({
              fileName: m.fileName ?? "file",
              content: m.content ?? "",
              mimeType: m.mimeType ?? "application/octet-stream",
            });
          } else if (m.type === "execution-complete") {
            completed = true;
            cleanup();
            resolve({
              returnValue: m.returnValue,
              consoleLogs: consoleProvider.getLogs(),
              files,
            });
          } else if (m.type === "execution-error") {
            completed = true;
            cleanup();
            resolve({
              consoleLogs: consoleProvider.getLogs(),
              error: m.error,
              files,
            });
          }
        },
      };

      RUNTIME_MESSAGE_ROUTER.addConsumer(sandboxId, executionConsumer);

      const cleanup = () => {
        for (const provider of providers) {
          provider.onExecutionEnd?.(sandboxId);
        }
        RUNTIME_MESSAGE_ROUTER.unregisterSandbox(sandboxId);
        signal?.removeEventListener("abort", abortHandler);
        clearTimeout(timeoutId);
        this.iframe?.remove();
        this.iframe = undefined;
      };

      const abortHandler = () => {
        if (!completed) {
          completed = true;
          cleanup();
          reject(new Error("Execution aborted"));
        }
      };

      if (signal) {
        signal.addEventListener("abort", abortHandler);
      }

      const timeoutId = setTimeout(() => {
        if (!completed) {
          completed = true;
          cleanup();
          resolve({
            consoleLogs: consoleProvider.getLogs(),
            error: { message: "Execution timeout (120s)", stack: "" },
            files,
          });
        }
      }, SANDBOX_EXECUTE_TIMEOUT_MS);

      const completeHtml = this.prepareHtmlDocument(sandboxId, code, providers, {
        isHtmlArtifact,
        isStandalone: false,
      });

      const validationError = this.validateHtml(completeHtml);
      if (validationError) {
        cleanup();
        reject(new Error(`HTML validation failed: ${validationError}`));
        return;
      }

      if (this.sandboxUrlProvider) {
        this.iframe = document.createElement("iframe");
        this.iframe.sandbox.add("allow-scripts", "allow-modals");
        this.iframe.style.cssText = "width: 100%; height: 100%; border: none;";
        this.iframe.src = this.sandboxUrlProvider();

        RUNTIME_MESSAGE_ROUTER.setSandboxIframe(sandboxId, this.iframe);

        const readyHandler = (e: MessageEvent) => {
          if (e.data?.type === "sandbox-ready" && e.source === this.iframe?.contentWindow) {
            window.removeEventListener("message", readyHandler);
            window.removeEventListener("message", errorHandler);
            this.iframe?.contentWindow?.postMessage(
              { type: "sandbox-load", sandboxId, code: completeHtml },
              "*",
            );
          }
        };

        const errorHandler = (e: MessageEvent) => {
          if (e.data?.type === "sandbox-error" && e.source === this.iframe?.contentWindow) {
            window.removeEventListener("message", readyHandler);
            window.removeEventListener("message", errorHandler);
            window.postMessage(
              {
                sandboxId,
                type: "execution-error",
                error: { message: e.data.error, stack: e.data.stack },
              },
              "*",
            );
          }
        };

        window.addEventListener("message", readyHandler);
        window.addEventListener("message", errorHandler);

        this.appendChild(this.iframe);
      } else {
        this.iframe = document.createElement("iframe");
        this.iframe.sandbox.add("allow-scripts", "allow-modals");
        this.iframe.style.cssText = "width: 100%; height: 100%; border: none; display: none;";
        this.iframe.srcdoc = completeHtml;

        RUNTIME_MESSAGE_ROUTER.setSandboxIframe(sandboxId, this.iframe);

        this.appendChild(this.iframe);
      }
    });
  }

  /**
   * Validate HTML with DOMParser. Returns an error message if a `parsererror`
   * node is present, otherwise null. JS syntax is validated inside the sandbox.
   */
  private validateHtml(html: string): string | null {
    try {
      const parser = new DOMParser();
      const doc = parser.parseFromString(html, "text/html");
      const parserError = doc.querySelector("parsererror");
      if (parserError) {
        return parserError.textContent || "Unknown parse error";
      }
      return null;
    } catch (error) {
      return (error as Error).message || "Unknown validation error";
    }
  }

  /**
   * Assemble a complete HTML document with the runtime + user code. PUBLIC so
   * the artifacts layer can build a standalone HTML document for download.
   */
  public prepareHtmlDocument(
    sandboxId: string,
    userCode: string,
    providers: SandboxRuntimeProvider[] = [],
    options?: PrepareHtmlOptions,
  ): string {
    const opts: PrepareHtmlOptions = {
      isHtmlArtifact: false,
      isStandalone: false,
      ...options,
    };

    const runtime = this.getRuntimeScript(sandboxId, providers, opts.isStandalone || false);

    if (opts.isHtmlArtifact) {
      // Inject the runtime into the existing HTML, after <head> or <html>.
      const headMatch = userCode.match(/<head[^>]*>/i);
      if (headMatch) {
        const index = headMatch.index! + headMatch[0].length;
        return userCode.slice(0, index) + runtime + userCode.slice(index);
      }

      const htmlMatch = userCode.match(/<html[^>]*>/i);
      if (htmlMatch) {
        const index = htmlMatch.index! + htmlMatch[0].length;
        return userCode.slice(0, index) + runtime + userCode.slice(index);
      }

      return runtime + userCode;
    }

    // REPL: wrap the code in an async function so we can capture its return
    // value, then call window.complete() (after draining completion callbacks).
    const escapedUserCode = escapeScriptContent(userCode);

    return `<!DOCTYPE html>
<html>
<head>
	${runtime}
</head>
<body>
	<script type="module">
		(async () => {
			try {
				const userCodeFunc = async () => {
					${escapedUserCode}
				};

				const returnValue = await userCodeFunc();

				if (window.__completionCallbacks && window.__completionCallbacks.length > 0) {
					try {
						await Promise.all(window.__completionCallbacks.map(cb => cb(true)));
					} catch (e) {
						console.error('Completion callback error:', e);
					}
				}

				await window.complete(null, returnValue);
			} catch (error) {
				if (window.__completionCallbacks && window.__completionCallbacks.length > 0) {
					try {
						await Promise.all(window.__completionCallbacks.map(cb => cb(false)));
					} catch (e) {
						console.error('Completion callback error:', e);
					}
				}

				await window.complete({
					message: error?.message || String(error),
					stack: error?.stack || new Error().stack
				});
			}
		})();
	</script>
</body>
</html>`;
  }

  /**
   * Generate the runtime <script> from providers: window-scoped data injection,
   * the message bridge (unless standalone), each provider's runtime function via
   * `.toString()`, and the navigation interceptor (unless standalone).
   */
  private getRuntimeScript(
    sandboxId: string,
    providers: SandboxRuntimeProvider[] = [],
    isStandalone: boolean = false,
  ): string {
    const allData: Record<string, unknown> = {};
    for (const provider of providers) {
      Object.assign(allData, provider.getData());
    }

    const bridgeCode = isStandalone
      ? ""
      : RuntimeMessageBridge.generateBridgeCode({ context: "sandbox-iframe", sandboxId });

    // Each provider's runtime function is stringified and invoked with the
    // sandboxId. The .toString() injection requires self-contained function
    // bodies (no closures, no imports).
    const runtimeFunctions: string[] = [];
    for (const provider of providers) {
      runtimeFunctions.push(
        `(${provider.getRuntime().toString()})(${JSON.stringify(sandboxId)});`,
      );
    }

    const dataInjection = Object.entries(allData)
      .map(([key, value]) => {
        const jsonStr = JSON.stringify(value).replace(/<\/script/gi, "<\\/script");
        return `window.${key} = ${jsonStr};`;
      })
      .join("\n");

    const navigationInterceptor = isStandalone
      ? ""
      : `
// Navigation interceptor: prevent navigation and open externally instead.
(function() {
	document.addEventListener('click', function(e) {
		const link = e.target.closest('a');
		if (link && link.href) {
			if (link.href.startsWith('http://') || link.href.startsWith('https://')) {
				e.preventDefault();
				e.stopPropagation();
				window.parent.postMessage({ type: 'open-external-url', url: link.href }, '*');
			}
		}
	}, true);

	document.addEventListener('submit', function(e) {
		const form = e.target;
		if (form && form.action) {
			e.preventDefault();
			e.stopPropagation();
			window.parent.postMessage({ type: 'open-external-url', url: form.action }, '*');
		}
	}, true);

	try {
		const originalLocation = window.location;
		Object.defineProperty(window, 'location', {
			get: function() { return originalLocation; },
			set: function(url) {
				window.parent.postMessage({ type: 'open-external-url', url: url.toString() }, '*');
			}
		});
	} catch (e) {
		// Already defined, skip
	}
})();
`;

    return `<style>
html, body {
	font-size: initial;
}
</style>
<script>
window.sandboxId = ${JSON.stringify(sandboxId)};
${dataInjection}
${bridgeCode}
${runtimeFunctions.join("\n")}
${navigationInterceptor}
</script>`;
  }
}

declare global {
  interface HTMLElementTagNameMap {
    "sandbox-iframe": SandboxIframe;
  }
}
