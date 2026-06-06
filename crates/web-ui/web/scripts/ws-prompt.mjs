// Live streaming probe: send one prompt and collect the streamed assistant
// text via message_update / message_end agent-event frames. Requires the
// server to have a working provider key in its environment. Exits non-zero
// if no assistant text streamed back within the timeout.

const port = process.argv[2] ?? "4137";
const prompt = process.argv[3] ?? "Say hello in exactly one short sentence.";
const url = `ws://127.0.0.1:${port}/ws`;
const ws = new WebSocket(url);

let text = "";
let sawUpdate = false;
const deadline = setTimeout(() => finish(false, "timeout"), 60000);

function finish(ok, note) {
  clearTimeout(deadline);
  try { ws.close(); } catch {}
  if (ok) {
    console.log("PROMPT_OK", JSON.stringify({ text }));
    process.exit(0);
  } else {
    console.error("PROMPT_FAIL", note, JSON.stringify({ text }));
    process.exit(1);
  }
}

ws.addEventListener("open", () => {
  ws.send(JSON.stringify({ type: "prompt", id: "p1", message: prompt }));
});

ws.addEventListener("message", (ev) => {
  let frame;
  try { frame = JSON.parse(ev.data); } catch { return; }
  if (frame.type === "event" && frame.event?.kind === "agent") {
    const e = frame.event;
    // Streaming assistant content arrives as a block array via message_update;
    // turn_end carries the finalized assistant message. User-message echoes
    // (string content) are ignored.
    const m = e.message;
    if (m && Array.isArray(m.content) && m.role === "assistant") {
      text = m.content.filter((b) => b.type === "text").map((b) => b.text).join("");
      if (e.type === "message_update") sawUpdate = true;
    }
  }
  if (frame.type === "response" && frame.command === "prompt") {
    if (!frame.success) return finish(false, frame.error ?? "prompt failed");
    finish(text.trim().length > 0, sawUpdate ? "streamed" : "no streaming deltas seen");
  }
});

ws.addEventListener("error", (e) => finish(false, "ws error: " + (e?.message ?? "unknown")));
