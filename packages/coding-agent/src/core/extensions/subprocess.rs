//! Tier 2 subprocess extension implementation.
//!
//! Each subprocess extension is a child process that speaks the JSONL
//! JSON-RPC protocol from Phase 1 (`rpc::jsonl`). Hooks are translated to
//! "extension events" sent to the child; the child responds with the
//! decision (HookDecision-shaped JSON).
//!
//! # Concurrency
//!
//! A single mutex guards the child handle. RPC calls are therefore
//! serialized per extension. This is intentional: subprocess hosts are not
//! required to handle concurrent requests, and tool calls within a session
//! are sequential anyway. See ADR-001.
//!
//! # Lifecycle
//!
//! - The child is lazy-spawned on first hook fire.
//! - `on_shutdown` sends a final event (best-effort) and kills the process.
//! - If the host crashes, the OS reaps. No orphan story for v1.
//!
//! # Per-hook timeouts
//!
//! Not implemented in v1. A misbehaving extension can deadlock a session.
//! T3.5 will optionally add timeouts.

use crate::core::extensions::api::{
    Extension, ExtensionContext, ExtensionError, ExtensionManifest, HookDecision,
    SlashCommandSpec, ToolCallEvent, ToolResultEvent,
};
use crate::core::extensions::manifest::load_manifest;
use crate::rpc::jsonl::{JsonlReadError, read_jsonl, write_jsonl};
use async_trait::async_trait;
use futures::StreamExt;
use hand_agent::types::{AgentTool, BoxFuture, ToolExecuteFn, ToolExecutionContext, ToolResult};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::BufReader;
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, mpsc};

/// Wire format for events the host sends TO the subprocess extension.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExtensionEventOut {
    OnLoad {
        context: ContextDto,
    },
    OnShutdown {
        context: ContextDto,
    },
    OnBeforeToolCall {
        context: ContextDto,
        event: ToolCallDto,
    },
    OnAfterToolCall {
        context: ContextDto,
        event: ToolResultDto,
    },
    /// Invoke a manifest-declared custom tool. The subprocess responds with
    /// [`ExtensionEventIn::ToolResult`].
    ExecuteCustomTool {
        context: ContextDto,
        tool_name: String,
        arguments: serde_json::Value,
        call_id: String,
    },
    /// Invoke a manifest-declared slash command. The subprocess responds
    /// with [`ExtensionEventIn::SlashResult`].
    ExecuteSlashCommand {
        context: ContextDto,
        command_name: String,
        args: String,
    },
}

