// Side-effect registration of the built-in tool renderers and wiring of the
// default fallback renderer. Importing this module registers `bash`, `calculate`,
// and `get_current_time`; importing the browser-only tool modules registers the
// `javascript_repl` and `extract_document` renderers (their `registerToolRenderer`
// calls run as import side effects). Any other tool falls through to the
// DefaultRenderer; the `artifacts` renderer is registered by the ChatPanel.

import { BashRenderer } from "./bash";
import { CalculateRenderer } from "./calculate";
import { DefaultRenderer } from "./default";
// Side-effect imports: register the browser-only tool renderers.
import "./extract-document";
import "./javascript-repl";
import { GetCurrentTimeRenderer } from "./get-current-time";
import { registerToolRenderer, setDefaultToolRenderer } from "./renderer-registry";

setDefaultToolRenderer(new DefaultRenderer());

registerToolRenderer("bash", new BashRenderer());
registerToolRenderer("calculate", new CalculateRenderer());
registerToolRenderer("get_current_time", new GetCurrentTimeRenderer());

export {
  getToolRenderer,
  registerToolRenderer,
  renderTool,
  setShowJsonMode,
  toolRenderers,
  renderHeader,
  renderCollapsibleHeader,
} from "./renderer-registry";
export type { ToolRenderer, ToolRenderResult, ToolRenderState } from "./renderer-registry";
