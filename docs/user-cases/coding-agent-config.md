# User-Cases: src/config — install detection + self-update

**Upstream source:** `pi-mono/packages/coding-agent/test/config.test.ts` (9 cases)
**hand-ai source:**   N/A (no equivalent module)

## Status

All 9 cases are 🚫 N/A: pi's `detectInstallMethod` / `getSelfUpdateCommand` / `getUpdateInstruction` infer the install method by examining `process.execPath` against npm / pnpm / bun global install path conventions. hand-ai is distributed via **cargo/crates.io** rather than a JavaScript package manager — the entire install-detection surface (npm `--prefix`, bun `pm bin -g`, pnpm `.pnpm/` path patterns, write-permission probing) does not apply. Self-update for hand is `cargo install hand-coding-agent`; install method is always "cargo" because nothing else can deliver it.

| ID | Status | Reason |
|----|--------|--------|
| UC-cfg-001..009 | 🚫 N/A | Distribution-channel mismatch. pi infers npm/pnpm/bun install paths; hand uses cargo with a single canonical install command. The entire surface is intentionally not ported. |

## Notes

When hand ever ships through a JS-runtime wrapper (npm package wrapping the cargo binary), these cases re-open. Until then they're filed as architectural-divergence N/A.