/// Wire format for responses the subprocess sends back.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExtensionEventIn {
    /// Generic acknowledgement, e.g. for on_load / on_after / on_shutdown.
    Ok,
    /// `HookDecision::Continue`.
    Continue,
    /// `HookDecision::Cancel(reason)`.
    Cancel { reason: String },
    /// `HookDecision::Replace(arguments)`.
    Replace { arguments: serde_json::Value },
    /// Subprocess-reported error; surfaces as `ExtensionError::Custom`.
    Error { message: String },
    /// Custom tool result. `content` is the text the model sees;
    /// `is_error` flags an error condition.
    ToolResult {
        content: String,
        #[serde(default)]
        is_error: bool,
    },
    /// Slash command result. `output` is what the host prints; `error` is
    /// shown when the command failed (and surfaces as ExtensionError).
    SlashResult {
        #[serde(default)]
        output: String,
        #[serde(default)]
        error: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextDto {
    pub cwd: PathBuf,
    pub session_id: String,
    pub data_dir: PathBuf,
}

impl From<&ExtensionContext> for ContextDto {
    fn from(cx: &ExtensionContext) -> Self {
        ContextDto {
            cwd: cx.cwd.clone(),
            session_id: cx.session_id.clone(),
            data_dir: cx.data_dir.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallDto {
    pub tool_name: String,
    pub arguments: serde_json::Value,
    pub call_id: String,
}

impl From<&ToolCallEvent> for ToolCallDto {
    fn from(event: &ToolCallEvent) -> Self {
        ToolCallDto {
            tool_name: event.tool_name.clone(),
            arguments: event.arguments.clone(),
            call_id: event.call_id.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultDto {
    pub tool_name: String,
    pub call_id: String,
    pub success: bool,
    pub result: serde_json::Value,
}

impl From<&ToolResultEvent> for ToolResultDto {
    fn from(event: &ToolResultEvent) -> Self {
        ToolResultDto {
            tool_name: event.tool_name.clone(),
            call_id: event.call_id.clone(),
            success: event.success,
            result: event.result.clone(),
        }
    }
}

/// A Tier 2 (subprocess) extension. Implements [`Extension`] so the host
/// cannot tell it apart from a Tier 1 in-process extension.
///
/// The shared state lives in [`SubprocessInner`] behind an `Arc` so that
/// tool execute closures (which need `'static + Send + Sync`) and slash
/// command handlers can clone a handle to drive RPC back into the
/// subprocess from the agent loop's tool list.
pub struct SubprocessExtension {
    inner: Arc<SubprocessInner>,
}

pub(crate) struct SubprocessInner {
    manifest: ExtensionManifest,
    /// Path to the directory containing `extension.toml`. Used as the cwd
    /// for the subprocess so relative `exec` paths resolve correctly.
    extension_dir: PathBuf,
    /// Lazy-spawned child process. Mutex-guarded for hook serialization.
    child: Mutex<Option<SubprocessHandle>>,
    /// Custom tool schemas pre-parsed from `manifest.custom_tools`. Indexed
    /// by tool name. Populated at construction; if any schema string fails
    /// JSON parsing, [`SubprocessExtension::new`] returns an error and the
    /// extension does not load.
    parsed_tool_schemas: std::collections::HashMap<String, serde_json::Value>,
}

struct SubprocessHandle {
    child: Child,
    stdin: tokio::process::ChildStdin,
    /// Stream of frames from the subprocess. A separate task reads stdout
    /// and pushes parsed frames here so the RPC method can `.recv()` one at
    /// a time.
    stdout_rx: mpsc::Receiver<Result<ExtensionEventIn, ExtensionError>>,
}

impl SubprocessExtension {
    /// Construct a new subprocess extension. Eagerly parses the JSON Schema
    /// of every declared custom tool; if any schema string is not valid
    /// JSON, returns an error and the extension fails to load.
    pub fn new(
        manifest: ExtensionManifest,
        extension_dir: PathBuf,
    ) -> Result<Self, ExtensionError> {
        let mut parsed_tool_schemas = std::collections::HashMap::new();
        for tool in &manifest.custom_tools {
            let value: serde_json::Value =
                serde_json::from_str(&tool.schema).map_err(|e| ExtensionError::Custom {
                    name: manifest.name.clone(),
                    message: format!(
                        "custom tool {:?}: schema is not valid JSON: {e}",
                        tool.name
                    ),
                })?;
            parsed_tool_schemas.insert(tool.name.clone(), value);
        }
        Ok(SubprocessExtension {
            inner: Arc::new(SubprocessInner {
                manifest,
                extension_dir,
                child: Mutex::new(None),
                parsed_tool_schemas,
            }),
        })
    }

    #[cfg(test)]
    pub(crate) fn inner_for_test(&self) -> Arc<SubprocessInner> {
        self.inner.clone()
    }
}

/// Pull `context.data_dir` out of any outbound event variant. Used at
/// subprocess spawn time to set `HAND_DATA_DIR`.
fn event_data_dir(event: &ExtensionEventOut) -> Option<&Path> {
    let dto = match event {
        ExtensionEventOut::OnLoad { context } => context,
        ExtensionEventOut::OnShutdown { context } => context,
        ExtensionEventOut::OnBeforeToolCall { context, .. } => context,
        ExtensionEventOut::OnAfterToolCall { context, .. } => context,
        ExtensionEventOut::ExecuteCustomTool { context, .. } => context,
        ExtensionEventOut::ExecuteSlashCommand { context, .. } => context,
    };
    Some(dto.data_dir.as_path())
}

impl SubprocessInner {
    /// Spawn the subprocess. Caller must hold the child mutex and have
    /// observed `None`.
    ///
    /// `data_dir` is exported as `HAND_DATA_DIR` so subprocess hosts (e.g.
    /// shell scripts that cannot easily parse JSON) can persist per-session
    /// state without scraping the event payload. The directory is created
    /// lazily here so the subprocess does not need to mkdir itself.
    fn spawn_locked(&self, data_dir: Option<&Path>) -> Result<SubprocessHandle, ExtensionError> {
        let exec = self.manifest.exec.as_ref().ok_or_else(|| {
            ExtensionError::Custom {
                name: self.manifest.name.clone(),
                message: "manifest missing `exec` for Tier 2 extension".to_string(),
            }
        })?;
        let (program, args) = exec.split_first().ok_or_else(|| ExtensionError::Custom {
            name: self.manifest.name.clone(),
            message: "manifest `exec` must have at least one element".to_string(),
        })?;

        let mut cmd = Command::new(program);
        cmd.args(args)
            .current_dir(&self.extension_dir)
            .envs(self.manifest.env.iter())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        if let Some(dir) = data_dir {
            // Best-effort directory creation. If this fails the subprocess
            // can still run; it just won't have the dir pre-created.
            let _ = std::fs::create_dir_all(dir);
            cmd.env("HAND_DATA_DIR", dir);
        }

        let mut child = cmd.spawn().map_err(|e| ExtensionError::Rpc {
            extension: self.manifest.name.clone(),
            source: Box::new(e),
        })?;

        let stdin = child.stdin.take().ok_or_else(|| ExtensionError::Custom {
            name: self.manifest.name.clone(),
            message: "subprocess stdin not captured".to_string(),
        })?;
        let stdout = child.stdout.take().ok_or_else(|| ExtensionError::Custom {
            name: self.manifest.name.clone(),
            message: "subprocess stdout not captured".to_string(),
        })?;

        let (tx, rx) = mpsc::channel::<Result<ExtensionEventIn, ExtensionError>>(8);
        let extension_name = self.manifest.name.clone();
        tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut stream = Box::pin(read_jsonl::<_, ExtensionEventIn>(reader));
            while let Some(item) = stream.next().await {
                let mapped = item.map_err(|e: JsonlReadError| ExtensionError::Rpc {
                    extension: extension_name.clone(),
                    source: Box::new(e),
                });
                if tx.send(mapped).await.is_err() {
                    break;
                }
            }
        });

        Ok(SubprocessHandle {
            child,
            stdin,
            stdout_rx: rx,
        })
    }

    /// Send one event and read one response. Spawns the subprocess if it
    /// is not already running.
    async fn rpc(
        &self,
        event: ExtensionEventOut,
    ) -> Result<ExtensionEventIn, ExtensionError> {
        let mut guard = self.child.lock().await;
        if guard.is_none() {
            // Extract `data_dir` from the event's embedded context (every
            // `ExtensionEventOut` variant carries one) so the subprocess
            // sees a stable `HAND_DATA_DIR` for the rest of its lifetime.
            let data_dir = event_data_dir(&event);
            *guard = Some(self.spawn_locked(data_dir)?);
        }
        let handle = guard.as_mut().expect("just-spawned handle present");

        // Write the event as one JSONL frame.
        if let Err(e) = write_jsonl(&mut handle.stdin, &event).await {
            // If the write fails, drop the handle so a future call can try
            // to respawn rather than reuse a half-dead child.
            *guard = None;
            return Err(ExtensionError::Rpc {
                extension: self.manifest.name.clone(),
                source: Box::new(e),
            });
        }

        // Read one frame back.
        match handle.stdout_rx.recv().await {
            Some(Ok(frame)) => Ok(frame),
            Some(Err(e)) => {
                *guard = None;
                Err(e)
            }
            None => {
                // Subprocess closed stdout (likely exited or crashed).
                *guard = None;
                Err(ExtensionError::Custom {
                    name: self.manifest.name.clone(),
                    message: "subprocess closed stdout before responding".to_string(),
                })
            }
        }
    }
}

#[async_trait]
impl Extension for SubprocessExtension {
    fn manifest(&self) -> &ExtensionManifest {
        &self.inner.manifest
    }

    async fn on_load(&self, cx: &ExtensionContext) -> Result<(), ExtensionError> {
        let response = self
            .inner
            .rpc(ExtensionEventOut::OnLoad {
                context: cx.into(),
            })
            .await?;
        match response {
            ExtensionEventIn::Ok | ExtensionEventIn::Continue => Ok(()),
            ExtensionEventIn::Error { message } => Err(ExtensionError::Custom {
                name: self.inner.manifest.name.clone(),
                message,
            }),
            _ => Err(ExtensionError::Custom {
                name: self.inner.manifest.name.clone(),
                message: "unexpected response shape for on_load".to_string(),
            }),
        }
    }

    async fn on_shutdown(&self, cx: &ExtensionContext) -> Result<(), ExtensionError> {
        // Best-effort: send shutdown event, ignore the response, then kill
        // the process. A subprocess that has already exited or hangs on
        // shutdown must not block session teardown.
        let _ = self
            .inner
            .rpc(ExtensionEventOut::OnShutdown {
                context: cx.into(),
            })
            .await;
        let mut guard = self.inner.child.lock().await;
        if let Some(mut handle) = guard.take() {
            // `kill_on_drop` would also handle this, but be explicit so the
            // process is reaped before we return.
            let _ = handle.child.kill().await;
        }
        Ok(())
    }

    async fn on_before_tool_call(
        &self,
        cx: &ExtensionContext,
        event: &ToolCallEvent,
    ) -> Result<HookDecision, ExtensionError> {
        let response = self
            .inner
            .rpc(ExtensionEventOut::OnBeforeToolCall {
                context: cx.into(),
                event: event.into(),
            })
            .await?;
        match response {
            ExtensionEventIn::Continue => Ok(HookDecision::Continue),
            ExtensionEventIn::Cancel { reason } => Ok(HookDecision::Cancel(reason)),
            ExtensionEventIn::Replace { arguments } => Ok(HookDecision::Replace(arguments)),
            ExtensionEventIn::Error { message } => Err(ExtensionError::Custom {
                name: self.inner.manifest.name.clone(),
                message,
            }),
            ExtensionEventIn::Ok => Err(ExtensionError::Custom {
                name: self.inner.manifest.name.clone(),
                message: "unexpected `ok` response for on_before_tool_call".to_string(),
            }),
            _ => Err(ExtensionError::Custom {
                name: self.inner.manifest.name.clone(),
                message: "unexpected response shape for on_before_tool_call".to_string(),
            }),
        }
    }

    async fn on_after_tool_call(
        &self,
        cx: &ExtensionContext,
        event: &ToolResultEvent,
    ) -> Result<(), ExtensionError> {
        let response = self
            .inner
            .rpc(ExtensionEventOut::OnAfterToolCall {
                context: cx.into(),
                event: event.into(),
            })
            .await?;
        match response {
            ExtensionEventIn::Ok | ExtensionEventIn::Continue => Ok(()),
            ExtensionEventIn::Error { message } => Err(ExtensionError::Custom {
                name: self.inner.manifest.name.clone(),
                message,
            }),
            _ => Err(ExtensionError::Custom {
                name: self.inner.manifest.name.clone(),
                message: "unexpected response shape for on_after_tool_call".to_string(),
            }),
        }
    }

    fn slash_commands(&self) -> Vec<SlashCommandSpec> {
        self.inner.manifest.slash_commands.clone()
    }

    /// Build [`AgentTool`] entries for every manifest-declared custom tool.
    ///
    /// Each tool's execute closure clones an `Arc<SubprocessInner>` and the
    /// extension context so it can drive an RPC round-trip into the
    /// subprocess from inside the agent loop's tool list.
    fn custom_tools(&self) -> Vec<AgentTool> {
        let mut tools = Vec::with_capacity(self.inner.manifest.custom_tools.len());
        for spec in &self.inner.manifest.custom_tools {
            let inner = self.inner.clone();
            let tool_name = spec.name.clone();
            let parameters = self
                .inner
                .parsed_tool_schemas
                .get(&spec.name)
                .cloned()
                .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}}));

            let execute: ToolExecuteFn =
                Box::new(move |_id, args, exec_cx: ToolExecutionContext| {
                    let inner = inner.clone();
                    let tool_name = tool_name.clone();
                    let fut: BoxFuture<'static, ToolResult> = Box::pin(async move {
                        // Build the `ExtensionContext` from the live session
                        // metadata supplied by the agent loop. `data_dir` is
                        // anchored at the extension's own install directory
                        // so subprocesses can persist per-extension state
                        // without trampling other extensions.
                        let cx = ExtensionContext {
                            cwd: exec_cx.cwd.clone(),
                            session_id: exec_cx.session_id.clone(),
                            data_dir: inner.extension_dir.join("data"),
                        };
                        match inner
                            .rpc(ExtensionEventOut::ExecuteCustomTool {
                                context: (&cx).into(),
                                tool_name: tool_name.clone(),
                                arguments: args,
                                call_id: exec_cx.call_id.clone(),
                            })
                            .await
                        {
                            Ok(ExtensionEventIn::ToolResult { content, is_error }) => {
                                // Preserve the subprocess's `is_error` flag
                                // distinct from the textual content. Using
                                // `ToolResult::text` + an explicit set keeps
                                // the success path's content shape and only
                                // flips the error bit when the subprocess
                                // says so.
                                let mut result = ToolResult::text(content);
                                result.is_error = is_error;
                                result
                            }
                            Ok(ExtensionEventIn::Error { message }) => {
                                ToolResult::error(format!("extension error: {message}"))
                            }
                            Ok(_) => ToolResult::error(
                                "extension returned unexpected response for custom tool",
                            ),
                            Err(e) => ToolResult::error(format!("extension error: {e}")),
                        }
                    });
                    fut
                });

            tools.push(AgentTool::new(
                spec.name.clone(),
                spec.description.clone(),
                parameters,
                spec.name.clone(),
                execute,
            ));
        }
        tools
    }

    async fn handle_slash_command(
        &self,
        cx: &ExtensionContext,
        name: &str,
        args: &str,
    ) -> Result<String, ExtensionError> {
        let response = self
            .inner
            .rpc(ExtensionEventOut::ExecuteSlashCommand {
                context: cx.into(),
                command_name: name.to_string(),
                args: args.to_string(),
            })
            .await?;
        match response {
            ExtensionEventIn::SlashResult { output, error } => {
                if let Some(message) = error {
                    Err(ExtensionError::Custom {
                        name: self.inner.manifest.name.clone(),
                        message,
                    })
                } else {
                    Ok(output)
                }
            }
            ExtensionEventIn::Error { message } => Err(ExtensionError::Custom {
                name: self.inner.manifest.name.clone(),
                message,
            }),
            _ => Err(ExtensionError::Custom {
                name: self.inner.manifest.name.clone(),
                message: "unexpected response shape for slash command".to_string(),
            }),
        }
    }
}

