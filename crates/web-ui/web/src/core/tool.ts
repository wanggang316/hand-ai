// Local tool types for browser-executed tools (the JS REPL, document
// extraction, and artifacts tool land in later milestones). Server-side
// tools never surface here — they execute on the server and arrive only as
// tool-execution events for rendering.

export interface ToolResult {
  content: string;
  details?: unknown;
  isError: boolean;
}

export interface AgentTool {
  name: string;
  description: string;
  /** JSON Schema for the tool parameters. */
  parameters: unknown;
  execute(args: unknown): Promise<ToolResult>;
}
