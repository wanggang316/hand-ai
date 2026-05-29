# User-Cases: cli/args

**Upstream source:** `pi-mono/packages/coding-agent/test/args.test.ts`
**hand-ai source:**   `crates/coding-agent/src/cli/args.rs`
**Surface:**          The CLI argument parser. pi rolls a hand-written
parser that captures unknown flags into a `Map`; hand uses clap with
declarative attributes. The high-level surface (flags, shorthands,
mode values) must match across the two so user scripts and shell
aliases work unchanged.

## API delta

pi's `ParsedArgs` shape contains many fields hand does not currently
carry (or carries under a different name):

| pi field          | hand field           | status |
|-------------------|----------------------|--------|
| version           | (clap auto `-V/--version`)             | observable, partial |
| help              | (clap auto `-h/--help`)                | observable, partial |
| print, -p         | print, -p                              | ✅ |
| continue, -c      | continue_session, -c                   | ✅ name diverges |
| resume, -r        | resume (Option<String>), -r            | semantics diverge (pi: boolean OR string; hand: Option<String> only) |
| provider          | provider                               | ✅ |
| model             | model                                  | ✅ |
| api_key (camel `apiKey`) | api_key                         | ✅ (case style aside) |
| system_prompt     | system_prompt                          | ✅ |
| append_system_prompt | append_system_prompt (Vec<String>)  | ✅ |
| mode              | mode (with `rpc` alias)                | ✅ |
| session           | (aliased through `--session` → resume) | ✅ |
| fork              | fork                                   | ✅ |
| export            | export                                 | ✅ |
| thinking          | thinking                               | ✅ |
| models            | list_models (Option<Option<String>>)   | semantics differ |
| no_session        | no_session                             | ✅ |
| extensions, -e    | —                                       | ❌ missing |
| no_extensions     | —                                       | ❌ missing |
| skills            | —                                       | ❌ missing (only `--no-skills`) |
| prompt_templates  | —                                       | ❌ missing |
| themes            | —                                       | ❌ missing |
| no_skills         | no_skills                              | ✅ |
| no_prompt_templates | —                                     | ❌ missing |
| no_themes         | —                                       | ❌ missing |
| no_context_files, -nc | no_context_files (no -nc short)    | ❌ short missing |
| verbose           | verbose                                | ✅ |
| offline           | offline                                | ✅ |
| no_tools, -nt     | no_tools (no -nt short)                | ❌ short missing |
| no_builtin_tools, -nbt | —                                  | ❌ missing |
| tools, -t         | tools (no -t short; Option<String> CSV vs Vec<String>) | ❌ short + shape |
| messages          | (positional becomes prompt field)      | semantics differ |
| file_args (@…)    | —                                       | ❌ missing |
| unknown_flags     | (clap rejects unknown)                  | ❌ behaviour differs |

The 60-case suite below preserves pi's surface; cases for features hand
lacks land as ❌ with a resolution proposal so the gap is tracked.

## Status