/// Discover all `<root>/<name>/extension.toml` files and return wrapped
/// `SubprocessExtension` instances ready to register, plus per-manifest
/// errors collected separately so a single bad extension does not abort
/// discovery of the others.
///
/// A missing `root` is not an error — returns `(vec![], vec![])`.
pub fn discover_subprocess_extensions(
    root: &Path,
) -> (Vec<Arc<dyn Extension>>, Vec<(PathBuf, ExtensionError)>) {
    let mut extensions: Vec<Arc<dyn Extension>> = Vec::new();
    let mut failures: Vec<(PathBuf, ExtensionError)> = Vec::new();

    let entries = match std::fs::read_dir(root) {
        Ok(it) => it,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (extensions, failures),
        Err(e) => {
            failures.push((root.to_path_buf(), ExtensionError::Io(e)));
            return (extensions, failures);
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let manifest_path = path.join("extension.toml");
        if !manifest_path.is_file() {
            continue;
        }
        match load_manifest(&manifest_path) {
            Ok(manifest) => match SubprocessExtension::new(manifest, path.clone()) {
                Ok(sub) => {
                    let ext: Arc<dyn Extension> = Arc::new(sub);
                    extensions.push(ext);
                }
                Err(err) => {
                    failures.push((manifest_path, err));
                }
            },
            Err(source) => {
                failures.push((
                    manifest_path.clone(),
                    ExtensionError::Manifest {
                        path: manifest_path,
                        source,
                    },
                ));
            }
        }
    }

    (extensions, failures)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn ctx() -> ExtensionContext {
        ExtensionContext {
            cwd: PathBuf::from("/tmp"),
            session_id: "test-session".to_string(),
            data_dir: PathBuf::from("/tmp/data"),
        }
    }

    fn tool_call_event() -> ToolCallEvent {
        ToolCallEvent {
            tool_name: "read".to_string(),
            arguments: serde_json::json!({"path": "/etc/hosts"}),
            call_id: "call-1".to_string(),
        }
    }

    fn tool_result_event() -> ToolResultEvent {
        ToolResultEvent {
            tool_name: "read".to_string(),
            call_id: "call-1".to_string(),
            success: true,
            result: serde_json::json!({"content": "127.0.0.1 localhost"}),
        }
    }

    /// Write a Bash script that for every line on stdin emits one JSONL
    /// response. The script body is provided by the caller; it can branch
    /// on `$line` to vary the response per event type.
    fn write_bash_script(dir: &Path, script: &str) -> PathBuf {
        let script_path = dir.join("ext.sh");
        let body = format!("#!/bin/bash\nset -u\nwhile IFS= read -r line; do\n{script}\ndone\n");
        fs::write(&script_path, body).unwrap();
        // chmod +x
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&script_path).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&script_path, perms).unwrap();
        }
        script_path
    }

    fn make_manifest(name: &str, exec: Vec<String>) -> ExtensionManifest {
        ExtensionManifest {
            name: name.to_string(),
            version: "0.1.0".to_string(),
            description: None,
            capabilities: Default::default(),
            exec: Some(exec),
            env: Default::default(),
            slash_commands: Vec::new(),
            custom_tools: Vec::new(),
        }
    }

    #[test]
    fn discovery_returns_empty_for_missing_dir() {
        let nonexistent = PathBuf::from("/tmp/does-not-exist-coding-agent-discovery");
        // Belt and suspenders: ensure it really doesn't exist.
        let _ = std::fs::remove_dir_all(&nonexistent);
        let (exts, failures) = discover_subprocess_extensions(&nonexistent);
        assert!(exts.is_empty());
        assert!(failures.is_empty());
    }

    #[test]
    fn discovery_parses_one_valid_extension_toml() {
        let dir = TempDir::new().unwrap();
        let foo_dir = dir.path().join("foo");
        fs::create_dir(&foo_dir).unwrap();
        fs::write(
            foo_dir.join("extension.toml"),
            r#"
name = "foo"
version = "0.1"
exec = ["/bin/true"]
"#,
        )
        .unwrap();
        let (exts, failures) = discover_subprocess_extensions(dir.path());
        assert_eq!(exts.len(), 1);
        assert!(failures.is_empty());
        assert_eq!(exts[0].manifest().name, "foo");
    }

    #[test]
    fn discovery_handles_malformed_manifest() {
        let dir = TempDir::new().unwrap();
        let bar_dir = dir.path().join("bar");
        fs::create_dir(&bar_dir).unwrap();
        fs::write(
            bar_dir.join("extension.toml"),
            "this is = not [ valid toml",
        )
        .unwrap();
        let (exts, failures) = discover_subprocess_extensions(dir.path());
        assert!(exts.is_empty());
        assert_eq!(failures.len(), 1);
        let (path, err) = &failures[0];
        assert!(path.ends_with("extension.toml"));
        assert!(matches!(err, ExtensionError::Manifest { .. }));
    }

    #[tokio::test]
    async fn subprocess_handles_load_before_after() {
        // Script: respond `continue` to OnBeforeToolCall, `ok` to everything
        // else. The host sends one JSON frame per line so we can branch on
        // a substring of `$line`.
        let dir = TempDir::new().unwrap();
        let script = write_bash_script(
            dir.path(),
            r#"
  case "$line" in
    *on_before_tool_call*) printf '{"type":"continue"}\n' ;;
    *) printf '{"type":"ok"}\n' ;;
  esac
