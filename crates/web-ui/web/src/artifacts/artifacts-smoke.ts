// Tiny browser-console smoke helper for the artifacts subsystem. The controller
// can call `await runArtifactsSmoke()` from the browser console to verify that an
// HTML artifact renders in the sandbox and its console output is captured.
//
// It side-effect-imports the panel, creates an `<artifacts-panel>`, drives the
// create path to add an `index.html` artifact that logs to the console, waits
// briefly for sandbox execution, and reports the resulting tab names and
// captured console logs.

import "./index";
import { ArtifactsPanel } from "./artifacts-panel";
import { HtmlArtifact } from "./html-artifact";

export interface ArtifactsSmokeResult {
  count: number;
  tabNames: string[];
  consoleLogs: string[];
}

export async function runArtifactsSmoke(): Promise<ArtifactsSmokeResult> {
  const panel = document.createElement("artifacts-panel") as ArtifactsPanel;
  // Mount on-screen (a corner overlay): off-screen iframes can have their
  // script execution deferred by the browser, which stalls the HTML artifact's
  // console-capture wait. A visible, small overlay loads and executes reliably.
  panel.style.position = "fixed";
  panel.style.right = "0";
  panel.style.bottom = "0";
  panel.style.width = "320px";
  panel.style.height = "240px";
  panel.style.zIndex = "2147483647";
  document.body.appendChild(panel);

  // Let the panel connect and create its content container. Use a macrotask
  // rather than requestAnimationFrame so this stays reliable in a backgrounded
  // or headless tab (where rAF callbacks are throttled or never fire).
  await panel.updateComplete?.catch?.(() => {});
  await new Promise<void>((resolve) => setTimeout(resolve, 0));

  const filename = "index.html";
  const content = `<h1>hi-artifact</h1><script>console.log('art-log')</script>`;

  // Drive the create path through the public tool execute.
  await panel.tool.execute("smoke", { command: "create", filename, content });

  // Wait for the sandbox iframe to load and forward console output.
  await new Promise<void>((resolve) => setTimeout(resolve, 1600));

  const tabNames = Array.from(panel.artifacts.keys());

  // Pull the captured logs back out of the HTML artifact element.
  let consoleLogs: string[] = [];
  const htmlEl = panel.querySelector("html-artifact");
  if (htmlEl instanceof HtmlArtifact) {
    const logsText = htmlEl.getLogs();
    consoleLogs = logsText ? logsText.split("\n") : [];
  }

  return {
    count: panel.artifacts.size,
    tabNames,
    consoleLogs,
  };
}

// Expose on window for easy invocation from the browser console.
(window as unknown as { runArtifactsSmoke?: typeof runArtifactsSmoke }).runArtifactsSmoke =
  runArtifactsSmoke;
