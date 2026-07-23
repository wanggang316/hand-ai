# Mock-provider test fixture

Deterministic, API-key-free fixture for TUI streaming tests. It lets `hand`
complete a full streaming turn (and `--resume` replay) while believing it is
talking to a real OpenAI-compatible provider — no network, no credentials.

Three pieces:

1. **Mock HTTP server** — `crates/coding-agent/examples/mock_provider.rs`.
   Serves canned OpenAI Completions-style SSE over plain HTTP.
2. **`models.json`** (this dir) — points the `openai` provider at the server.
3. **Session fixtures** — `../sessions/*.jsonl` for `--resume` scenarios.

## 1. Start the mock server

```sh
# default port 39217
cargo run --example mock_provider -p hand-coding-agent

# custom port
MOCK_PROVIDER_PORT=39411 cargo run --example mock_provider -p hand-coding-agent
```

It prints `mock-provider listening on http://127.0.0.1:<port>` once ready, so
a harness can block on that line instead of sleeping.

Endpoint: `POST {base_url}/chat/completions` (the `openai-rust` client appends
`/chat/completions`). Point `models.json` `baseUrl` at
`http://127.0.0.1:<port>/v1`.

### Scenario selection (first match wins)

1. `?scenario=<name>` query param
2. `X-Mock-Scenario: <name>` header
3. `MOCK_PROVIDER_SCENARIO` env var
4. default: `text`

| scenario       | shape                                                          |
|----------------|----------------------------------------------------------------|
| `text`         | short multi-delta text turn                                    |
| `thinking`     | reasoning deltas then a text answer                            |
| `slow`         | text one word at a time with per-chunk delay (loader lifecycle)|
| `stall`        | first delta, then a long silence (watchdog), then finishes     |
| `tool_call`    | a `read` tool call (args streamed in two fragments)            |
| `edit_tool`    | an `edit` tool call (`oldString`/`newString`)                  |
| `write_tool`   | a `write` tool call (new file)                                 |
| `image_result` | a `read` tool call whose result is an image block              |
| `streamed_fence`| text that opens a code fence mid-stream, closes it at the end |
| `error`        | partial text then `finish_reason: error`                       |

The tool-call scenarios (`tool_call`, `edit_tool`, `write_tool`, `image_result`)
are two-round: the first request emits the tool call; the follow-up request
(which carries the tool result) returns terminal text so the agent loop
terminates instead of re-emitting the same call forever.

Timing knobs: `MOCK_PROVIDER_SLOW_MS` (default 60), `MOCK_PROVIDER_STALL_MS`
(default 3000).

Curl one scenario:

```sh
curl -N -X POST 'http://127.0.0.1:39217/v1/chat/completions?scenario=tool_call' \
  -H 'Authorization: Bearer anything' -d '{}'
```

## 2. Point `hand` at the server

`models.json` here registers `openai:mock-model` with `baseUrl` +
`api: openai-completions` + a literal (fake) `apiKey`. `hand` reads
`models.json` from `$HOME/.hand/agent/models.json` (via `dirs::home_dir`), so
isolate with `HOME`:

```sh
ISO=$(mktemp -d); mkdir -p "$ISO/.hand/agent"
sed 's/39217/39411/' models.json > "$ISO/.hand/agent/models.json"

echo "say hi" | HOME="$ISO" OPENAI_API_KEY=any \
  hand -p --provider openai --model mock-model \
       --base-url http://127.0.0.1:39411/v1 --no-context-files
# -> "Hello from the mock provider."
```

Notes on wiring (from the M3 investigation):

- The `hand` **CLI** builds a *synthetic* model from `--provider/--model` and
  does **not** read the api key out of `models.json` for that path, so the CLI
  smoke needs `OPENAI_API_KEY=<any>` in the env (the mock ignores the value).
  `--base-url` is what redirects the request to localhost.
- `models.json`'s `baseUrl` + literal `apiKey` are consulted by the
  `ModelRegistry` path (what the interactive driver uses); that path resolves
  `mock-model` end-to-end without an env key.

## 3. Session fixtures (`../sessions/`)

`.jsonl` session files (v3 header + entries) for `hand --resume <path>`. The
header `cwd` is `/tmp` so resume passes its cwd-exists check on any machine.

| file                         | covers                                              |
|------------------------------|-----------------------------------------------------|
| `thinking-blocks.jsonl`      | assistant `thinking` + `text` blocks                |
| `error-ended.jsonl`          | an assistant turn ending in `stopReason: error`     |
| `multi-message-resume.jsonl` | multi-turn: `model_change`, tool call, tool result, image-block tool result |

On-disk casing (Rust reader): content-block `type` tags are **lowercase**
(`text`, `thinking`, `toolcall`, `image`); `ImageContent.mime_type` and
`ModelChange.model_id` are **snake_case**; message roles/fields are camelCase
(`toolResult`, `toolCallId`, `isError`, `stopReason`, `errorMessage`).

Replay a fixture (exports the loaded history, proving it parsed):

```sh
hand --resume "$PWD/../sessions/multi-message-resume.jsonl" \
     --export /tmp/out.json </dev/null
```
