# User-Cases: rpc (jsonl + server + types + clone semantics)

**Upstream sources:**
- `pi-mono/packages/coding-agent/test/rpc.test.ts` (14 cases)
- `pi-mono/packages/coding-agent/test/rpc-jsonl.test.ts` (4 cases)
- `pi-mono/packages/coding-agent/test/rpc-prompt-response-semantics.test.ts` (3 cases)
- `pi-mono/packages/coding-agent/test/rpc-client-clone.test.ts` (1 case)

**hand-ai source:**   `crates/coding-agent/src/rpc/` + `crates/coding-agent/tests/rpc_smoke.rs`

## Surface

Hand's RPC subsystem (4 files):
- **`jsonl.rs`** (10 tests) — line-delimited JSON framing
- **`server.rs`** (31 tests) — request/response/notification routing
- **`types.rs`** (14 tests) — protocol message shapes
- **`mod.rs`** — wiring

55 unit tests + integration smoke test in `tests/rpc_smoke.rs`.

## Status (summary mapping)

| Pi file (cases) | hand coverage |
|---|---|
| rpc (14) | `server.rs::tests` covers route registration, request handling, error propagation |
| rpc-jsonl (4) | `jsonl.rs::tests` covers framing/parsing |
| rpc-prompt-response-semantics (3) | `server.rs` prompt-response cases |
| rpc-client-clone (1) | `types.rs` clone-trait derivation |

| ID | Status | Reason |
|----|--------|--------|
| UC-rpc-001..022 | ✅ collectively pinned | Hand's 55 RPC tests cover the surface. |

## Notes

The RPC protocol is one of the modules where hand has *more* tests than pi (55 vs 22). Hand chose to invest deeply here because the RPC surface is a stable host-integration contract.
