// Console runtime provider — REQUIRED and injected first.
//
// Overrides `console.*` inside the sandbox, forwards each call to the host via
// `sendRuntimeMessage`, installs error / unhandledrejection handlers, and
// exposes `window.complete()` which the REPL wrapper calls to signal completion.
// On the host side it collects the forwarded console logs for retrieval.

import type { SandboxRuntimeProvider } from "./provider";

export interface ConsoleLog {
  type: "log" | "warn" | "error" | "info";
  text: string;
  args?: unknown[];
}

export class ConsoleRuntimeProvider implements SandboxRuntimeProvider {
  private logs: ConsoleLog[] = [];
  private completionError: { message: string; stack: string } | null = null;
  private completed = false;

  getData(): Record<string, unknown> {
    return {};
  }

  getDescription(): string {
    return "";
  }

  getRuntime(): (sandboxId: string) => void {
    // Self-contained: stringified and injected. No outer references.
    return (_sandboxId: string) => {
      const w = window as unknown as Record<string, unknown>;

      // Capture the truly-original console methods on first wrap only, so
      // repeated executions never accumulate wrapper layers.
      if (!w.__originalConsole) {
        w.__originalConsole = {
          log: console.log.bind(console),
          error: console.error.bind(console),
          warn: console.warn.bind(console),
          info: console.info.bind(console),
        };
      }
      const originalConsole = w.__originalConsole as Record<string, (...a: unknown[]) => void>;

      // Track in-flight forwarding promises so onCompleted can drain them.
      const pendingSends: Promise<unknown>[] = [];

      (["log", "error", "warn", "info"] as const).forEach((method) => {
        (console as unknown as Record<string, unknown>)[method] = (...args: unknown[]) => {
          const text = args
            .map((arg) => {
              try {
                return typeof arg === "object" ? JSON.stringify(arg) : String(arg);
              } catch {
                return String(arg);
              }
            })
            .join(" ");

          // Mirror locally using the truly-original console.
          originalConsole[method].apply(console, args);

          // Forward to the host (only present when the bridge is installed).
          const send = w.sendRuntimeMessage as
            | ((m: unknown) => Promise<unknown>)
            | undefined;
          if (send) {
            const sendPromise = send({ type: "console", method, text, args }).catch(() => {});
            pendingSends.push(sendPromise);
          }
        };
      });

      // Drain pending console forwards before completion is reported.
      const onCompleted = w.onCompleted as
        | ((cb: (success: boolean) => Promise<void>) => void)
        | undefined;
      if (onCompleted) {
        onCompleted(async (_success: boolean) => {
          if (pendingSends.length > 0) {
            await Promise.all(pendingSends);
          }
        });
      }

      // Capture errors so HTML artifacts/REPL can surface them via complete().
      let lastError: { message: string; stack: string } | null = null;

      window.addEventListener("error", (e: ErrorEvent) => {
        const text = `${e.error?.stack || e.message || String(e)} at line ${e.lineno || "?"}:${e.colno || "?"}`;
        lastError = {
          message: e.error?.message || e.message || String(e),
          stack: e.error?.stack || text,
        };
      });

      window.addEventListener("unhandledrejection", (e: PromiseRejectionEvent) => {
        const reason = e.reason as { message?: string; stack?: string } | undefined;
        const text = `Unhandled promise rejection: ${reason?.message || reason || "Unknown error"}`;
        lastError = {
          message: reason?.message || String(e.reason) || "Unhandled promise rejection",
          stack: reason?.stack || text,
        };
      });

      // complete() is called by the REPL wrapper (or user code) to finish.
      let completionSent = false;
      w.complete = async (error?: { message: string; stack: string }, returnValue?: unknown) => {
        if (completionSent) return;
        completionSent = true;

        const finalError = error || lastError;
        const send = w.sendRuntimeMessage as ((m: unknown) => Promise<unknown>) | undefined;
        if (send) {
          if (finalError) {
            await send({ type: "execution-error", error: finalError });
          } else {
            await send({ type: "execution-complete", returnValue });
          }
        }
      };
    };
  }

  async handleMessage(
    message: unknown,
    respond: (response: Record<string, unknown>) => void,
  ): Promise<void> {
    const msg = message as { type?: string; method?: string; text?: string; args?: unknown[] };
    if (msg.type === "console") {
      this.logs.push({
        type:
          msg.method === "error"
            ? "error"
            : msg.method === "warn"
              ? "warn"
              : msg.method === "info"
                ? "info"
                : "log",
        text: msg.text ?? "",
        args: msg.args,
      });
      respond({ success: true });
    }
  }

  /** Collected console logs. */
  getLogs(): ConsoleLog[] {
    return this.logs;
  }

  isCompleted(): boolean {
    return this.completed;
  }

  getCompletionError(): { message: string; stack: string } | null {
    return this.completionError;
  }

  /** Reset state for reuse. */
  reset(): void {
    this.logs = [];
    this.completionError = null;
    this.completed = false;
  }
}
