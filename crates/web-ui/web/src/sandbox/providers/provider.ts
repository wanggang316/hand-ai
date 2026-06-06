// Interface for providing runtime capabilities to sandboxed iframes. Each
// provider injects window-scoped data and a runtime function into the sandbox.

export interface SandboxRuntimeProvider {
  /**
   * Data to inject into the iframe window scope. Keys become window properties
   * (e.g. `{ attachments: [...] }` -> `window.attachments`).
   */
  getData(): Record<string, unknown>;

  /**
   * Returns a runtime function that is stringified via `.toString()` and
   * executed inside the sandbox. The function receives the sandboxId and reads
   * its data from `window`.
   *
   * IMPORTANT: because the function is serialized with `.toString()`, its body
   * MUST be fully self-contained — no closures over outer variables and no
   * imports. This constraint is load-bearing; violating it silently breaks the
   * injected runtime.
   */
  getRuntime(): (sandboxId: string) => void;

  /**
   * Optional bidirectional message handler. All providers receive all messages
   * and decide internally what to handle; `respond` posts a reply back to the
   * sandbox.
   */
  handleMessage?(message: unknown, respond: (response: Record<string, unknown>) => void): Promise<void>;

  /**
   * Documentation describing the globals/functions this provider injects. This
   * is appended to tool descriptions so the model knows what is available.
   */
  getDescription(): string;

  /** Lifecycle: invoked when sandbox execution starts. */
  onExecutionStart?(sandboxId: string, signal?: AbortSignal): void;

  /** Lifecycle: invoked when sandbox execution ends (success, error, or abort). */
  onExecutionEnd?(sandboxId: string): void;
}
