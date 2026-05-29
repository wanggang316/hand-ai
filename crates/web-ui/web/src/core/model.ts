// Local model types. These replace the external model package the reference
// frontend imported and are structurally compatible with the JSON the Rust
// server emits (camelCase fields).

export type Api =
  | "anthropic-messages"
  | "openai-completions"
  | "openai-responses"
  | "google-generative-ai"
  | (string & {});

export type ThinkingLevel = "off" | "minimal" | "low" | "medium" | "high";

export interface ModelCost {
  input: number;
  output: number;
  cacheRead: number;
  cacheWrite: number;
}

export interface Model {
  id: string;
  name: string;
  api: Api;
  provider: string;
  baseUrl?: string;
  /** Drives whether the thinking-level selector is shown. */
  reasoning: boolean;
  input: ("text" | "image")[];
  contextWindow: number;
  maxTokens: number;
  cost?: ModelCost;
}