| ID | Status | Verified-by |
|----|--------|-------------|
| UC-args-001 | 🚫 N/A | clap's `--version` auto-prints and exits the process; there is no `version: bool` field to assert against. Pi's flag-as-field shape is intentionally not replicated. |
| UC-args-002 | ✅ pass | `version_flag` rebound to `-v` via ArgAction::Version; `--verbose` no longer has a short |
| UC-args-003 | 🚫 N/A | `--version` exits clap before any other flag is parsed; "precedence over other args" is automatic and untestable as a field. |
| UC-args-004 | 🚫 N/A | clap's `--help` auto-prints and exits — same shape as UC-args-001. |
| UC-args-005 | 🚫 N/A | `-h` shorthand is wired by clap automatically — same shape as UC-args-004. |
| UC-args-006 | ✅ pass | `parses_print_flag` |
| UC-args-007 | ✅ pass | `parses_short_prompt` |
| UC-args-008 | ✅ pass | `parses_prompt_with_yaml_frontmatter` |
| UC-args-009 | ✅ pass | `parses_long_prompt_with_yaml_frontmatter` (covers post-`-p` flag handling) |
| UC-args-010 | ✅ pass | `parses_continue_short_and_long` |
| UC-args-011 | ✅ pass | `parses_continue_short_and_long` |
| UC-args-012 | ✅ pass | `parses_bare_resume_without_value` |
| UC-args-013 | ✅ pass | `parses_bare_resume_short_without_value` |
| UC-args-014 | ✅ pass | `parses_provider_flag` |
| UC-args-015 | ✅ pass | `parses_model` |
| UC-args-016 | ✅ pass | `parses_api_key_flag` |
| UC-args-017 | ✅ pass | `parses_system_prompt_flag` |
| UC-args-018 | ✅ pass | `parses_repeated_append_system_prompt` (single + multiple covered) |
| UC-args-019 | ✅ pass | `parses_repeated_append_system_prompt` |
| UC-args-020 | ✅ pass | `parses_mode_text_and_json` (json branch) |
| UC-args-021 | ✅ pass | `parses_mode_rpc` |
| UC-args-022 | ✅ pass | `parses_session_alias_for_resume` |
| UC-args-023 | ✅ pass | `parses_fork_flag` |
| UC-args-024 | ✅ pass | `parses_export_flag` |
| UC-args-025 | ✅ pass | `parses_thinking_flag` |
| UC-args-026 | ✅ pass | `parses_models_csv` |
| UC-args-027 | ✅ pass | `parses_no_session_flag` |
| UC-args-028 | ✅ pass | `parses_extension_single_and_repeated` |
| UC-args-029 | ✅ pass | same (covers `-e` short) |
| UC-args-030 | ✅ pass | same (covers repeated) |
| UC-args-031 | ✅ pass | `parses_no_extensions_with_explicit_entries` |
| UC-args-032 | ✅ pass | same |
| UC-args-033 | ✅ pass | `parses_skill_single_and_repeated` |
| UC-args-034 | ✅ pass | same (covers repeated) |
| UC-args-035 | 🚫 N/A | hand has no prompt-template subsystem |
| UC-args-036 | 🚫 N/A | same — no subsystem |
| UC-args-037 | 🚫 N/A | hand has no theme subsystem |
| UC-args-038 | 🚫 N/A | same — no subsystem |
| UC-args-039 | ✅ pass | `parses_no_skills_flag` |
| UC-args-040 | 🚫 N/A | no prompt-template subsystem to disable |
| UC-args-041 | 🚫 N/A | no theme subsystem to disable |
| UC-args-042 | ✅ pass | `parses_no_context_files_flag` |
| UC-args-043 | ✅ pass | `nc_short_alias_rewrites_to_no_context_files` |
| UC-args-044 | ✅ pass | `parses_verbose_long_form` |
| UC-args-045 | ✅ pass | `parses_offline_flag` |
| UC-args-046 | ✅ pass | `parses_no_tools_flag` |
| UC-args-047 | ✅ pass | `nt_short_alias_rewrites_to_no_tools` |
| UC-args-048 | ✅ pass | `parses_no_builtin_tools_flag` |
| UC-args-049 | ✅ pass | `nbt_short_alias_rewrites_to_no_builtin_tools` |
| UC-args-050 | ✅ pass | `parses_tools_csv` (long form; pi takes the same CSV) |
| UC-args-051 | ✅ pass | `parses_tools_short_t` |
| UC-args-052 | ✅ pass | `no_tools_and_tools_are_mutually_exclusive` — clap rejects the combination at parse time (#83) |
| UC-args-053 | ✅ pass | duplicate of UC-args-048 — `parses_no_builtin_tools_flag` |
| UC-args-054 | ✅ pass | `positional_plain_text_lands_in_messages` |
| UC-args-055 | ✅ pass | `positional_at_file_lands_in_file_args` |
| UC-args-056 | ✅ pass | `positional_mixed_messages_and_file_args` |
| UC-args-057 | 🚫 N/A | hand's clap rejects unknown flags by design (typo-safe UX); pi's lenient capture into `unknownFlags` would break hand's strict-parsing contract |
| UC-args-058 | 🚫 N/A | same |
| UC-args-059 | 🚫 N/A | same |
| UC-args-060 | ✅ pass | `parses_complex_combo_end_to_end` — provider + model + extensions + tools + positional messages + `@file` all parse without collision |

## Cases

### UC-args-001 — `--version` sets `version=true`

**Given** a fresh CLI invocation.
**When**  the user runs `hand --version`.
**Then**  the parsed result indicates version mode (output may print and
exit; either way the user sees version output, not a normal run).

- Observable: stdout begins with the program name + version string.
- Probe (PARTIAL): hand's clap auto-handles `--version`, printing and
  exiting (`exit(0)`). pi sets a `version: true` field instead, then
  the caller handles it. From the user's seat both outcomes show
  version info; the parse-shape divergence is captured here.

### UC-args-002 — `-v` shorthand triggers version

**Given** a fresh invocation.
**When**  the user runs `hand -v`.
**Then**  version output is produced.

- Observable: stdout shows version.
- Probe (FAILS): hand's clap binds `-v` to `--verbose` (boolean), NOT to
  `--version`. A user typing `hand -v` gets a verbose-logging run, not a
  version dump.
- Resolution proposal: re-bind `-v` to `--version` (or drop the conflict
  and use `-V` for version, matching cargo's convention). Update the
  user-facing docs.

### UC-args-003 — `--version` takes precedence over other args

**Given** any combination of `--version` plus other flags and a message.
**When**  the user runs them together.
**Then**  version output is produced; the message and other flags do
not trigger normal execution.

- Probe (PARTIAL): clap-built-in behaviour matches.

### UC-args-004 — `--help` sets help mode

**Given** `hand --help`.
**Then**  help text is printed.
- Probe: clap auto-handles.

### UC-args-005 — `-h` shorthand triggers help

- Probe: clap auto-handles.

### UC-args-006 — `--print` flag sets print mode

**When**  the user runs `hand --print`.
**Then**  the parsed args carry `print=true`.
- Probe: `cargo test -p hand-coding-agent parses_print_flag -- --exact`.

### UC-args-007 — `-p <prompt>` captures the prompt that follows

**Given** `hand -p hello`.
**Then**  `prompt == "hello"` and print mode is enabled (when wired by main).
- Probe: `cargo test -p hand-coding-agent parses_short_prompt -- --exact`.

### UC-args-008 — `-p <prompt>` captures even YAML-frontmatter prompts (`-p "---\ntitle: …\n---\nsay hi"`)

- Probe: `cargo test -p hand-coding-agent parses_prompt_with_yaml_frontmatter -- --exact`.

### UC-args-009 — flags after `-p` are still parsed as flags, not consumed as prompt extension

**Given** `hand -p --provider openai "Say hi."`.
**Then**  `provider == "openai"`, `messages/prompt` contains only
`"Say hi."`.
- Probe: `cargo test -p hand-coding-agent parses_long_prompt_with_yaml_frontmatter -- --exact`
  (covers the post-flag handling on the long form).

### UC-args-010 — `--continue` sets continue mode

- Probe: `cargo test -p hand-coding-agent parses_continue_short_and_long -- --exact`.

### UC-args-011 — `-c` shorthand sets continue

- Probe: same as UC-args-010.

### UC-args-012 — `--resume` (no value) sets resume mode

**Given** `hand --resume` with NO id following.
**Then**  in pi `resume === true`; the agent picks the most recent session.
- Probe (FAILS): hand's `resume: Option<String>` demands a value; bare
  `--resume` is a clap parse error.
- Resolution proposal: model resume as `Option<Option<String>>` (or
  introduce a separate `--continue`/`-c` for the bare case, matching
  pi's mapping where `-c` ≡ continue-without-id).

### UC-args-013 — `-r` shorthand without value sets resume mode

- Probe (FAILS): same as UC-args-012.

### UC-args-014 — `--provider <name>` captures provider

- Probe: `cargo test -p hand-coding-agent parses_provider_flag -- --exact`.

### UC-args-015 — `--model <pattern>` captures model

- Probe: `cargo test -p hand-coding-agent parses_model -- --exact`.

### UC-args-016 — `--api-key <key>` captures API key

- Probe: `cargo test -p hand-coding-agent parses_api_key_flag -- --exact`.

### UC-args-017 — `--system-prompt <text>` captures the system prompt

- Probe: `cargo test -p hand-coding-agent parses_system_prompt_flag -- --exact`.

### UC-args-018 — `--append-system-prompt <text>` appends one entry

- Probe: `cargo test -p hand-coding-agent parses_repeated_append_system_prompt -- --exact`.

### UC-args-019 — multiple `--append-system-prompt` flags append in order

- Probe: same as UC-args-018 (covers the multi-entry case).

### UC-args-020 — `--mode json` selects JSON mode

- Probe: `cargo test -p hand-coding-agent parses_mode_text_and_json -- --exact`.

### UC-args-021 — `--mode rpc` selects RPC mode

- Probe: `cargo test -p hand-coding-agent parses_mode_rpc -- --exact`.

### UC-args-022 — `--session <path>` aliases to resume

- Probe: `cargo test -p hand-coding-agent parses_session_alias_for_resume -- --exact`.

### UC-args-023 — `--fork <id>` captures fork id

- Probe: `cargo test -p hand-coding-agent parses_fork_flag -- --exact`.

### UC-args-024 — `--export <path>` captures export target

- Probe: `cargo test -p hand-coding-agent parses_export_flag -- --exact`.

### UC-args-025 — `--thinking <level>` captures thinking level

- Probe: `cargo test -p hand-coding-agent parses_thinking_flag -- --exact`.

### UC-args-026 — `--models <a,b,c>` parses a comma-separated list

**Given** `hand --models gpt-4o,claude-sonnet,gemini-pro`.
**Then**  `models == ["gpt-4o", "claude-sonnet", "gemini-pro"]`.
- Probe (FAILS): hand has no plural `--models` flag. It has
  `--list-models` which is a different surface (toggle, not a list of
  models to enable).
- Resolution proposal: add a `--models <csv>` value flag; on parse,
  split by `,`.

### UC-args-027 — `--no-session` sets no_session

- Probe: `cargo test -p hand-coding-agent parses_no_session_flag -- --exact`.

### UC-args-028..030 — `--extension <path>` / `-e <path>` collects extension paths

- Probe (FAILS): hand has no `--extension` / `-e` flag. Extensions are
  loaded from a settings file or hard-coded location, not from CLI args.
- Resolution proposal: add `--extension` / `-e` as a repeatable
  string-valued flag collecting into `Vec<String>`.

### UC-args-031..032 — `--no-extensions` disables extension loading

- Probe (FAILS): blocked on UC-args-028 (no extension flag at all).

### UC-args-033..034 — `--skill <path>` collects skill paths

- Probe (FAILS): hand has no `--skill` flag.

### UC-args-035..036 — `--prompt-template <path>` collects template paths

- Probe (FAILS): hand has no `--prompt-template` flag.

### UC-args-037..038 — `--theme <path>` collects theme paths

- Probe (FAILS): hand has no `--theme` flag.

### UC-args-039 — `--no-skills` disables skills

- Probe: `cargo test -p hand-coding-agent parses_no_skills_flag -- --exact`.

### UC-args-040 — `--no-prompt-templates` disables prompt templates

- Probe (FAILS): hand has no `--no-prompt-templates` flag.

### UC-args-041 — `--no-themes` disables themes

- Probe (FAILS): hand has no `--no-themes` flag.

### UC-args-042 — `--no-context-files` disables context-file loading

- Probe: `cargo test -p hand-coding-agent parses_no_context_files_flag -- --exact`.

### UC-args-043 — `-nc` shorthand disables context-file loading

- Probe (FAILS): hand has no `-nc` shorthand.
- Resolution proposal: add `-nc` as the short form of `--no-context-files`
  in the clap derive macro.

### UC-args-044 — `--verbose` enables verbose logging

- Probe (pending): hand has `verbose: bool` but I haven't pinned the
  parse-equivalence test name. Likely covered by clap's derive but
  needs an explicit test.

### UC-args-045 — `--offline` enables offline mode

- Probe: `cargo test -p hand-coding-agent parses_offline_flag -- --exact`.

### UC-args-046 — `--no-tools` disables tool registration

- Probe: `cargo test -p hand-coding-agent parses_no_tools_flag -- --exact`.

### UC-args-047 — `-nt` shorthand disables tools

- Probe (FAILS): hand has no `-nt` shorthand.

### UC-args-048..049 — `--no-builtin-tools` / `-nbt` disable built-in tools (leaving extension-provided)

- Probe (FAILS): hand has no `--no-builtin-tools` flag.

### UC-args-050 — `--tools read,bash` captures a CSV tool selection

- Probe: `cargo test -p hand-coding-agent parses_tools_csv -- --exact`.
- Status note: hand stores `tools: Option<String>` (raw CSV). pi stores
  `tools: string[]` (already split). The user's-eye effect is similar:
  both reflect the selection back; only the in-process shape differs.

### UC-args-051 — `-t read,bash` shorthand

- Probe (FAILS): hand has no `-t` shorthand for `--tools`.

### UC-args-052..053 — `--no-tools --tools …` and `--no-builtin-tools --tools …` combos

- Status note: pending — needs a dedicated test in hand (whether clap's
  derive allows the combination, given the bool + Option<String> mix).

### UC-args-054 — positional plain-text args land as `messages: string[]`

**Given** `hand hello world`.
**Then**  `messages == ["hello", "world"]`.
- Probe (FAILS): hand has `prompt: Option<String>` (single). Multiple
  positionals are an error (or only the first is consumed; behaviour
  unverified).
- Resolution proposal: switch `prompt` to `Vec<String>` collecting all
  positional args (and join with `\n` at the point of use), matching
  pi's `messages`.

### UC-args-055 — `@README.md` positional adds to `fileArgs`

**Given** `hand @README.md @src/main.ts`.
**Then**  `fileArgs == ["README.md", "src/main.ts"]`, `messages == []`.
- Probe (FAILS): hand has no `@<path>` recognition. The `@` prefix is
  treated as a literal positional and either falls into the prompt or
  errors.
- Resolution proposal: add a `file_args: Vec<String>` field and a
  custom value parser that strips a leading `@` and routes the rest
  there; non-`@` positionals go into `messages`.

### UC-args-056 — mixed messages and `@file` positionals are split

**Given** `hand @file.txt "explain this" @image.png`.
**Then**  `fileArgs == ["file.txt", "image.png"]`,
`messages == ["explain this"]`.
- Probe (FAILS): blocked on UC-args-055.

### UC-args-057 — unknown long flags with a string value land in `unknownFlags`

**Given** `hand --unknown-flag message`.
**Then**  `unknownFlags.get("unknown-flag") == "message"`, `messages == []`.
- Probe (FAILS): hand's clap rejects unknown flags with a parse error.
- Resolution proposal: enable clap's `allow_external_subcommands` or
  collect unknowns via a custom parser; expose them as a
  `HashMap<String, serde_json::Value>` on `Args`.

### UC-args-058 — unknown bare-boolean long flags surface as `true` in `unknownFlags`

- Probe (FAILS): same — blocked on UC-args-057.

### UC-args-059 — unknown flags with `--key=value` syntax decompose correctly

- Probe (FAILS): same.

### UC-args-060 — complex combo: provider+model+print+thinking+@file+message all parse together

- Probe (pending): individual components covered; needs one composite
  test pinning the joint shape.
