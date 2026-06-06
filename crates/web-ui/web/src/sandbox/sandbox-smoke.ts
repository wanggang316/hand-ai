// Tiny test helper to verify the sandbox execute() path from the browser
// console. Creates a transient <sandbox-iframe>, runs a one-line program, and
// returns the SandboxResult. Invoke it from the devtools console (after this
// module is imported somewhere reachable):
//
//   import("./sandbox/sandbox-smoke").then(m => m.runSandboxSmoke()).then(console.log)
//
// Expected: consoleLogs contains "x" and returnValue === 2.

// Side-effect import: registers the <sandbox-iframe> custom element. The named
// imports below are type-only and would otherwise be elided, skipping
// registration.
import "./sandboxed-iframe";
import type { SandboxIframe, SandboxResult } from "./sandboxed-iframe";

export async function runSandboxSmoke(): Promise<SandboxResult> {
  const el = document.createElement("sandbox-iframe") as SandboxIframe;
  document.body.appendChild(el);
  try {
    return await el.execute(
      `smoke-${Date.now()}`,
      "console.log('x'); return 1+1;",
    );
  } finally {
    el.remove();
  }
}
