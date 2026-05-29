// Side-effect registration of the built-in tool renderers and wiring of the
// default fallback renderer. Importing this module registers `bash`, `calculate`,
// and `get_current_time`; any other tool falls through to the DefaultRenderer.
// The browser-only renderers (javascript_repl / extract_document / artifacts)
// register themselves in later milestones.

import { BashRenderer } from "./bash";
import { CalculateRenderer } from "./calculate";
import { DefaultRenderer } from "./default";
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
