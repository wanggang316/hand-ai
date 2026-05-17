# User-Cases: modes/interactive/components/assistant_message — OSC 133 zones

**Upstream source:** `pi-mono/packages/coding-agent/test/assistant-message.test.ts` (2 cases)
**hand-ai source:**   `crates/coding-agent/src/modes/interactive/components/assistant_message.rs`
**Surface:**          OSC 133 zone markers (`\x1b]133;A\x07` / `B` / `C`) bracket assistant text so a terminal that recognises Final-Term semantic prompts can detect output boundaries. Markers are emitted ONLY when the assistant message has no tool calls — a tool-call response should not be flagged as a complete reply.

## Status

| ID | Status | Verified-by |
|----|--------|-------------|
| UC-am-001 | ✅ pass | `assistant_message.rs:286` test asserts `OSC133_ZONE_START` on the first line and `OSC133_ZONE_END` / `OSC133_ZONE_FINAL` on the last line for a plain text-only message |
| UC-am-002 | ✅ pass | `assistant_message.rs:347` test asserts no OSC 133 markers appear when the message contains tool calls |

## Notes

The 3 OSC 133 byte constants in hand match pi byte-for-byte (`OSC133_ZONE_START = "\x1b]133;A\x07"`, etc.). The first / last line splice is in `assistant_message.rs:215-216`.

- Probe: `cargo test -p hand-coding-agent --lib modes::interactive::components::assistant_message -- --exact`.
