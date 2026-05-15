# User-Cases: core/session_manager

**Upstream source:** `pi-mono/packages/coding-agent/test/session-info-modified-timestamp.test.ts`
+ `session-cwd.test.ts` + `sdk-session-manager.test.ts` (7 cases
covering session manager core; the 19 session-selector cases live
under tui/modes).
**hand-ai source:**   `crates/coding-agent/src/core/session_manager.rs`
**Surface:**          `SessionManager` — JSONL-backed session
persistence. Reads / writes the
`{"type": <tag>, "data": {...}}` envelope shape. Exposes `list`,
`open`, `append_message`, `fork_from`, `build_session_info`. The
SessionInfo.modified field MUST track the latest message timestamp,
not file mtime (which fluctuates with backups / cloud sync / atime
touches).

## Status

| ID | Status | Verified-by |
|----|--------|-------------|
| UC-sm-001 | ✅ pass | `test_session_info_modified_uses_message_timestamp_not_mtime` |
| UC-sm-002 | ⚠️ pending | "default persisted session path uses agentDir" — hand uses `~/.hand/sessions` directly; agentDir override surface differs |
| UC-sm-003 | ⚠️ pending | "explicit SessionManager override is kept" — hand allows constructing AgentSession with a custom session_manager; needs explicit test |
| UC-sm-004 | ⚠️ pending | "cwd derived from explicit SessionManager when cwd omitted" |
| UC-sm-005 | ⚠️ pending | "detect missing session cwd from persisted sessions" — hand has `session_cwd_missing` detection somewhere; needs probe |
| UC-sm-006 | ⚠️ pending | "supports overriding effective cwd when opening" |
| UC-sm-007 | ❌ fail | hand throws a generic IO error when stored cwd is missing; pi throws a controlled error before runtime creation |

## Cases

### UC-sm-001 — `SessionInfo.modified` reflects the latest message timestamp, not file mtime

**Given** an existing session JSONL file whose mtime is `T_mtime`
(set by the filesystem, possibly very recent due to a backup tool
touching it).
**When**  the session has at least one message whose `timestamp` is
some specific value `T_msg` (NOT equal to `T_mtime`).
**Then**  `SessionManager::list(...)` returns a `SessionInfo` whose
`modified.getTime() == T_msg` — NOT `T_mtime`.

- Assertion: `info.modified == T_msg`.
- Assertion: `info.modified != T_mtime`.
- Probe: `cargo test -p hand-coding-agent test_session_info_modified_uses_message_timestamp_not_mtime -- --exact`.
- Why: cloud sync, backup software, and even atime updates change
  mtime without changing session content. Sorting the picker by
  mtime would shuffle the user's recent sessions in confusing ways.

### UC-sm-002 — `agent_dir` controls the default session directory

**Given** `create_agent_session({ agent_dir: "/custom/agentdir" })`
with no explicit `session_manager`.
**When**  the agent persists a session.
**Then**  the file lands under `/custom/agentdir/sessions/` (or
hand's equivalent default subpath).

- Probe (pending): hand's `AgentSessionConfig` may not have a
  parallel `agent_dir` knob; needs verification.

### UC-sm-003 — passing an explicit `session_manager` keeps it intact

**Given** `create_agent_session({ session_manager: custom })`.
**When**  the agent runs.
**Then**  the same `custom` instance is used; no new manager is
allocated.

- Probe (pending): needs explicit test pinning identity.

### UC-sm-004 — when `cwd` is omitted but a `session_manager` is supplied, cwd derives from the session header

**Given** an existing session whose header records
`cwd: "/saved/cwd"`, opened via an explicit session_manager.
**When**  `create_agent_session` is called with no `cwd`.
**Then**  the runtime cwd is `/saved/cwd`.

- Probe (pending): needs explicit test.

### UC-sm-005 — `session_cwd_missing` detection flags sessions whose stored cwd no longer exists

**Given** a session whose header has `cwd: "/tmp/old-deleted-dir"`
(no longer on disk).
**When**  the manager inspects the session.
**Then**  it surfaces a "missing cwd" signal (e.g. an info field or
an exception) so the caller can offer to relocate or skip.

- Probe (pending): hand has missing-cwd handling around session
  resume but the exact surface needs to be located.

### UC-sm-006 — opening a session supports an effective-cwd override

**Given** a stored session with `cwd: "/old"`.
**When**  the caller opens it with `effective_cwd: "/new"`.
**Then**  the resumed runtime uses `/new` regardless of header.

- Probe (pending): needs explicit test.

### UC-sm-007 — opening a session whose stored cwd is missing throws BEFORE runtime creation

**Given** a stored session with `cwd: "/tmp/no-such-dir"`.
**When**  the user attempts to open it.
**Then**  the call returns a controlled error whose message names
the missing cwd; no agent runtime is allocated; no extension
lifecycle hook fires.

- Probe (FAILS): hand likely surfaces the failure later (e.g. when
  the runtime tries to spawn bash with an invalid cwd), not up
  front at session-load time.
- Resolution proposal: validate `header.cwd` in `SessionManager::open`
  and bail with `SessionManagerError::CwdMissing` before any
  AgentSession allocation.
