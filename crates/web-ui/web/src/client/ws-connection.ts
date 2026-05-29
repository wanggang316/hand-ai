// WebSocket lifecycle: framing, send buffering before open, and frame fan-out.
// One JSON object per text frame, matching the server's line protocol.

import type { ClientCommand, ServerFrame } from "./wire";

export type FrameHandler = (frame: ServerFrame) => void;

export class WsConnection {
  private ws: WebSocket;
  private handlers = new Set<FrameHandler>();
  private sendQueue: string[] = [];
  private isOpen = false;

  constructor(url: string) {
    this.ws = new WebSocket(url);
    this.ws.addEventListener("open", () => {
      this.isOpen = true;
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
      for (const handler of this.handlers) handler(frame);
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

  close(): void {
    this.ws.close();
  }
}
