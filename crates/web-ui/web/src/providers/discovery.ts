// Local-server model auto-discovery. These are direct browser-to-localhost
// fetch calls — no heavy provider SDKs (`@lmstudio/sdk` / `ollama`) are pulled
// in; everything goes through the browser `fetch` API so the discovery surface
// stays dependency-free and CORS-transparent for same-machine servers.
//
// All four discovered-model shapes are normalized into the local `Model` type
// (`src/core/model.ts`): completions API, `provider: ""` (the caller fills it
// in from the custom-provider name), zero cost, and a `baseUrl` pointing at the
// server's OpenAI-compatible `/v1` mount.

import type { AutoDiscoveryProviderType } from "../storage/backend";
import type { Model } from "../core/model";

/** Default base URL prefilled in the dialog per provider type. */
export const DEFAULT_BASE_URLS: Record<AutoDiscoveryProviderType, string> = {
  ollama: "http://localhost:11434",
  "llama.cpp": "http://localhost:8080",
  vllm: "http://localhost:8000",
  lmstudio: "http://localhost:1234",
};

/** Strip a trailing slash so we can append paths without doubling up. */
function trimTrailingSlash(url: string): string {
  return url.replace(/\/+$/, "");
}

function authHeaders(apiKey?: string): HeadersInit {
  const headers: Record<string, string> = { "Content-Type": "application/json" };
  if (apiKey) headers.Authorization = `Bearer ${apiKey}`;
  return headers;
}

/** Build a normalized completions-API Model from an id + sizing/capabilities. */
function makeModel(
  id: string,
  baseUrl: string,
  opts: {
    name?: string;
    reasoning?: boolean;
    vision?: boolean;
    contextWindow?: number;
    maxTokens?: number;
  } = {},
): Model {
  return {
    id,
    name: opts.name ?? id,
    api: "openai-completions",
    provider: "", // filled in by the caller from the custom-provider name
    baseUrl: `${trimTrailingSlash(baseUrl)}/v1`,
    reasoning: opts.reasoning ?? false,
    input: opts.vision ? ["text", "image"] : ["text"],
    contextWindow: opts.contextWindow ?? 8192,
    maxTokens: opts.maxTokens ?? 4096,
    cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
  };
}

/**
 * Discover models from an Ollama server. Uses `/api/tags` to list models, then
 * `/api/show` per model to read its capabilities (filtering out models that do
 * not advertise `tools`) and architecture-specific `context_length`.
 */
export async function discoverOllamaModels(baseUrl: string, _apiKey?: string): Promise<Model[]> {
  const base = trimTrailingSlash(baseUrl);
  const tagsRes = await fetch(`${base}/api/tags`, { method: "GET" });
  if (!tagsRes.ok) {
    throw new Error(`Ollama discovery failed: HTTP ${tagsRes.status} ${tagsRes.statusText}`);
  }
  const tags = (await tagsRes.json()) as { models?: { name?: string; model?: string }[] };
  const names = (tags.models ?? [])
    .map((m) => m.name ?? m.model)
    .filter((n): n is string => typeof n === "string");

  const results = await Promise.all(
    names.map(async (name): Promise<Model | null> => {
      try {
        const showRes = await fetch(`${base}/api/show`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ model: name }),
        });
        if (!showRes.ok) return null;
        const details = (await showRes.json()) as {
          capabilities?: string[];
          model_info?: Record<string, unknown>;
        };

        const capabilities = details.capabilities ?? [];
        // Only surface tool-capable models; the agent loop needs tool support.
        if (!capabilities.includes("tools")) return null;

        const modelInfo = details.model_info ?? {};
        const architecture = String(modelInfo["general.architecture"] ?? "");
        const contextKey = `${architecture}.context_length`;
        const contextWindow = Number.parseInt(String(modelInfo[contextKey] ?? "8192"), 10) || 8192;

        return makeModel(name, base, {
          reasoning: capabilities.includes("thinking"),
          contextWindow,
          // Ollama caps output at 10x context length.
          maxTokens: contextWindow * 10,
        });
      } catch {
        return null;
      }
    }),
  );

  return results.filter((m): m is Model => m !== null);
}

