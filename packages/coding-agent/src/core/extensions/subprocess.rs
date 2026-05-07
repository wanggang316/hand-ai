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
pub struct SubprocessExtension {
    manifest: ExtensionManifest,
    /// Path to the directory containing `extension.toml`. Used as the cwd
    /// for the subprocess so relative `exec` paths resolve correctly.
    extension_dir: PathBuf,
    /// Lazy-spawned child process. Mutex-guarded for hook serialization.
    child: Mutex<Option<SubprocessHandle>>,
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
    pub fn new(manifest: ExtensionManifest, extension_dir: PathBuf) -> Self {
        SubprocessExtension {
            manifest,
            extension_dir,
            child: Mutex::new(None),
        }
    }

    /// Spawn the subprocess. Caller must hold the child mutex and have
    /// observed `None`.
    fn spawn_locked(&self) -> Result<SubprocessHandle, ExtensionError> {
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
            *guard = Some(self.spawn_locked()?);
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
        &self.manifest
    }

    async fn on_load(&self, cx: &ExtensionContext) -> Result<(), ExtensionError> {
        let response = self
            .rpc(ExtensionEventOut::OnLoad {
                context: cx.into(),
            })
            .await?;
        match response {
            ExtensionEventIn::Ok | ExtensionEventIn::Continue => Ok(()),
            ExtensionEventIn::Error { message } => Err(ExtensionError::Custom {
                name: self.manifest.name.clone(),
                message,
            }),
            _ => Err(ExtensionError::Custom {
                name: self.manifest.name.clone(),
                message: "unexpected response shape for on_load".to_string(),
            }),
        }
    }

    async fn on_shutdown(&self, cx: &ExtensionContext) -> Result<(), ExtensionError> {
        // Best-effort: send shutdown event, ignore the response, then kill
        // the process. A subprocess that has already exited or hangs on
        // shutdown must not block session teardown.
        let _ = self
            .rpc(ExtensionEventOut::OnShutdown {
                context: cx.into(),
            })
            .await;
        let mut guard = self.child.lock().await;
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
                name: self.manifest.name.clone(),
                message,
            }),
            ExtensionEventIn::Ok => Err(ExtensionError::Custom {
                name: self.manifest.name.clone(),
                message: "unexpected `ok` response for on_before_tool_call".to_string(),
            }),
        }
    }

    async fn on_after_tool_call(
        &self,
        cx: &ExtensionContext,
        event: &ToolResultEvent,
    ) -> Result<(), ExtensionError> {
        let response = self
            .rpc(ExtensionEventOut::OnAfterToolCall {
                context: cx.into(),
                event: event.into(),
            })
            .await?;
        match response {
            ExtensionEventIn::Ok | ExtensionEventIn::Continue => Ok(()),
            ExtensionEventIn::Error { message } => Err(ExtensionError::Custom {
                name: self.manifest.name.clone(),
                message,
            }),
            _ => Err(ExtensionError::Custom {
                name: self.manifest.name.clone(),
                message: "unexpected response shape for on_after_tool_call".to_string(),
            }),
        }
    }

    fn slash_commands(&self) -> Vec<SlashCommandSpec> {
        // T3.4 will wire `manifest.slash_commands` once that field exists.
        Vec::new()
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
            Ok(manifest) => {
                let ext: Arc<dyn Extension> =
                    Arc::new(SubprocessExtension::new(manifest, path.clone()));
                extensions.push(ext);
            }
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
        let ext = SubprocessExtension::new(manifest, dir.path().to_path_buf());

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
        let guard = ext.child.lock().await;
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
        let ext = SubprocessExtension::new(manifest, dir.path().to_path_buf());

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
        let ext = SubprocessExtension::new(manifest, dir.path().to_path_buf());

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
        let ext = SubprocessExtension::new(manifest, dir.path().to_path_buf());

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
        let ext = SubprocessExtension::new(manifest, dir.path().to_path_buf());

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
}
