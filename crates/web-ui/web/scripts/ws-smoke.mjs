// M0 seam probe: connect to /ws, send a get_state command (no LLM needed),
// and assert a matching response frame comes back through run_rpc_server.
// Uses Node's built-in global WebSocket (Node >= 22). Exits non-zero on
// failure so it can gate CI.

const port = process.argv[2] ?? "4137";
const url = `ws://127.0.0.1:${port}/ws`;

function once(deadlineMs) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(url);
    const timer = setTimeout(() => {
      ws.close();
      reject(new Error("timeout waiting for get_state response"));
    }, deadlineMs);
    ws.addEventListener("open", () => {
      ws.send(JSON.stringify({ type: "get_state", id: "smoke-1" }));
    });
    ws.addEventListener("message", (ev) => {
      let frame;
      try {
        frame = JSON.parse(ev.data);
      } catch {
        return;
      }
      if (frame.type === "response" && frame.command === "get_state") {
        clearTimeout(timer);
        ws.close();
        resolve(frame);
      }
    });
    ws.addEventListener("error", (e) => {
      clearTimeout(timer);
      reject(new Error("ws error: " + (e?.message ?? "unknown")));
    });
  });
}

// Retry connecting for a few seconds while the server comes up.
async function main() {
  const start = Date.now();
  let lastErr;
  while (Date.now() - start < 8000) {
    try {
      const frame = await once(3000);
      if (frame.success && typeof frame.data?.sessionId === "string") {
        console.log("SMOKE_OK", JSON.stringify({ sessionId: frame.data.sessionId, isStreaming: frame.data.isStreaming }));
        process.exit(0);
      }
      throw new Error("unexpected frame: " + JSON.stringify(frame));
    } catch (e) {
      lastErr = e;
      await new Promise((r) => setTimeout(r, 400));
    }
  }
  console.error("SMOKE_FAIL", lastErr?.message ?? "unknown");
  process.exit(1);
}

main();
