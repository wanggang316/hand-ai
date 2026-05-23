# UX-parity findings — pi vs hand (subagent 2026-05-23 sweep)

Output of a user-perspective subagent ran without access to source. 36 findings, severity-tagged.

## Methodology

- Tools: `pi` (v0.72.1, in PATH) and `hand` (target/release/hand).
- Both auth-configured with the same `openrouter` + `zai` keys at
  `~/.pi/agent/auth.json` and `~/.hand/agent/auth.json`.
- Harness: `/tmp/hand-vs-pi/compare.sh <LABEL> -- <shared-args>` runs
  both binaries with identical args, captures stdout/stderr/exit.

## Findings table

| # | Category | Reproducer | pi | hand | Severity | Status |
|---|---|---|---|---|---|---|
| 1 | CLI surface | `hand list` | prints installed extensions | falls into interactive TUI | blocker | open |
| 2 | CLI surface | `hand install foo` | installs source, exits 1 on failure | falls into interactive TUI | blocker | open |
| 3 | CLI surface | `hand config` | opens config TUI | falls into interactive TUI | major | open |
| 4 | CLI surface | `hand update` | updates extensions and self | no subcommand | major | open |
| 5 | CLI surface | `hand remove foo` / `hand uninstall foo` | removes source | no subcommand | major | open |
| 6 | CLI surface | `--prompt-template /path` | accepted | clap rejects | major | open |
| 7 | CLI surface | `--theme /path` and `--no-themes` | accepted | absent | major | open |
| 8 | CLI surface | `-V` (cargo-style version) | rejected | prints version (exit 0) | cosmetic | open |
| 9 | CLI surface | `-d /tmp -p hi` | rejected | hand accepts `-d/--cwd` | cosmetic | hand-extra |
| 10 | CLI surface | `--base-url` | rejected | hand accepts | cosmetic | hand-extra |
| 11 | CLI surface | `--rpc` shortcut | rejected | hand accepts both `--rpc` and `--mode rpc` | cosmetic | hand-extra |
| 12 | CLI surface | `--diagnostics` | rejected | hand prints diagnostics | cosmetic | hand-extra |
| 13 | CLI surface | `-ns/-ne/-np` short aliases | accepted | only `-nt/-nbt/-nc` aliased; `-ns/-ne/-np` rejected | major | open |
| 14 | --print | `pi -p "say hi" --provider … --model …` raw bytes | clean: `Hi!\n` | trailing `\n\n\x1b[1;35m>\x1b[0m ` REPL prompt | blocker | **fixed (-p rebind)** |
| 15 | --print | `pi --print "say hi" --provider …` (positional + long `--print`) | works | exit 1, "No API key found for Anthropic" | blocker | **fixed** |
| 16 | --print | `pi "say hi" -p --provider …` (positional before `-p`) | works | drops into REPL | major | **fixed (-p rebind)** |
| 17 | --print | `pi --prompt "msg"` long form | pi rejects | hand accepts | cosmetic | hand-extra (intentional) |
| 18 | --print | `echo say hi \| pi -p --provider …` (stdin + bare `-p`) | reads stdin, answers | drops into interactive REPL | blocker | **fixed (-p rebind)** |
| 19 | --print + @file | `pi @scenarios.sh "summarize" -p …` | loads file, summarizes | REPL | major | **fixed (@file validation)** |
| 20 | --print + @file | `pi -p "summarize" @/tmp/missing.md …` | exit 1, "File not found: …" | silent exit 0 | blocker | **fixed (@file validation)** |
| 21 | Error msg | `--model totally/fake -p hi` | warns, exit 1, provider 400 visible | silent exit 0, empty stdout | blocker | **fixed (-p rebind, exit 1)** |
| 22 | Error msg | `--export` (no arg) | exit 1, one-line | exit 2, clap verbose dump | cosmetic | open |
| 23 | Error msg | `--export /tmp/missing.jsonl` | "File not found: …" | leaky `Session error: Cannot export an in-memory session…` | major | open |
| 24 | Error msg | `--fork nonexistent-id …` | "No session found matching '…'" | adds `Error:` prefix | cosmetic | open |
| 25 | Error msg | `--thinking bogus -p hi …` | warns + proceeds | silently accepted | cosmetic | open |
| 26 | --mode json | `-p "say hi" --mode json` | full JSONL event stream | plain text + REPL prompt | blocker | **fixed (json events now emit; schema differs but shape matches)** |
| 27 | Default model | `-p "say hi"` (no flags / env) | falls back to google+auth.json, answers | empty stdout, exit 0 | major | **fixed (env var fallback + smart auto-pick)** |
| 28 | Output formatting | `-p "say hi"` raw bytes | clean `Hi!\n` | leading `\n` + trailing `\n\n\x1b[1;35m>\x1b[0m ` | major | **fixed (-p rebind)** |
| 29 | Output formatting | `-p …` stderr | OSC 777 notify escape | nothing | cosmetic | open |
| 30 | Output formatting | `--list-models` dest | stderr | stdout | cosmetic | open |
| 31 | Output formatting | model catalogue | richer (deepseek-v4-*, gemini-3.1-*) | smaller; has kimi-coding/minimax-cn | cosmetic | open |
| 32 | --continue | `-c -p "?"  --session-dir /tmp/empty` | finds session elsewhere, answers | exposes internal `Session(...)` wrapper | major | open |
| 33 | --resume | `-r -p hi --session-dir /tmp/empty` | opens TUI picker | exposes `Session("...")` repr | blocker | open |
| 34 | Side effects | any invocation in cwd | sessions go to `~/.pi/agent/sessions/` | hand creates `<cwd>/.hand/sessions/` polluting cwd | major | **fixed (sessions now under ~/.hand/agent/sessions/<flat-cwd>/)** |
| 35 | Help routing | `--help` | stderr | stdout | cosmetic | open |
| 36 | Unknown flag | `--bogus` | exit 1, one-line | exit 2, clap dump | cosmetic | open |

## Skipped (TTY-required)

- Ctrl-C / SIGINT mid-stream
- `pi config` TUI interaction
- `pi -r` TUI picker
- Image / binary `@file`
