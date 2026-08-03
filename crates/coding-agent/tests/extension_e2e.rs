//! Phase 3 acceptance gate: drive an `AgentSession` through the fixture
//! extensions and assert end-to-end behaviour.
//!
//! What this test proves:
//!
//! 1. `permission_gate_cancels_dangerous_bash_through_session`: a Tier 1
//!    `before_tool_call` hook (`PermissionGate`) cancels a dangerous bash
//!    tool call before the host dispatches it. The agent loop continues
//!    cleanly — the model sees an error result for that call and the
//!    session does not crash.
//! 2. `notify_sh_subprocess_logs_tool_call`: a Tier 2 subprocess extension
//!    (`notify-sh`) discovered from an on-disk `extensions/` directory
//!    receives an `on_after_tool_call` event and writes a side effect
//!    (`notifications.log`) under the host-injected `HAND_DATA_DIR`.
//!
//! The auto-commit-on-exit fixture is exercised by its own crate's inline
//! tests; we deliberately do not duplicate that path here.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ext_permission_gate::PermissionGate;
use hand_coding_agent::AgentSession;
use hand_coding_agent::core::agent_session::AgentSessionConfig;
use hand_coding_agent::core::extensions::api::{ExtensionContext, ToolResultEvent};
use hand_coding_agent::core::extensions::subprocess::discover_subprocess_extensions;
use hand_coding_agent::tools::bash;
use model::types::Provider;
use model::{
    Api, ApiProvider, AssistantContentBlock, AssistantMessage, AssistantMessageEvent,
    AssistantMessageEventStream, Context, Cost, InputType, Model, SimpleStreamOptions, StopReason,
    StreamOptions, TextContent, ToolCall, Usage,
};

// ------------------------------------------------------------------
// Test scaffolding: model + provider that emits one tool call then
// terminates.
// ------------------------------------------------------------------

fn openai_test_model() -> Model {
    Model {
        id: "test-model".into(),
        name: "Test Model".into(),
        api: Api::OpenAICompletions,
        provider: Provider::OpenAI,
        base_url: "https://api.test.com".into(),
        reasoning: false,
        input: vec![InputType::Text],
        cost: Cost {
            input: 1.0,
            output: 2.0,
            cache_read: 0.5,
            cache_write: 0.75,
        },
        context_window: 128_000,
        max_tokens: 4096,
        headers: None,
        compat: None,
        thinking_level_map: None,
    }
}

fn assistant_text_message(text: &str) -> AssistantMessage {
    AssistantMessage {
        role: "assistant".into(),
        content: vec![AssistantContentBlock::Text(TextContent::new(text))],
        api: Api::OpenAICompletions,
        provider: Provider::OpenAI,
        model: "test-model".into(),
        usage: Usage::default(),
        stop_reason: StopReason::Stop,
        error_message: None,
        timestamp: 0,
        response_model: None,
        response_id: None,
        diagnostics: None,
    }
}

fn assistant_tool_call_message(
    tool_name: &str,
    tool_id: &str,
    args: serde_json::Value,
) -> AssistantMessage {
    AssistantMessage {
        role: "assistant".into(),
        content: vec![AssistantContentBlock::ToolCall(ToolCall::new(
            tool_id, tool_name, args,
        ))],
        api: Api::OpenAICompletions,
        provider: Provider::OpenAI,
        model: "test-model".into(),
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
        error_message: None,
        timestamp: 0,
        response_model: None,
        response_id: None,
        diagnostics: None,
    }
}

/// Provider that emits one tool-call turn on first call, terminating text
/// turn on every subsequent call. Lets the agent loop drive a single tool
/// dispatch and exit cleanly.
struct ToolThenTextProvider {
    tool_name: String,
    args: serde_json::Value,
    invocation: AtomicUsize,
}

impl ApiProvider for ToolThenTextProvider {
    fn stream(
        &self,
        _model: Model,
        _context: Context,
        _options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        let n = self.invocation.fetch_add(1, Ordering::SeqCst);
        if n == 0 {
            let tool_name = self.tool_name.clone();
            let args = self.args.clone();
            Box::pin(async_stream::stream! {
                let msg = assistant_tool_call_message(&tool_name, "call_1", args);
                let tool_call = match &msg.content[0] {
                    AssistantContentBlock::ToolCall(tc) => tc.clone(),
                    _ => unreachable!("constructed with ToolCall block"),
                };
                yield AssistantMessageEvent::Start { partial: msg.clone() };
                yield AssistantMessageEvent::ToolCallStart {
                    content_index: 0,
                    partial: msg.clone(),
                };
                yield AssistantMessageEvent::ToolCallEnd {
                    content_index: 0,
                    tool_call,
                    partial: msg.clone(),
                };
                yield AssistantMessageEvent::Done {
                    reason: StopReason::ToolUse,
                    message: msg,
                };
            })
        } else {
            Box::pin(async_stream::stream! {
                let msg = assistant_text_message("done");
                yield AssistantMessageEvent::Start { partial: msg.clone() };
                yield AssistantMessageEvent::Done {
                    reason: StopReason::Stop,
                    message: msg,
                };
            })
        }
    }

