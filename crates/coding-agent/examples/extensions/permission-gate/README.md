# permission-gate

Tier 1 extension fixture. Blocks `bash` tool calls whose command matches a
small substring blocklist (`rm -rf`, `chmod 777`, `sudo `, ...).

This is a demo, not a sandbox. Real permission checks belong in a UI prompt
or an OS-level sandbox; the fixture only exists to prove the
`before_tool_call` hook can cancel a tool call end-to-end.

## Usage

```rust,ignore
use std::sync::Arc;
use ext_permission_gate::PermissionGate;

let mut session = /* AgentSession */;
session.register_extension(Arc::new(PermissionGate::new()));
```

Ported from `pi-mono/.../examples/extensions/permission-gate.ts`.
