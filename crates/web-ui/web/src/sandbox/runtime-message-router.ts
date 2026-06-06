// Centralized message router for all sandbox runtime communication. A single
// global `message` listener routes inbound postMessages from sandbox iframes to
// the right registered sandbox: provider `handleMessage` hooks run first (for
// bidirectional request/response), then consumers receive a broadcast (for
// one-way lifecycle messages such as console output and execution-complete).

import type { SandboxRuntimeProvider } from "./providers/provider";

/**
 * Components that want to receive messages from sandboxes. All consumers receive
 * all messages and decide internally what to handle.
 */
export interface MessageConsumer {
  handleMessage(message: unknown): Promise<void>;
}

/** Internal per-sandbox bookkeeping. */
interface SandboxContext {
  sandboxId: string;
  /** null until `setSandboxIframe()` is called after the iframe is created. */
  iframe: HTMLIFrameElement | null;
  providers: SandboxRuntimeProvider[];
  consumers: Set<MessageConsumer>;
}

/**
 * Routes runtime messages to the appropriate sandbox instance/handler.
 *
 * One global `message` listener replaces per-sandbox listeners. The listener is
 * installed lazily on first registration and removed once the last sandbox is
 * unregistered, so an idle app holds no global listener.
 */
export class RuntimeMessageRouter {
  private sandboxes = new Map<string, SandboxContext>();
  private messageListener: ((e: MessageEvent) => void) | null = null;

  /**
   * Register a sandbox with its runtime providers and consumers. Call BEFORE
   * creating the iframe so an early message is never dropped.
   */
  registerSandbox(
    sandboxId: string,
    providers: SandboxRuntimeProvider[],
    consumers: MessageConsumer[],
  ): void {
    this.sandboxes.set(sandboxId, {
      sandboxId,
      iframe: null,
      providers,
      consumers: new Set(consumers),
    });
    this.setupListener();
  }

  /**
   * Update the iframe reference for a sandbox. Call AFTER creating the iframe so
   * providers can post responses back into it.
   */
  setSandboxIframe(sandboxId: string, iframe: HTMLIFrameElement): void {
    const context = this.sandboxes.get(sandboxId);
    if (context) {
      context.iframe = iframe;
    }
  }

  /** Unregister a sandbox; tears down the global listener when none remain. */
  unregisterSandbox(sandboxId: string): void {
    this.sandboxes.delete(sandboxId);

    if (this.sandboxes.size === 0 && this.messageListener) {
      window.removeEventListener("message", this.messageListener);
      this.messageListener = null;
    }
  }

  /** Add a consumer to a sandbox (receives broadcast messages). */
  addConsumer(sandboxId: string, consumer: MessageConsumer): void {
    this.sandboxes.get(sandboxId)?.consumers.add(consumer);
  }

  /** Remove a consumer from a sandbox. */
  removeConsumer(sandboxId: string, consumer: MessageConsumer): void {
    this.sandboxes.get(sandboxId)?.consumers.delete(consumer);
  }

  /** Install the global message listener once. */
  private setupListener(): void {
    if (this.messageListener) return;

    this.messageListener = async (e: MessageEvent) => {
      const data = e.data as { sandboxId?: string; messageId?: string } | null;
      if (!data || !data.sandboxId) return;

      const context = this.sandboxes.get(data.sandboxId);
      if (!context) return;

      // respond() lets providers reply to the originating iframe.
      const respond = (response: Record<string, unknown>) => {
        context.iframe?.contentWindow?.postMessage(
          {
            type: "runtime-response",
            messageId: data.messageId,
            sandboxId: data.sandboxId,
            ...response,
          },
          "*",
        );
      };

      // 1. Provider handlers first (bidirectional comm). Do not stop early —
      // every provider sees the message, and consumers see it afterwards.
      for (const provider of context.providers) {
        if (provider.handleMessage) {
          await provider.handleMessage(data, respond);
        }
      }

      // 2. Broadcast to consumers (one-way lifecycle messages).
      for (const consumer of context.consumers) {
        await consumer.handleMessage(data);
      }
    };

    window.addEventListener("message", this.messageListener);
  }
}

/**
 * Global singleton router instance. Import this wherever you need to interact
 * with sandbox runtime messaging.
 */
export const RUNTIME_MESSAGE_ROUTER = new RuntimeMessageRouter();
