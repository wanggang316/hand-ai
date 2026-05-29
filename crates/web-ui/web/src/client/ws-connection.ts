// WebSocket lifecycle: framing, send buffering before open, and frame fan-out.
// One JSON object per text frame, matching the server's line protocol.

import type { ClientCommand, ResponseFrame, ServerFrame } from "./wire";

export type FrameHandler = (frame: ServerFrame) => void;

/** A command awaiting its correlated `response` frame. */
interface PendingRequest {
  resolve: (data: unknown) => void;
  reject: (err: Error) => void;
  timer: ReturnType<typeof setTimeout>;
}

/** Default request timeout: reject if no matching response arrives in time. */
const REQUEST_TIMEOUT_MS = 30_000;

/** Initial reconnect backoff; doubles up to {@link MAX_RECONNECT_DELAY_MS}. */
const INITIAL_RECONNECT_DELAY_MS = 1000;
const MAX_RECONNECT_DELAY_MS = 15_000;

export class WsConnection {
  private ws!: WebSocket;
  private handlers = new Set<FrameHandler>();
  private sendQueue: string[] = [];
  private isOpen = false;

  // Correlated request/response: each `request()` injects a unique `id` and
  // parks a resolver here until the matching `response` frame (same `id`)
  // arrives. Event frames are unaffected and continue to fan out via handlers.
  private pendingRequests = new Map<string, PendingRequest>();
  private nextRequestId = 1;

  // Auto-reconnect state. On an unexpected close the socket is re-opened with a
  // capped exponential backoff; `close()` sets `closedByUser` to stop that.
  // Frame subscribers (`onFrame`) persist across reconnects since they live on
  // this instance, not the socket. A reconnect yields a fresh server-side
  // session (per-connection model); browser-side state is unaffected.
  private closedByUser = false;
  private reconnectDelayMs = INITIAL_RECONNECT_DELAY_MS;

  constructor(private readonly url: string) {
    this.connect();
  }

  private connect(): void {
    this.ws = new WebSocket(this.url);
    this.ws.addEventListener("open", () => {
      this.isOpen = true;
      this.reconnectDelayMs = INITIAL_RECONNECT_DELAY_MS; // reset backoff
      for (const msg of this.sendQueue) this.ws.send(msg);
      this.sendQueue = [];
    });
    this.ws.addEventListener("message", (event) => {
      let frame: ServerFrame;
      try {
        frame = JSON.parse(event.data as string) as ServerFrame;
      } catch {
        return;
      }
      // Settle a correlated request first; still fan the frame out so existing
      // frame subscribers (RemoteAgent) keep their unconditional view.
      if (frame.type === "response") this.settleResponse(frame);
      for (const handler of this.handlers) handler(frame);
    });
    this.ws.addEventListener("close", () => {
      this.isOpen = false;
      const err = new Error("WebSocket closed before response");
      for (const pending of this.pendingRequests.values()) {
        clearTimeout(pending.timer);
        pending.reject(err);
      }
      this.pendingRequests.clear();
      if (!this.closedByUser) {
        setTimeout(() => this.connect(), this.reconnectDelayMs);
        this.reconnectDelayMs = Math.min(this.reconnectDelayMs * 2, MAX_RECONNECT_DELAY_MS);
      }
    });
  }

  onFrame(handler: FrameHandler): () => void {
    this.handlers.add(handler);
    return () => this.handlers.delete(handler);
  }

  send(command: ClientCommand): void {
    const serialized = JSON.stringify(command);
    if (this.isOpen) {
      this.ws.send(serialized);
    } else {
      this.sendQueue.push(serialized);
    }
  }

  /**
   * Send a command and resolve with its response `data` when the matching
   * `response` frame (same `id`) arrives. Rejects on `success: false`, on
   * connection close, or after a timeout. The caller-provided command must not
   * already carry an `id`; a unique correlation id is assigned here.
   */
  request<T = unknown>(
    command: Omit<ClientCommand, "id"> & { id?: string },
    timeoutMs = REQUEST_TIMEOUT_MS,
  ): Promise<T> {
    const id = `req-${this.nextRequestId++}`;
    return new Promise<T>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pendingRequests.delete(id);
        reject(new Error(`Request timed out: ${command.type}`));
      }, timeoutMs);
      this.pendingRequests.set(id, {
        resolve: resolve as (data: unknown) => void,
        reject,
        timer,
      });
      this.send({ ...command, id } as ClientCommand);
    });
  }

  private settleResponse(frame: ResponseFrame): void {
    const id = frame.id;
    if (!id) return;
    const pending = this.pendingRequests.get(id);
    if (!pending) return;
    this.pendingRequests.delete(id);
    clearTimeout(pending.timer);
    if (frame.success) {
      pending.resolve(frame.data);
    } else {
      pending.reject(new Error(frame.error ?? `Command failed: ${frame.command}`));
    }
  }

  close(): void {
    this.closedByUser = true;
    this.ws.close();
  }
}