"#,
        );
        let manifest = make_manifest("hooky", vec![script.to_string_lossy().into_owned()]);
        let ext = SubprocessExtension::new(manifest, dir.path().to_path_buf())
            .expect("subprocess constructs");

        let cx = ctx();
        ext.on_load(&cx).await.expect("on_load ok");

        let decision = ext
            .on_before_tool_call(&cx, &tool_call_event())
            .await
            .expect("on_before_tool_call ok");
        assert!(matches!(decision, HookDecision::Continue));

        ext.on_after_tool_call(&cx, &tool_result_event())
            .await
            .expect("on_after_tool_call ok");

        ext.on_shutdown(&cx).await.expect("on_shutdown ok");
        // After shutdown the child handle should be cleared.
        let inner = ext.inner_for_test();
        let guard = inner.child.lock().await;
        assert!(guard.is_none(), "child handle cleared on shutdown");
    }

    #[tokio::test]
    async fn subprocess_returns_cancel() {
        let dir = TempDir::new().unwrap();
        let script = write_bash_script(
            dir.path(),
            r#"  printf '{"type":"cancel","reason":"blocked"}\n'"#,
        );
        let manifest = make_manifest("blocker", vec![script.to_string_lossy().into_owned()]);
        let ext = SubprocessExtension::new(manifest, dir.path().to_path_buf())
            .expect("subprocess constructs");

        let decision = ext
            .on_before_tool_call(&ctx(), &tool_call_event())
            .await
            .expect("rpc ok");
        match decision {
            HookDecision::Cancel(reason) => assert_eq!(reason, "blocked"),
            other => panic!("expected Cancel, got {other:?}"),
        }
        let _ = ext.on_shutdown(&ctx()).await;
    }

    #[tokio::test]
    async fn subprocess_returns_replace() {
        let dir = TempDir::new().unwrap();
        let script = write_bash_script(
            dir.path(),
            r#"  printf '{"type":"replace","arguments":{"foo":"bar"}}\n'"#,
        );
        let manifest = make_manifest("rewriter", vec![script.to_string_lossy().into_owned()]);
        let ext = SubprocessExtension::new(manifest, dir.path().to_path_buf())
            .expect("subprocess constructs");

        let decision = ext
            .on_before_tool_call(&ctx(), &tool_call_event())
            .await
            .expect("rpc ok");
        match decision {
            HookDecision::Replace(args) => {
                assert_eq!(args, serde_json::json!({"foo":"bar"}));
            }
            other => panic!("expected Replace, got {other:?}"),
        }
        let _ = ext.on_shutdown(&ctx()).await;
    }

    #[tokio::test]
    async fn subprocess_malformed_response_yields_error() {
        // Script: emit garbage that is not JSON, then exit. The reader task
        // will yield a parse error, and the recv on the host side should
        // surface it as ExtensionError::Rpc.
        let dir = TempDir::new().unwrap();
        let script_path = dir.path().join("ext.sh");
        fs::write(
            &script_path,
            "#!/bin/bash\nread -r _\nprintf 'this is not json\\n'\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&script_path).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&script_path, perms).unwrap();
        }
        let manifest = make_manifest("broken", vec![script_path.to_string_lossy().into_owned()]);
        let ext = SubprocessExtension::new(manifest, dir.path().to_path_buf())
            .expect("subprocess constructs");

        let err = ext
            .on_before_tool_call(&ctx(), &tool_call_event())
            .await
            .expect_err("malformed response should error");
        // Either Rpc (parse error from jsonl) or Custom (closed stdout) is
        // acceptable as long as the error is reported, not silently swallowed.
        match err {
            ExtensionError::Rpc { .. } | ExtensionError::Custom { .. } => {}
            other => panic!("expected Rpc or Custom, got {other:?}"),
        }
        let _ = ext.on_shutdown(&ctx()).await;
    }

    #[tokio::test]
    async fn subprocess_explicit_error_response() {
        let dir = TempDir::new().unwrap();
        let script = write_bash_script(
            dir.path(),
            r#"  printf '{"type":"error","message":"boom"}\n'"#,
        );
        let manifest = make_manifest("erroring", vec![script.to_string_lossy().into_owned()]);
        let ext = SubprocessExtension::new(manifest, dir.path().to_path_buf())
            .expect("subprocess constructs");

        let err = ext
            .on_before_tool_call(&ctx(), &tool_call_event())
            .await
            .expect_err("explicit error should propagate");
        match err {
            ExtensionError::Custom { name, message } => {
                assert_eq!(name, "erroring");
                assert_eq!(message, "boom");
            }
            other => panic!("expected Custom, got {other:?}"),
        }
        let _ = ext.on_shutdown(&ctx()).await;
    }

    // ----------------------------------------------------------------------
    // T3.5 — manifest-driven slash commands and custom tools
    // ----------------------------------------------------------------------

    use crate::core::extensions::api::{CustomToolSpec, SlashCommandSpec};

    /// Tier-2 manifest-declared slash commands surface via `slash_commands()`.
    #[test]
    fn tier2_slash_commands_returned_from_manifest() {
        let dir = TempDir::new().unwrap();
        let mut manifest = make_manifest("greeter", vec!["/bin/true".into()]);
        manifest.slash_commands = vec![
            SlashCommandSpec {
                name: "greet".into(),
                description: "Greet the user".into(),
                usage: Some("/greet [name]".into()),
            },
            SlashCommandSpec {
                name: "wave".into(),
                description: "Wave hello".into(),
                usage: None,
            },
        ];
        let ext = SubprocessExtension::new(manifest, dir.path().to_path_buf())
            .expect("subprocess constructs");

        let cmds = ext.slash_commands();
        assert_eq!(cmds.len(), 2);
        assert_eq!(cmds[0].name, "greet");
        assert_eq!(cmds[1].name, "wave");
    }

    /// Construction fails when a custom tool's schema string isn't valid JSON.
    #[test]
    fn tier2_custom_tool_invalid_schema_fails_to_load() {
        let dir = TempDir::new().unwrap();
        let mut manifest = make_manifest("bad-schema", vec!["/bin/true".into()]);
        manifest.custom_tools = vec![CustomToolSpec {
            name: "broken".into(),
            description: "Broken tool".into(),
            schema: "this is not json".into(),
        }];
        let result = SubprocessExtension::new(manifest, dir.path().to_path_buf());
        match result {
            Err(ExtensionError::Custom { name, message }) => {
                assert_eq!(name, "bad-schema");
                assert!(
                    message.contains("schema is not valid JSON"),
                    "unexpected message: {message}"
                );
            }
            Err(other) => panic!("expected Custom error, got {other:?}"),
            Ok(_) => panic!("invalid schema should reject load"),
        }
    }

    /// Tier-2 custom tool round-trip: the AgentTool returned by
    /// `custom_tools()` drives an RPC into the subprocess and converts the
    /// response into a `ToolResult`.
    #[tokio::test]
    async fn tier2_custom_tool_round_trip_via_subprocess() {
        let dir = TempDir::new().unwrap();
        let script = write_bash_script(
            dir.path(),
            r#"  printf '{"type":"tool_result","content":"hello","is_error":false}\n'"#,
        );
        let mut manifest = make_manifest(
            "rust-checker",
            vec![script.to_string_lossy().into_owned()],
        );
        manifest.custom_tools = vec![CustomToolSpec {
            name: "rust_check".into(),
            description: "Run cargo check".into(),
            schema: r#"{"type":"object","properties":{"package":{"type":"string"}}}"#.into(),
        }];
        let ext = SubprocessExtension::new(manifest, dir.path().to_path_buf())
            .expect("subprocess constructs");

        let tools = ext.custom_tools();
        assert_eq!(tools.len(), 1);
        let tool = &tools[0];
        assert_eq!(tool.name, "rust_check");
        // Schema parsed at load time and round-trips through AgentTool.
        assert_eq!(tool.parameters["type"], "object");

        let cx = ToolExecutionContext {
            cwd: PathBuf::from("/tmp"),
            session_id: "test-session".into(),
            call_id: "call-1".into(),
        };
        let result = (tool.execute)("call-1".into(), serde_json::json!({}), cx).await;
        // Successful result: text content "hello".
        let mut found = false;
        for block in &result.content {
            if let model::ToolResultContent::Text(t) = block
                && t.text == "hello"
            {
                found = true;
            }
        }
        assert!(
            found,
            "expected `hello` text content; got {:?}",
            result.content
        );

        let _ = ext.on_shutdown(&ctx()).await;
    }

    /// F23 — Tier-2 subprocess that returns `is_error: true` propagates the
    /// error flag into the resulting `ToolResult`. The content text is
    /// preserved unchanged.
    #[tokio::test]
    async fn tier2_subprocess_is_error_propagates() {
        let dir = TempDir::new().unwrap();
        let script = write_bash_script(
            dir.path(),
            r#"  printf '{"type":"tool_result","content":"compile failed","is_error":true}\n'"#,
        );
        let mut manifest =
            make_manifest("erroring", vec![script.to_string_lossy().into_owned()]);
        manifest.custom_tools = vec![CustomToolSpec {
            name: "rust_check".into(),
            description: "Run cargo check".into(),
            schema: r#"{"type":"object","properties":{}}"#.into(),
        }];
        let ext = SubprocessExtension::new(manifest, dir.path().to_path_buf())
            .expect("subprocess constructs");

        let tools = ext.custom_tools();
        let tool = &tools[0];
        let cx = ToolExecutionContext {
            cwd: PathBuf::from("/tmp"),
            session_id: "test-session".into(),
            call_id: "call-1".into(),
        };
        let result = (tool.execute)("call-1".into(), serde_json::json!({}), cx).await;

        assert!(
            result.is_error,
            "is_error=true from subprocess must propagate into ToolResult.is_error"
        );
        // Content text is preserved.
        let mut text = String::new();
        for block in &result.content {
            if let model::ToolResultContent::Text(t) = block {
                text = t.text.clone();
            }
        }
        assert_eq!(text, "compile failed");

        let _ = ext.on_shutdown(&ctx()).await;
    }

    /// F23 — Tier-2 custom tool execute closure receives the live session's
    /// `cwd` / `session_id` (forwarded via `ToolExecutionContext`), not the
    /// `<ext:.../no-cwd>` sentinel that earlier versions of the host
    /// synthesized. The fixture script echoes the inbound JSON line back so
    /// the test can parse the embedded `context` and assert the values.
    #[tokio::test]
    async fn tier2_custom_tool_receives_real_session_context() {
        let dir = TempDir::new().unwrap();
        // Echo the inbound line as a tool_result whose `content` is the
        // original event JSON. The host then parses that content and asserts
        // the embedded context fields.
        let script_path = dir.path().join("ext.sh");
        let body = r#"#!/bin/bash
set -u
while IFS= read -r line; do
  # Build a tool_result whose content is the inbound line (escaped).
  python3 -c '
import json, sys
line = sys.argv[1]
print(json.dumps({"type":"tool_result","content":line,"is_error":False}))
' "$line"
done
"#;
        std::fs::write(&script_path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script_path, perms).unwrap();
        }

        let mut manifest = make_manifest(
            "context-echo",
            vec![script_path.to_string_lossy().into_owned()],
        );
        manifest.custom_tools = vec![CustomToolSpec {
            name: "echo_ctx".into(),
            description: "Echo context".into(),
            schema: r#"{"type":"object","properties":{}}"#.into(),
        }];
        let ext = SubprocessExtension::new(manifest, dir.path().to_path_buf())
            .expect("subprocess constructs");

        let tools = ext.custom_tools();
        let tool = &tools[0];

        let real_cwd = PathBuf::from("/the/real/cwd");
        let real_session = "s_real_123".to_string();
        let cx = ToolExecutionContext {
            cwd: real_cwd.clone(),
            session_id: real_session.clone(),
            call_id: "call-xyz".into(),
        };
        let result = (tool.execute)("call-xyz".into(), serde_json::json!({}), cx).await;

        assert!(
            !result.is_error,
            "context-echo is_error must remain false: {:?}",
            result
        );

        let mut echoed = String::new();
        for block in &result.content {
            if let model::ToolResultContent::Text(t) = block {
                echoed = t.text.clone();
            }
        }
        // The echoed line is the inbound `ExtensionEventOut::ExecuteCustomTool`
        // event with its embedded `context` DTO. Parse and verify the cwd
        // and session_id match the live session — NOT the old
        // `<ext:.../no-cwd>` / `<ext:.../no-session>` sentinels.
        let event: serde_json::Value =
            serde_json::from_str(&echoed).expect("echoed line is JSON");
        let context = event
            .get("context")
            .expect("event carries context")
            .clone();
        assert_eq!(
            context.get("cwd").and_then(|v| v.as_str()),
            Some(real_cwd.to_str().unwrap()),
            "subprocess must see the live cwd, not a sentinel"
        );
        assert_eq!(
            context.get("sessionId").and_then(|v| v.as_str()),
            Some(real_session.as_str()),
            "subprocess must see the live session_id, not a sentinel"
        );
        let cwd_str = context.get("cwd").and_then(|v| v.as_str()).unwrap_or("");
        assert!(
            !cwd_str.contains("no-cwd"),
            "cwd must not be a `<ext:.../no-cwd>` sentinel; got {cwd_str:?}"
        );

        let _ = ext.on_shutdown(&ctx()).await;
    }

    /// Tier-2 slash command round-trip: `handle_slash_command` issues an
    /// RPC and surfaces the subprocess's `slash_result.output`.
    #[tokio::test]
    async fn tier2_slash_command_round_trip_via_subprocess() {
        let dir = TempDir::new().unwrap();
        let script = write_bash_script(
            dir.path(),
            r#"  printf '{"type":"slash_result","output":"done"}\n'"#,
        );
        let mut manifest = make_manifest(
            "slasher",
            vec![script.to_string_lossy().into_owned()],
        );
        manifest.slash_commands = vec![SlashCommandSpec {
            name: "review".into(),
            description: "Review code".into(),
            usage: None,
        }];
        let ext = SubprocessExtension::new(manifest, dir.path().to_path_buf())
            .expect("subprocess constructs");

        let output = ext
            .handle_slash_command(&ctx(), "review", "src/")
            .await
            .expect("slash command ok");
        assert_eq!(output, "done");

        let _ = ext.on_shutdown(&ctx()).await;
    }
}