/** Read an OpenAI-compatible `/v1/models` list into a raw entry array. */
async function fetchOpenAiModels(baseUrl: string, apiKey: string | undefined, label: string): Promise<any[]> {
  const base = trimTrailingSlash(baseUrl);
  const res = await fetch(`${base}/v1/models`, { method: "GET", headers: authHeaders(apiKey) });
  if (!res.ok) {
    throw new Error(`${label} discovery failed: HTTP ${res.status} ${res.statusText}`);
  }
  const data = (await res.json()) as { data?: unknown };
  if (!Array.isArray(data.data)) {
    throw new Error(`${label} discovery failed: invalid /v1/models response`);
  }
  return data.data as any[];
}

/**
 * Discover models from a llama.cpp server via the OpenAI-compatible
 * `/v1/models` endpoint. llama.cpp rarely reports sizing, so fall back to
 * conservative defaults.
 */
export async function discoverLlamaCppModels(baseUrl: string, apiKey?: string): Promise<Model[]> {
  const entries = await fetchOpenAiModels(baseUrl, apiKey, "llama.cpp");
  return entries.map((model) =>
    makeModel(String(model.id), baseUrl, {
      contextWindow: Number(model.context_length) || 8192,
      maxTokens: Number(model.max_tokens) || 4096,
    }),
  );
}

/**
 * Discover models from a vLLM server via the OpenAI-compatible `/v1/models`
 * endpoint. vLLM reports `max_model_len` as the context window.
 */
export async function discoverVLLMModels(baseUrl: string, apiKey?: string): Promise<Model[]> {
  const entries = await fetchOpenAiModels(baseUrl, apiKey, "vLLM");
  return entries.map((model) => {
    const contextWindow = Number(model.max_model_len) || 8192;
    return makeModel(String(model.id), baseUrl, {
      contextWindow,
      maxTokens: Math.min(contextWindow, 4096),
    });
  });
}

/**
 * Discover models from an LM Studio server. Prefers LM Studio's REST API
 * (`/api/v0/models`), which exposes capability hints (`type`, `vision`,
 * context length); falls back to the OpenAI-compatible `/v1/models` list when
 * the REST API is unavailable. Uses plain `fetch` — no `@lmstudio/sdk`.
 */
export async function discoverLMStudioModels(baseUrl: string, apiKey?: string): Promise<Model[]> {
  const base = trimTrailingSlash(baseUrl);
  try {
    const res = await fetch(`${base}/api/v0/models`, { method: "GET", headers: authHeaders(apiKey) });
    if (res.ok) {
      const data = (await res.json()) as { data?: any[] };
      const entries = Array.isArray(data.data) ? data.data : [];
      const llms = entries.filter((m) => m.type === "llm" || m.type === undefined);
      return llms.map((model) => {
        const contextWindow = Number(model.max_context_length ?? model.loaded_context_length) || 8192;
        return makeModel(String(model.id), base, {
          name: model.id,
          reasoning: Boolean(model.trained_for_tool_use),
          vision: model.vision === true,
          contextWindow,
          maxTokens: contextWindow,
        });
      });
    }
  } catch {
    // Fall through to the OpenAI-compatible endpoint below.
  }

  const entries = await fetchOpenAiModels(base, apiKey, "LM Studio");
  return entries.map((model) => makeModel(String(model.id), base));
}

/** Dispatch discovery by provider type. */
export async function discoverModels(
  type: AutoDiscoveryProviderType,
  baseUrl: string,
  apiKey?: string,
): Promise<Model[]> {
  switch (type) {
    case "ollama":
      return discoverOllamaModels(baseUrl, apiKey);
    case "llama.cpp":
      return discoverLlamaCppModels(baseUrl, apiKey);
    case "vllm":
      return discoverVLLMModels(baseUrl, apiKey);
    case "lmstudio":
      return discoverLMStudioModels(baseUrl, apiKey);
  }
}
