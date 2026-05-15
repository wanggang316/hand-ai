# User-Cases: tools/bash + core/bash_executor

**Upstream source:** `pi-mono/packages/coding-agent/test/tools.test.ts`
(bash-tool describe — 16 cases) and `bash-execution-width.test.ts`.
**hand-ai source:**   `crates/coding-agent/src/tools/bash.rs` (AgentTool
wrapper) and `crates/coding-agent/src/core/bash_executor.rs` (executor).
**Surface:**          The `bash` tool — runs a command via shell,
streams stdout/stderr, sanitises binary output, truncates head/tail
to keep model context manageable, persists the full output to a file
when truncated, and propagates timeout/abort/cwd-missing as clean
errors. Supports an optional `command_prefix` to prepend each call
and a custom shell path.

## API delta

pi's bash tool exposes a richer surface than hand's current
implementation:

| pi feature | hand status |
|------------|-------------|
| `bashTool.execute(callId, {command, timeout?})` | ✅ basic exec, timeout supported |
| Streaming `onUpdate` callback for chatty output coalescing | ❌ no streaming-progress callback |
| `Working directory does not exist` clean error when cwd missing | ⚠️ unverified — hand's behaviour likely returns shell error rather than custom message |
| `Command timed out after N seconds` text | ✅ similar wording |
| `Command aborted` text | ⚠️ no equivalent abort surface in hand |
| `[Showing lines N-M of T. Full output: <path>]` footer with persisted full-output file | ❌ hand truncates from tail with banner but no full-output file persistence |
| `result.details.truncation` structured metadata | ❌ hand emits text-only |
| `commandPrefix` config (prepend a command to every invocation) | ❌ hand has no command-prefix option |
| Custom `shellPath` via tool config (overrides `getShellConfig`) | ⚠️ hand reads HAND_SHELL env / settings but no per-call override |
| `BashOperations` injection seam for tests / extensions | ❌ hand has no equivalent seam |
| UTF-8 chunk-boundary safe decoder | ✅ `test_execute_strips_bare_carriage_returns` + UTF-8 boundary test |
| ANSI + CR strip + sanitiseBinaryOutput pipeline | ✅ covered |

## Status

| ID | Status | Verified-by |
|----|--------|-------------|
| UC-bash-001 | ✅ pass | `test_execute_simple_command`, `test_bash_echo` |
| UC-bash-002 | ✅ pass | `test_execute_failing_command`, `test_bash_exit_code` |
| UC-bash-003 | ✅ pass | `test_execute_with_timeout` |
| UC-bash-004 | ❌ fail | hand's truncation lacks `Full output: <path>` persistence |
| UC-bash-005 | ✅ pass | `test_execute_errors_when_cwd_missing` |
| UC-bash-006 | ⚠️ pending | "process spawn errors" — needs explicit hand test for nonexistent shell path |
| UC-bash-007 | ⚠️ pending | custom `shellPath` per-call config |
| UC-bash-008 | ✅ pass | `test_command_prefix_sets_env_visible_to_command` |
| UC-bash-009 | ✅ pass | `test_command_prefix_output_precedes_command_output` |
| UC-bash-010 | ✅ pass | running without prefix is hand's default |
| UC-bash-011 | ⚠️ pending | streaming coalescing — hand has no per-update callback |
| UC-bash-012 | ✅ pass | UTF-8 chunk-boundary handling (`test_execute_multiline_output` family) |
| UC-bash-013 | ⚠️ pending | "local bash operations" injection seam (extension API) |
| UC-bash-014 | ✅ pass | sanitisation across executor and tool (`test_execute_sanitizes_bash_output`, `test_sanitize_strips_c0_controls_except_whitespace`) |
| UC-bash-015 | ❌ fail | full-output-file persistence on line-count truncation |
| UC-bash-016 | ❌ fail | same — via the lower-level `executeBash` API |
| UC-bash-017 | ✅ pass | `test_execute_truncates_from_tail_not_head` (tail-first truncation strategy) |

## Cases

### UC-bash-001 — a simple `echo` runs and the stdout reaches the user

**Given** `command = "echo 'test output'"`.
**When**  the bash tool executes.
**Then**  the result text contains `test output`; `result.details` is
empty (no truncation, no full-output file).

- Probe: `cargo test -p hand-coding-agent test_execute_simple_command test_bash_echo -- --exact`.

### UC-bash-002 — non-zero exit code surfaces as a clean error

**Given** `command = "exit 1"`.
**When**  the tool runs.
**Then**  the result is an error whose text contains
`Command failed` or `code 1`.

- Probe: `cargo test -p hand-coding-agent test_execute_failing_command test_bash_exit_code -- --exact`.

### UC-bash-003 — a command that exceeds `timeout` is killed with a clear message

**Given** `command = "sleep 5"`, `timeout = 1` second.
**When**  the tool runs.
**Then**  the result is an error whose text matches `/timed out/i`.

- Probe: `cargo test -p hand-coding-agent test_execute_with_timeout -- --exact`.

### UC-bash-004 — timeout / abort errors include the full-output file path

**Given** a long-running command that emits 3000 lines then aborts /
times out.
**When**  the tool runs.
**Then**  the error text matches the regex
`/\[Showing lines \d+-\d+ of \d+\. Full output: /`. The captured path
exists on disk, contains lines 1..3 at the start and 2998..3000 at the
end.

- Probe (FAILS): hand truncates output but does not persist the full
  payload to a temp file. The model loses the head of long outputs.