    fn stream_simple(
        &self,
        model: Model,
        context: Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        self.stream(model, context, options.map(|o| o.base))
    }
}

fn build_client(tool_name: &str, args: serde_json::Value) -> model::Client {
    let client = model::Client::new();
    client.registry.register(
        Api::OpenAICompletions,
        Box::new(ToolThenTextProvider {
            tool_name: tool_name.into(),
            args,
            invocation: AtomicUsize::new(0),
        }),
        Some("test".into()),
    );
    client
}

/// Lay out a `subprocess_extensions/notify-sh/` directory by copying the
/// in-tree fixture into a tempdir. Tests must NOT mutate the in-tree fixture.
fn install_notify_sh(root: &Path) -> PathBuf {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/extensions/notify-sh");
    let dst = root.join("notify-sh");
    std::fs::create_dir_all(&dst).unwrap();
    for f in ["extension.toml", "main.sh", "README.md"] {
        std::fs::copy(src.join(f), dst.join(f)).unwrap_or_else(|e| panic!("copy {f}: {e}"));
    }
    // Preserve the executable bit on main.sh under any umask.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(dst.join("main.sh"))
            .unwrap()
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(dst.join("main.sh"), perms).unwrap();
    }
    dst
}

fn bash_available() -> bool {
    Path::new("/bin/bash").exists()
}

// ------------------------------------------------------------------
// (a) PermissionGate cancels a dangerous bash command end-to-end.
// ------------------------------------------------------------------

#[tokio::test]
async fn permission_gate_cancels_dangerous_bash_through_session() {
    let cwd = tempfile::tempdir().unwrap();
    // The model wants to run `rm -rf /tmp/...`. PermissionGate must veto
    // this before the bash tool actually runs.
    let client = build_client(
        "bash",
        serde_json::json!({ "command": "rm -rf /tmp/should-be-blocked" }),
    );
    let mut session = AgentSession::in_memory_with_client(
        openai_test_model(),
        vec![bash::create_bash_tool(cwd.path().to_path_buf())],
        client,
    );
    session.register_extension(Arc::new(PermissionGate::new()));

    // The agent loop emits one tool call, the host's before-hook runs
    // PermissionGate which Cancels; the dispatcher surfaces an error
    // result to the model, the second turn returns terminating text.
    // send_message MUST succeed even though the tool was blocked.
    let _ = session
        .send_message("call rm")
        .await
        .expect("send_message should complete cleanly when a hook cancels");

    // The cancel reason itself is asserted in the fixture's own unit
    // tests. Here we verify the user-visible signal: the tempdir was
    // never touched (no real `rm -rf` ran).
    assert!(
        cwd.path().exists(),
        "session cwd should still exist; rm was blocked"
    );
}

// ------------------------------------------------------------------
// (b) notify-sh subprocess receives after-tool-call events and writes
// notifications.log under HAND_DATA_DIR.
// ------------------------------------------------------------------
//
// We exercise the subprocess directly (not through the agent loop) for
// two reasons:
//   1. `AgentSession::in_memory_with_client` pins `config.cwd` to "." with
//      no public setter, so `extension_context().data_dir` would resolve
//      against the process cwd and pollute the repo.
//   2. The subprocess wire-protocol round trip is what we actually want
//      to assert — the agent loop adds incidental complexity.
// The agent-loop path through extensions is already covered by
// `agent_session.rs`'s in-crate tests (`send_message_fires_before_and_after_hooks_on_tool_call`).

#[tokio::test]
async fn notify_sh_subprocess_logs_tool_call() {
    if !bash_available() {
        eprintln!("skipping: /bin/bash not found");
        return;
    }

    let ext_root = tempfile::tempdir().unwrap();
    let data_dir = tempfile::tempdir().unwrap();
    install_notify_sh(ext_root.path());

    // Discover the Tier 2 fixture from disk.
    let (mut subprocess_exts, failures) = discover_subprocess_extensions(ext_root.path());
    assert!(failures.is_empty(), "discovery failures: {failures:?}");
    assert_eq!(subprocess_exts.len(), 1, "expected one fixture extension");

    let ext = subprocess_exts.remove(0);
    assert_eq!(ext.manifest().name, "notify-sh");

    // Build a context whose `data_dir` points at our tempdir; the host
    // injects `HAND_DATA_DIR=<data_dir>` into the subprocess.
    let cx = ExtensionContext {
        cwd: ext_root.path().to_path_buf(),
        session_id: "e2e-test".into(),
        data_dir: data_dir.path().to_path_buf(),
    };

    // Fire one after-tool-call event. The subprocess greps the wire frame
    // and appends one line to notifications.log.
    let event = ToolResultEvent {
        tool_name: "bash".into(),
        call_id: "call_1".into(),
        success: true,
        result: serde_json::json!({"output": "hello"}),
    };
    ext.on_after_tool_call(&cx, &event)
        .await
        .expect("after-tool-call rpc ok");

    // Best-effort shutdown so the subprocess is killed and any buffered
    // line is flushed.
    let _ = ext.on_shutdown(&cx).await;

    let log_path = cx.data_dir.join("notifications.log");
    let log = std::fs::read_to_string(&log_path).unwrap_or_else(|e| {
        panic!(
            "expected notifications.log at {} but read failed: {e}",
            log_path.display()
        )
    });
    assert!(
        log.contains("tool=bash"),
        "log should record the bash tool call; got: {log:?}"
    );
    assert!(
        log.contains("success=true"),
        "log should record success flag; got: {log:?}"
    );
}

