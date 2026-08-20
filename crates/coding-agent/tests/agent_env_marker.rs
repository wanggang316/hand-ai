//! The `AI_AGENT` marker must reach anything the agent spawns.
//!
//! Hooks, `Makefile`s, and shell profiles branch on it to skip an
//! interactive confirm or pick machine-readable output, so what matters
//! is not that the process sets a variable but that a *child* sees it.
//! This drives the real binary over its RPC mode and asks it to run a
//! shell command, which is the same path a tool call takes.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn hand_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_hand"))
}

/// Run one RPC command against the binary and return its stdout.
fn rpc_roundtrip(command_line: &str) -> String {
    let mut child = Command::new(hand_bin())
        .args(["--mode", "rpc"])
        .env(
            "HAND_HOME",
            std::env::temp_dir().join("hand-agent-env-marker-test"),
        )
        // Deliberately not set here: the binary must supply it itself.
        .env_remove("AI_AGENT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn hand in rpc mode");

    child
        .stdin
        .as_mut()
        .expect("stdin piped")
        .write_all(command_line.as_bytes())
        .expect("write rpc command");
    // Dropping stdin closes it, which ends the dispatcher loop.
    drop(child.stdin.take());

    let out = child.wait_with_output().expect("collect rpc output");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A shell command run by the agent sees the marker, naming this agent.
#[test]
fn a_spawned_shell_sees_the_agent_marker() {
    let stdout = rpc_roundtrip(
        "{\"type\":\"bash\",\"id\":\"1\",\"command\":\"printf %s \\\"$AI_AGENT\\\"\"}\n",
    );

    assert!(
        stdout.contains("\"stdout\":\"hand\""),
        "the child shell must see AI_AGENT=hand; got:\n{stdout}"
    );
}