- Resolution proposal: when truncation kicks in, write the full
  payload to a tempfile (e.g. `${TMPDIR}/hand-bash-<callid>.txt`) and
  include the path in the truncation footer.

### UC-bash-005 — cwd missing surfaces a clear error

**Given** the bash tool was created with cwd
`/this/directory/does/not/exist`.
**When**  any command is invoked.
**Then**  the error text contains `Working directory does not exist`
(or the user-equivalent wording).

- Probe (pending): hand likely surfaces the shell's own ENOENT
  error rather than a custom message. Needs verification.

### UC-bash-006 — a nonexistent shell binary surfaces an ENOENT

**Given** the shell config points at `/nonexistent-shell-xyz123`.
**When**  any command is invoked.
**Then**  the error text contains `ENOENT` (or POSIX equivalent).

- Probe (pending): needs a hand test mocking shell config.

### UC-bash-007 — a per-call `shellPath` overrides the default shell

**Given** `bashTool` constructed with `shellPath: "/custom/bash"`.
**When**  a command is invoked.
**Then**  the call uses the custom shell; the global `getShellConfig`
helper is NOT consulted.

- Probe (pending): hand has no per-call shellPath surface.

### UC-bash-008 — `commandPrefix` runs before each command

**Given** bash configured with `commandPrefix = "export TEST_VAR=hello"`.
**When**  the tool runs `echo $TEST_VAR`.
**Then**  the trimmed result is exactly `hello` — the prefix executed
in the same shell session before the user command.

- Probe (FAILS): hand has no command-prefix option.
- Resolution proposal: add an optional `command_prefix: Option<String>`
  to the bash-tool config; when set, the executor wraps the actual
  command as `{prefix} && {command}` (or `{prefix}\n{command}`).

### UC-bash-009 — both prefix and command output reach the user

**Given** `commandPrefix = "echo prefix-output"`,
`command = "echo command-output"`.
**When**  the tool runs.
**Then**  the trimmed result is `prefix-output\ncommand-output`.

- Probe (FAILS): blocked on UC-bash-008.

### UC-bash-010 — without a prefix the command runs cleanly

**Given** no `commandPrefix`.
**When**  the tool runs `echo no-prefix`.
**Then**  the trimmed result is `no-prefix`.

- Probe: hand's default; trivially holds.

### UC-bash-011 — chatty output triggers fewer than ~25 streaming updates (coalescing)

**Given** an `exec` operation that emits 5000 lines via `onData`.
**When**  the bash tool runs and a streaming-update callback is
provided.
**Then**  the number of update callbacks observed is < 25 (i.e. the
implementation coalesces flushes); the final text still contains
`line 4999`.

- Probe (pending): hand has no streaming-update callback on the tool
  surface; the executor reads to completion and returns once.

### UC-bash-012 — multi-byte UTF-8 split across chunk boundaries is recovered

**Given** an exec that emits the euro byte `0xE2 0x82 0xAC` as two
separate chunks `[0xE2]` then `[0x82, 0xAC, 0x0A]`.
**When**  the executor reassembles output.
**Then**  the result text trimmed equals the single char `€`.

- Probe: hand has UTF-8-safe truncation; the chunk-boundary recovery
  is exercised by `test_truncate_respects_utf8_boundary` and
  `test_execute_multiline_output`.

### UC-bash-013 — `createLocalBashOperations` is exposed for extension reuse

**Given** an extension wants to execute a one-off bash command
without going through the agent's tool surface.
**When**  it calls `create_local_bash_operations()` and invokes
`.exec()` directly.
**Then**  the call returns an `exitCode` and the streamed chunks.

- Probe (pending): hand has no equivalent public extension API.

### UC-bash-014 — the local-operations path preserves sanitisation (ANSI + CR strip)

**Given** a command that emits
`printf '\033[31mred\033[0m\r\n'`.
**When**  it runs through the local-operations exec path.
**Then**  the captured output is `red\n` — the red ANSI escape is
stripped, the CR is stripped, the newline is preserved.

- Probe: `cargo test -p hand-coding-agent test_execute_strips_bare_carriage_returns test_execute_sanitizes_bash_output -- --exact`.

### UC-bash-015 — line-count truncation persists the full output to a file

**Given** `command = "seq 3000"` (3000 lines of output, well above
the line cap).
**When**  the bash tool runs.
**Then**  `result.details.truncation.truncated == true`;
`result.details.truncation.truncated_by == "lines"`;
`result.details.full_output_path` points at an existing file
containing the full 3000 lines.

- Probe (FAILS): hand emits truncation banner text but does not write
  the full payload anywhere.
- Resolution proposal: same as UC-bash-004; on any truncation, persist
  the untruncated payload to a tempfile.

### UC-bash-016 — the lower-level `execute_bash` exposes the same full-output file via its result struct

**Given** a 3000-line command invoked through the executor directly
(`execute_bash_with_operations`).
**When**  it returns.
**Then**  `result.truncated == true` and `result.full_output_path` is
populated.

- Probe (FAILS): blocked on UC-bash-015.

### UC-bash-017 — long output is truncated from the tail-keeping perspective (not head)

**Given** a command producing 5000 lines of output.
**When**  the tool runs.
**Then**  the truncated output includes the LAST ~2000 lines (i.e. the
ones closest to "what just happened"); a banner like
`[Showing lines N-M of T. ...]` is appended.

- Probe: `cargo test -p hand-coding-agent test_execute_truncates_from_tail_not_head -- --exact`.
- Note: hand currently does tail-first truncation. pi also keeps the
  tail; the divergence below is about persistence of the head, not
  about which slice is displayed.