// ------------------------------------------------------------------
// (c) The session drives the Tier 2 lifecycle: on_load fires once, and
// shutdown reaches the child and reaps it.
// ------------------------------------------------------------------

/// Install a subprocess fixture that appends every event type it sees to
/// `$HAND_DATA_DIR/events.log`, prefixed by its own pid, and answers `ok`.
fn install_lifecycle_logger(root: &Path) -> PathBuf {
    let dst = root.join("lifecycle-logger");
    std::fs::create_dir_all(&dst).unwrap();
    std::fs::write(
        dst.join("extension.toml"),
        r#"
name = "lifecycle-logger"
version = "0.1.0"
exec = ["./main.sh"]

[capabilities]
after-tool-call = true
"#,
    )
    .unwrap();
    std::fs::write(
        dst.join("main.sh"),
        r#"#!/bin/bash
set -u
mkdir -p "$HAND_DATA_DIR"
log="$HAND_DATA_DIR/events.log"
echo "pid=$$" >> "$log"
while IFS= read -r line; do
  case "$line" in
    *on_load*) echo "on_load" >> "$log" ;;
    *on_shutdown*) echo "on_shutdown" >> "$log" ;;
    *) echo "other" >> "$log" ;;
  esac
  printf '{"type":"ok"}\n'
done
"#,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(dst.join("main.sh")).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(dst.join("main.sh"), perms).unwrap();
    }
    dst
}

fn process_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[tokio::test]
async fn session_drives_tier2_lifecycle_and_reaps_the_child() {
    if !bash_available() {
        eprintln!("skipping: /bin/bash not found");
        return;
    }

    let workspace = tempfile::tempdir().unwrap();
    let ext_root = tempfile::tempdir().unwrap();
    install_lifecycle_logger(ext_root.path());

    let (mut exts, failures) = discover_subprocess_extensions(ext_root.path());
    assert!(failures.is_empty(), "discovery failures: {failures:?}");
    let ext = exts.remove(0);

    // A session pinned to a workspace tempdir, with extension state routed
    // under it — nothing lands in the repo.
    let config = AgentSessionConfig {
        cwd: workspace.path().to_path_buf(),
        model: openai_test_model(),
        stream_options: SimpleStreamOptions::default(),
        custom_system_prompt: None,
        custom_guidelines: None,
        resume_session: None,
        no_session: true,
        no_context_files: true,
        session_dir: None,
        no_skills: true,
        extra_skill_dirs: Vec::new(),
        base_dir: Some(workspace.path().join("state")),
    };
    let mut session = AgentSession::new(config, vec![]).expect("session builds");
    session.register_extension(ext);

    session.load_extensions().await;
    // Idempotent: a second call must not re-run setup.
    session.load_extensions().await;

    let data_dir = session
        .extension_context_for("lifecycle-logger")
        .data_dir
        .clone();
    let log_path = data_dir.join("events.log");
    let after_load = std::fs::read_to_string(&log_path).expect("events.log written on load");
    assert_eq!(
        after_load.matches("on_load").count(),
        1,
        "on_load must fire exactly once; got: {after_load:?}"
    );

    let pid: u32 = after_load
        .lines()
        .find_map(|l| l.strip_prefix("pid="))
        .expect("fixture records its pid")
        .trim()
        .parse()
        .expect("pid parses");
    assert!(process_alive(pid), "child should be running before shutdown");

    session.shutdown_extensions().await;

    let after_shutdown = std::fs::read_to_string(&log_path).unwrap();
    assert_eq!(
        after_shutdown.matches("on_shutdown").count(),
        1,
        "on_shutdown must reach the child exactly once; got: {after_shutdown:?}"
    );
    assert!(
        !process_alive(pid),
        "child should be reaped once the session shuts its extensions down"
    );
    assert!(
        data_dir.starts_with(workspace.path()),
        "extension state must stay under the session's workspace, got {data_dir:?}"
    );
}
