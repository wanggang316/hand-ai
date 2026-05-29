// Generates the in-iframe `sendRuntimeMessage()` bridge function as injectable
// source code. The bridge provides a unified request/response messaging API
// between the sandboxed iframe and its host window. The generated code is a
// self-contained string (no closures over outer scope, no imports) so it can be
// dropped directly into a <script> tag inside the iframe document.

export type MessageType = "request-response" | "fire-and-forget";

export interface RuntimeMessageBridgeOptions {
  /**
   * Execution context. Only `sandbox-iframe` is supported here; the reference
   * `user-script` (browser-extension) variant is intentionally omitted.
   */
  context: "sandbox-iframe";
  sandboxId: string;
}

/**
 * Produces the injectable bridge JavaScript. The generated code installs
 * `window.sendRuntimeMessage` (a Promise-returning postMessage round-trip keyed
 * by a generated message id) and `window.onCompleted` (a registry of completion
 * callbacks the REPL wrapper awaits before signalling completion).
 */
export class RuntimeMessageBridge {
  static generateBridgeCode(options: RuntimeMessageBridgeOptions): string {
    return RuntimeMessageBridge.generateSandboxBridge(options.sandboxId);
  }

  private static generateSandboxBridge(sandboxId: string): string {
    // Stringified bridge that posts to window.parent and resolves on the
    // matching `runtime-response` reply.
    return `
window.__completionCallbacks = [];
window.sendRuntimeMessage = async (message) => {
    const messageId = 'msg_' + Date.now() + '_' + Math.random().toString(36).substring(2, 9);

    return new Promise((resolve, reject) => {
        const handler = (e) => {
            if (e.data.type === 'runtime-response' && e.data.messageId === messageId) {
                window.removeEventListener('message', handler);
                if (e.data.success) {
                    resolve(e.data);
                } else {
                    reject(new Error(e.data.error || 'Operation failed'));
                }
            }
        };

        window.addEventListener('message', handler);

        window.parent.postMessage({
            ...message,
            sandboxId: ${JSON.stringify(sandboxId)},
            messageId: messageId
        }, '*');

        // Timeout after 30s
        setTimeout(() => {
            window.removeEventListener('message', handler);
            reject(new Error('Runtime message timeout'));
        }, 30000);
    });
};
window.onCompleted = (callback) => {
    window.__completionCallbacks.push(callback);
};
`.trim();
  }
}
