//! Tier 1 extension: blocks dangerous bash commands at the agent loop boundary.
//!
//! Ported from `pi-mono/.../examples/extensions/permission-gate.ts`.
//!
//! Registers a `before_tool_call` hook. When the model issues a `bash` tool
//! call, the command string is checked against a small blocklist of obviously
//! destructive substrings. A match returns `HookDecision::Cancel(reason)` so
//! the host blocks the tool call and the model receives an error result.
//!
//! This fixture intentionally uses simple substring matching — it is a demo,
//! not a production sandbox. A real permission system would prompt the user
//! and/or run the tool inside a sandbox.

use async_trait::async_trait;
use hand_coding_agent::core::extensions::api::{ToolCallEvent, ToolResultEvent};
use hand_coding_agent::{
    Extension, ExtensionContext, ExtensionError, ExtensionManifest, HookDecision,
};

/// Default blocklist substrings. A bash command containing any of these is
/// blocked. Matching is case-sensitive — that's enough for a demo and avoids
/// pulling in a regex dependency for the fixture.
const DEFAULT_DENY_PATTERNS: &[&str] = &[
    "rm -rf",
    "rm -fr",
    "sudo ",
    "chmod 777",
    "chown 777",
    "mkfs",
    ":(){ :|:& };:",
];

pub struct PermissionGate {
    manifest: ExtensionManifest,
    /// Substrings that mark a bash command as dangerous.
    deny_patterns: Vec<&'static str>,
}

impl PermissionGate {
    pub fn new() -> Self {
        Self {
            manifest: ExtensionManifest {
                name: "permission-gate".to_string(),
                version: "0.1.0".to_string(),
                description: Some(
                    "Blocks bash commands matching a small dangerous-command blocklist."
                        .to_string(),
                ),
                capabilities: hand_coding_agent::core::extensions::api::ExtensionCapabilities {
                    before_tool_call: true,
                    ..Default::default()
                },
                exec: None,
                env: Default::default(),
                slash_commands: Vec::new(),
                custom_tools: Vec::new(),
            },
            deny_patterns: DEFAULT_DENY_PATTERNS.to_vec(),
        }
    }
}

impl Default for PermissionGate {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Extension for PermissionGate {
    fn manifest(&self) -> &ExtensionManifest {
        &self.manifest
    }

    async fn on_before_tool_call(
        &self,
        _cx: &ExtensionContext,
        event: &ToolCallEvent,
    ) -> Result<HookDecision, ExtensionError> {
        if event.tool_name != "bash" {
            return Ok(HookDecision::Continue);
        }
        // The bash tool's input schema declares `command: string`. Anything
        // else is treated as not-dangerous; a malformed call will be caught
        // by the tool itself.
        let cmd = event
            .arguments
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        for pattern in &self.deny_patterns {
            if cmd.contains(pattern) {
                return Ok(HookDecision::Cancel(format!(
                    "permission-gate: blocked dangerous command (matched {pattern:?})"
                )));
            }
        }
        Ok(HookDecision::Continue)
    }

    async fn on_after_tool_call(
        &self,
        _cx: &ExtensionContext,
        _event: &ToolResultEvent,
    ) -> Result<(), ExtensionError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ctx() -> ExtensionContext {
        ExtensionContext {
            cwd: PathBuf::from("/tmp"),
            session_id: "test-session".to_string(),
            data_dir: PathBuf::from("/tmp/data"),
        }
    }

    fn bash_event(command: &str) -> ToolCallEvent {
        ToolCallEvent {
            tool_name: "bash".to_string(),
            arguments: serde_json::json!({ "command": command }),
            call_id: "call-1".to_string(),
        }
    }

    #[tokio::test]
    async fn non_bash_tool_is_allowed() {
        let gate = PermissionGate::new();
        let event = ToolCallEvent {
            tool_name: "read".to_string(),
            arguments: serde_json::json!({ "path": "/etc/hosts" }),
            call_id: "call-1".to_string(),
        };
        let decision = gate.on_before_tool_call(&ctx(), &event).await.unwrap();
        assert!(matches!(decision, HookDecision::Continue));
    }

    #[tokio::test]
    async fn safe_bash_command_is_allowed() {
        let gate = PermissionGate::new();
        let decision = gate
            .on_before_tool_call(&ctx(), &bash_event("echo hello"))
            .await
            .unwrap();
        assert!(matches!(decision, HookDecision::Continue));
    }

    #[tokio::test]
    async fn rm_rf_is_cancelled_with_reason() {
        let gate = PermissionGate::new();
        let decision = gate
            .on_before_tool_call(&ctx(), &bash_event("rm -rf /tmp/foo"))
            .await
            .unwrap();
        match decision {
            HookDecision::Cancel(reason) => {
                assert!(
                    reason.contains("permission-gate") && reason.contains("rm -rf"),
                    "unexpected reason: {reason}"
                );
            }
            other => panic!("expected Cancel, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn chmod_777_is_cancelled() {
        let gate = PermissionGate::new();
        let decision = gate
            .on_before_tool_call(&ctx(), &bash_event("chmod 777 /etc/passwd"))
            .await
            .unwrap();
        assert!(matches!(decision, HookDecision::Cancel(_)));
    }
}
