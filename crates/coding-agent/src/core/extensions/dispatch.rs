//! Hook dispatcher for the Tier 1 extension chain.
//!
//! Aggregates `Vec<Arc<dyn Extension>>` into a single decision per hook
//! invocation. The ordering and aggregation rules are documented on each
//! function and exercised by the tests at the bottom of this file.
//!
//! See ADR-001 and Phase 3 task T3.3a for the design.

use super::api::{Extension, ExtensionContextFactory, HookDecision, ToolCallEvent, ToolResultEvent};
use std::sync::Arc;

/// How many passes over the chain `dispatch_before_tool_call` will make before
/// it gives up and cancels the call. One pass produces a rewrite, the next
/// re-validates it; a chain that keeps rewriting on every pass never converges
/// and is treated as a fault.
pub(crate) const MAX_BEFORE_TOOL_CALL_ROUNDS: usize = 3;

/// Dispatch `on_before_tool_call` across an ordered chain of extensions.
///
/// # Aggregation rules
///
/// Extensions are called sequentially in the order they appear in
/// `extensions`. Per extension:
///
/// - `Continue` — args are unchanged; continue to the next extension.
/// - `Replace(new_args)` — the working `ToolCallEvent::arguments` is updated to
///   `new_args` so subsequent extensions see the replaced value.
/// - `Cancel(reason)` — the chain short-circuits; no further extensions are
///   called, and `Cancel(reason)` is returned immediately.
/// - `Err(_)` — the error is logged via `tracing::warn!` and treated as
///   `Continue` for that extension. A misbehaving extension never aborts the
///   chain.
///
/// # Re-validation on replace
///
/// A `Replace` invalidates every verdict already cast in this pass: an
/// extension that answered `Continue` did so for arguments that no longer
/// exist. So whenever a pass rewrites the arguments, the whole chain is run
/// again from the head with the rewritten value, giving earlier extensions
/// (path guards, approval gates) a chance to inspect what will actually reach
/// the tool. Without this, a later extension could rewrite arguments past a
/// guard registered ahead of it.
///
/// A `Replace` whose value equals the current working arguments is a no-op and
/// does not trigger another pass, so idempotent rewriters (path canonicalizers
/// and the like) converge on the second pass instead of burning the budget.
///
/// The chain is re-run at most [`MAX_BEFORE_TOOL_CALL_ROUNDS`] times. A chain
/// that still rewrites on the last pass has not converged and the call is
/// cancelled — an unbounded rewrite loop is a fault, and failing closed keeps
/// an un-validated argument set from reaching the tool.
///
/// The returned `HookDecision`:
/// - `Continue` — every extension returned `Continue` (or errored);
///   `event.arguments` is unchanged from the caller's input.
/// - `Replace(final_args)` — the chain converged on `final_args` and every
///   extension has seen that value.
/// - `Cancel(reason)` — some extension cancelled, or the chain did not
///   converge.
pub(crate) async fn dispatch_before_tool_call(
    extensions: &[Arc<dyn Extension>],
    contexts: &ExtensionContextFactory,
    event: &ToolCallEvent,
) -> HookDecision {
    let mut working = event.clone();
    let mut replaced: Option<serde_json::Value> = None;

    for _round in 0..MAX_BEFORE_TOOL_CALL_ROUNDS {
        let mut replaced_this_round = false;

        for ext in extensions {
            let cx = contexts.for_extension(&ext.manifest().name);
            let decision = match ext.on_before_tool_call(&cx, &working).await {
                Ok(d) => d,
                Err(err) => {
                    tracing::warn!(
                        extension = %ext.manifest().name,
                        error = %err,
                        "extension on_before_tool_call errored; treating as Continue"
                    );
                    HookDecision::Continue
                }
            };

            match decision {
                HookDecision::Continue => {}
                HookDecision::Replace(new_args) => {
                    if new_args == working.arguments {
                        // Idempotent rewrite: nothing changed, so nothing
                        // needs re-validating.
                        continue;
                    }
                    working.arguments = new_args.clone();
                    replaced = Some(new_args);
                    replaced_this_round = true;
                }
                HookDecision::Cancel(reason) => {
                    return HookDecision::Cancel(reason);
                }
            }
        }

        if !replaced_this_round {
            return match replaced {
                Some(args) => HookDecision::Replace(args),
                None => HookDecision::Continue,
            };
        }
    }

    tracing::warn!(
        tool = %event.tool_name,
        rounds = MAX_BEFORE_TOOL_CALL_ROUNDS,
        "extension chain kept rewriting tool arguments; cancelling the call"
    );
    HookDecision::Cancel(format!(
        "extension chain did not converge on tool arguments after \
         {MAX_BEFORE_TOOL_CALL_ROUNDS} rounds of rewriting"
    ))
}

/// Dispatch `on_after_tool_call` across an ordered chain of extensions.
///
/// Every extension is called with the same `event` in registration order.
/// Errors are logged and ignored; one misbehaving extension never prevents
/// the rest from running. There is no aggregated return value — `after`
/// hooks are observational only.
pub(crate) async fn dispatch_after_tool_call(
    extensions: &[Arc<dyn Extension>],
    contexts: &ExtensionContextFactory,
    event: &ToolResultEvent,
) {
    for ext in extensions {
        let cx = contexts.for_extension(&ext.manifest().name);
        if let Err(err) = ext.on_after_tool_call(&cx, event).await {
            tracing::warn!(
                extension = %ext.manifest().name,
                error = %err,
                "extension on_after_tool_call errored; ignoring"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::extensions::api::{
        Extension, ExtensionCapabilities, ExtensionContext, ExtensionError, ExtensionManifest,
    };
    use async_trait::async_trait;
    use std::path::PathBuf;
    use std::sync::Mutex;

    fn manifest(name: &str) -> ExtensionManifest {
        ExtensionManifest {
            name: name.into(),
            version: "0.1.0".into(),
            description: None,
            capabilities: ExtensionCapabilities::default(),
            exec: None,
            env: Default::default(),
            slash_commands: Vec::new(),
            custom_tools: Vec::new(),
        }
    }

    fn ctx() -> ExtensionContextFactory {
        ExtensionContextFactory::new(
            PathBuf::from("/tmp"),
            "test-session",
            PathBuf::from("/tmp/.hand/extensions"),
        )
    }

    fn event(args: serde_json::Value) -> ToolCallEvent {
        ToolCallEvent {
            tool_name: "read".into(),
            arguments: args,
            call_id: "call-1".into(),
        }
    }

    /// What a `RecordingExt` should do for each invocation.
    enum BeforeAction {
        Continue,
        Replace(serde_json::Value),
        Cancel(String),
        Error(String),
    }

    /// A test extension that records every event it sees and returns a
    /// caller-configured response (Continue / Replace / Cancel / Error) per
    /// call.
    struct RecordingExt {
        manifest: ExtensionManifest,
        before_actions: Mutex<Vec<BeforeAction>>,
        before_calls: Mutex<Vec<ToolCallEvent>>,
        after_actions: Mutex<Vec<Result<(), String>>>,
        after_calls: Mutex<Vec<ToolResultEvent>>,
    }

    impl RecordingExt {
        fn new(name: &str, before: Vec<BeforeAction>) -> Self {
            Self {
                manifest: manifest(name),
                before_actions: Mutex::new(before),
                before_calls: Mutex::new(Vec::new()),
                after_actions: Mutex::new(Vec::new()),
                after_calls: Mutex::new(Vec::new()),
            }
        }

        fn with_after(mut self, after: Vec<Result<(), String>>) -> Self {
            self.after_actions = Mutex::new(after);
            self
        }
    }

    #[async_trait]
    impl Extension for RecordingExt {
        fn manifest(&self) -> &ExtensionManifest {
            &self.manifest
        }

        async fn on_before_tool_call(
            &self,
            _cx: &ExtensionContext,
            event: &ToolCallEvent,
        ) -> Result<HookDecision, ExtensionError> {
            self.before_calls.lock().unwrap().push(event.clone());
            let mut actions = self.before_actions.lock().unwrap();
            if actions.is_empty() {
                return Ok(HookDecision::Continue);
            }
            match actions.remove(0) {
                BeforeAction::Continue => Ok(HookDecision::Continue),
                BeforeAction::Replace(v) => Ok(HookDecision::Replace(v)),
                BeforeAction::Cancel(reason) => Ok(HookDecision::Cancel(reason)),
                BeforeAction::Error(msg) => Err(ExtensionError::Custom {
                    name: self.manifest.name.clone(),
                    message: msg,
                }),
            }
        }

        async fn on_after_tool_call(
            &self,
            _cx: &ExtensionContext,
            event: &ToolResultEvent,
        ) -> Result<(), ExtensionError> {
            self.after_calls.lock().unwrap().push(event.clone());
            let mut actions = self.after_actions.lock().unwrap();
            if actions.is_empty() {
                return Ok(());
            }
            match actions.remove(0) {
                Ok(()) => Ok(()),
                Err(msg) => Err(ExtensionError::Custom {
                    name: self.manifest.name.clone(),
                    message: msg,
                }),
            }
        }
    }

    // -- Tests for dispatch_before_tool_call ---------------------------------

    #[tokio::test]
    async fn no_extensions_returns_continue() {
        let exts: Vec<Arc<dyn Extension>> = Vec::new();
        let decision =
            dispatch_before_tool_call(&exts, &ctx(), &event(serde_json::json!({}))).await;
        assert!(matches!(decision, HookDecision::Continue));
    }

    #[tokio::test]
    async fn single_continue_returns_continue() {
        let ext = Arc::new(RecordingExt::new("a", vec![BeforeAction::Continue]));
        let exts: Vec<Arc<dyn Extension>> = vec![ext.clone()];
        let decision =
            dispatch_before_tool_call(&exts, &ctx(), &event(serde_json::json!({}))).await;
        assert!(matches!(decision, HookDecision::Continue));
        assert_eq!(ext.before_calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn cancel_short_circuits_chain() {
        let a = Arc::new(RecordingExt::new("a", vec![BeforeAction::Continue]));
        let b = Arc::new(RecordingExt::new(
            "b",
            vec![BeforeAction::Cancel("nope".into())],
        ));
        let c = Arc::new(RecordingExt::new("c", vec![BeforeAction::Continue]));

        let exts: Vec<Arc<dyn Extension>> = vec![a.clone(), b.clone(), c.clone()];
        let decision =
            dispatch_before_tool_call(&exts, &ctx(), &event(serde_json::json!({}))).await;

        match decision {
            HookDecision::Cancel(reason) => assert_eq!(reason, "nope"),
            other => panic!("expected Cancel, got {other:?}"),
        }
        assert_eq!(a.before_calls.lock().unwrap().len(), 1);
        assert_eq!(b.before_calls.lock().unwrap().len(), 1);
        assert_eq!(
            c.before_calls.lock().unwrap().len(),
            0,
            "extension after Cancel must not be called"
        );
    }

    #[tokio::test]
    async fn replace_propagates_to_next_extension_and_last_replace_wins() {
        let args1 = serde_json::json!({"step": 1});
        let args2 = serde_json::json!({"step": 2});
        let a = Arc::new(RecordingExt::new(
            "a",
            vec![BeforeAction::Replace(args1.clone())],
        ));
        let b = Arc::new(RecordingExt::new(
            "b",
            vec![BeforeAction::Replace(args2.clone())],
        ));
        let c = Arc::new(RecordingExt::new("c", vec![BeforeAction::Continue]));

        let exts: Vec<Arc<dyn Extension>> = vec![a.clone(), b.clone(), c.clone()];
        let decision =
            dispatch_before_tool_call(&exts, &ctx(), &event(serde_json::json!({"step": 0}))).await;

        match decision {
            HookDecision::Replace(v) => assert_eq!(v, args2),
            other => panic!("expected Replace, got {other:?}"),
        }

        // b saw args1 in its event.arguments
        let b_calls = b.before_calls.lock().unwrap();
        assert_eq!(b_calls[0].arguments, args1);
        // c saw args2 in its event.arguments
        let c_calls = c.before_calls.lock().unwrap();
        assert_eq!(c_calls[0].arguments, args2);
    }

    // -- Re-validation after a Replace ---------------------------------------

    /// An extension that cancels whenever the working arguments carry a
    /// `path` outside `/workspace`. Stateless, so it answers the same way
    /// on every pass over the chain.
    struct PathGuardExt {
        manifest: ExtensionManifest,
        calls: Mutex<Vec<ToolCallEvent>>,
    }

    impl PathGuardExt {
        fn new(name: &str) -> Self {
            Self {
                manifest: manifest(name),
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl Extension for PathGuardExt {
        fn manifest(&self) -> &ExtensionManifest {
            &self.manifest
        }

        async fn on_before_tool_call(
            &self,
            _cx: &ExtensionContext,
            event: &ToolCallEvent,
        ) -> Result<HookDecision, ExtensionError> {
            self.calls.lock().unwrap().push(event.clone());
            let path = event.arguments.get("path").and_then(|v| v.as_str());
            match path {
                Some(p) if !p.starts_with("/workspace") => {
                    Ok(HookDecision::Cancel(format!("path {p} is outside /workspace")))
                }
                _ => Ok(HookDecision::Continue),
            }
        }
    }

    /// An extension that rewrites the arguments to a fresh value on every
    /// call, so the chain can never converge.
    struct NeverConvergingExt {
        manifest: ExtensionManifest,
        calls: Mutex<usize>,
    }

    impl NeverConvergingExt {
        fn new(name: &str) -> Self {
            Self {
                manifest: manifest(name),
                calls: Mutex::new(0),
            }
        }
    }

    #[async_trait]
    impl Extension for NeverConvergingExt {
        fn manifest(&self) -> &ExtensionManifest {
            &self.manifest
        }

        async fn on_before_tool_call(
            &self,
            _cx: &ExtensionContext,
            _event: &ToolCallEvent,
        ) -> Result<HookDecision, ExtensionError> {
            let mut n = self.calls.lock().unwrap();
            *n += 1;
            Ok(HookDecision::Replace(serde_json::json!({ "round": *n })))
        }
    }

    /// The security case from the issue: a guard registered ahead of a
    /// rewriting extension must still see — and be able to veto — the
    /// arguments that would actually reach the tool.
    #[tokio::test]
    async fn replace_reruns_chain_so_an_earlier_guard_sees_the_final_args() {
        let guard = Arc::new(PathGuardExt::new("guard"));
        let rewriter = Arc::new(RecordingExt::new(
            "rewriter",
            vec![BeforeAction::Replace(
                serde_json::json!({"path": "/etc/passwd"}),
            )],
        ));

        let exts: Vec<Arc<dyn Extension>> = vec![guard.clone(), rewriter.clone()];
        let decision = dispatch_before_tool_call(
            &exts,
            &ctx(),
            &event(serde_json::json!({"path": "/workspace/notes.md"})),
        )
        .await;

        match decision {
            HookDecision::Cancel(reason) => assert!(
                reason.contains("/etc/passwd"),
                "cancel reason should name the rewritten path, got {reason:?}"
            ),
            other => panic!("expected Cancel, got {other:?}"),
        }

        let guard_calls = guard.calls.lock().unwrap();
        assert_eq!(guard_calls.len(), 2, "guard is re-consulted after a replace");
        assert_eq!(
            guard_calls[1].arguments,
            serde_json::json!({"path": "/etc/passwd"}),
            "the second consultation carries the rewritten arguments"
        );
    }

    #[tokio::test]
    async fn converging_replace_calls_each_extension_at_most_twice() {
        let replacement = serde_json::json!({"path": "/workspace/rewritten.md"});
        let a = Arc::new(RecordingExt::new(
            "a",
            vec![BeforeAction::Replace(replacement.clone())],
        ));
        let b = Arc::new(RecordingExt::new("b", vec![BeforeAction::Continue]));

        let exts: Vec<Arc<dyn Extension>> = vec![a.clone(), b.clone()];
        let decision = dispatch_before_tool_call(
            &exts,
            &ctx(),
            &event(serde_json::json!({"path": "/workspace/notes.md"})),
        )
        .await;

        match decision {
            HookDecision::Replace(v) => assert_eq!(v, replacement),
            other => panic!("expected Replace, got {other:?}"),
        }
        assert_eq!(a.before_calls.lock().unwrap().len(), 2);
        assert_eq!(b.before_calls.lock().unwrap().len(), 2);
    }

    /// A `Replace` that yields the value the extension was already handed is
    /// a no-op: it must not cost an extra pass over the chain.
    #[tokio::test]
    async fn idempotent_replace_does_not_trigger_another_round() {
        let same = serde_json::json!({"path": "/workspace/notes.md"});
        let a = Arc::new(RecordingExt::new(
            "a",
            vec![BeforeAction::Replace(same.clone())],
        ));

        let exts: Vec<Arc<dyn Extension>> = vec![a.clone()];
        let decision = dispatch_before_tool_call(&exts, &ctx(), &event(same)).await;

        assert!(matches!(decision, HookDecision::Continue));
        assert_eq!(a.before_calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn non_converging_chain_is_cancelled() {
        let ext = Arc::new(NeverConvergingExt::new("flip-flop"));
        let exts: Vec<Arc<dyn Extension>> = vec![ext.clone()];
        let decision =
            dispatch_before_tool_call(&exts, &ctx(), &event(serde_json::json!({}))).await;

        match decision {
            HookDecision::Cancel(reason) => assert!(
                reason.contains("did not converge"),
                "unexpected cancel reason: {reason:?}"
            ),
            other => panic!("expected Cancel, got {other:?}"),
        }
        assert_eq!(
            *ext.calls.lock().unwrap(),
            MAX_BEFORE_TOOL_CALL_ROUNDS,
            "the chain is bounded to MAX_BEFORE_TOOL_CALL_ROUNDS passes"
        );
    }

    #[tokio::test]
    async fn replace_then_cancel_returns_cancel() {
        let args1 = serde_json::json!({"step": 1});
        let a = Arc::new(RecordingExt::new("a", vec![BeforeAction::Replace(args1)]));
        let b = Arc::new(RecordingExt::new(
            "b",
            vec![BeforeAction::Cancel("blocked".into())],
        ));

        let exts: Vec<Arc<dyn Extension>> = vec![a, b];
        let decision =
            dispatch_before_tool_call(&exts, &ctx(), &event(serde_json::json!({}))).await;
        match decision {
            HookDecision::Cancel(reason) => assert_eq!(reason, "blocked"),
            other => panic!("expected Cancel, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn extension_error_is_non_fatal_and_treated_as_continue() {
        let a = Arc::new(RecordingExt::new(
            "a",
            vec![BeforeAction::Error("boom".into())],
        ));
        let b = Arc::new(RecordingExt::new("b", vec![BeforeAction::Continue]));

        let exts: Vec<Arc<dyn Extension>> = vec![a.clone(), b.clone()];
        let decision =
            dispatch_before_tool_call(&exts, &ctx(), &event(serde_json::json!({}))).await;
        assert!(matches!(decision, HookDecision::Continue));
        assert_eq!(a.before_calls.lock().unwrap().len(), 1);
        assert_eq!(
            b.before_calls.lock().unwrap().len(),
            1,
            "later extensions still run after a sibling errors"
        );
    }

    #[tokio::test]
    async fn order_is_preserved_in_calls() {
        let a = Arc::new(RecordingExt::new("a", vec![BeforeAction::Continue]));
        let b = Arc::new(RecordingExt::new("b", vec![BeforeAction::Continue]));
        let c = Arc::new(RecordingExt::new("c", vec![BeforeAction::Continue]));

        let exts: Vec<Arc<dyn Extension>> = vec![a.clone(), b.clone(), c.clone()];
        let _ = dispatch_before_tool_call(&exts, &ctx(), &event(serde_json::json!({}))).await;

        // Each saw exactly one call.
        assert_eq!(a.before_calls.lock().unwrap().len(), 1);
        assert_eq!(b.before_calls.lock().unwrap().len(), 1);
        assert_eq!(c.before_calls.lock().unwrap().len(), 1);
    }

    /// Each extension is handed its own context, stamped with its own
    /// manifest name — two extensions never share a `data_dir`.
    #[tokio::test]
    async fn each_extension_gets_its_own_context() {
        struct ContextRecorder {
            manifest: ExtensionManifest,
            seen: Mutex<Vec<PathBuf>>,
        }

        #[async_trait]
        impl Extension for ContextRecorder {
            fn manifest(&self) -> &ExtensionManifest {
                &self.manifest
            }

            async fn on_before_tool_call(
                &self,
                cx: &ExtensionContext,
                _event: &ToolCallEvent,
            ) -> Result<HookDecision, ExtensionError> {
                self.seen.lock().unwrap().push(cx.data_dir.clone());
                Ok(HookDecision::Continue)
            }

            async fn on_after_tool_call(
                &self,
                cx: &ExtensionContext,
                _event: &ToolResultEvent,
            ) -> Result<(), ExtensionError> {
                self.seen.lock().unwrap().push(cx.data_dir.clone());
                Ok(())
            }
        }

        let a = Arc::new(ContextRecorder {
            manifest: manifest("alpha"),
            seen: Mutex::new(Vec::new()),
        });
        let b = Arc::new(ContextRecorder {
            manifest: manifest("beta"),
            seen: Mutex::new(Vec::new()),
        });

        let exts: Vec<Arc<dyn Extension>> = vec![a.clone(), b.clone()];
        let _ = dispatch_before_tool_call(&exts, &ctx(), &event(serde_json::json!({}))).await;
        dispatch_after_tool_call(&exts, &ctx(), &result_event()).await;

        assert_eq!(
            *a.seen.lock().unwrap(),
            vec![
                PathBuf::from("/tmp/.hand/extensions/alpha/data"),
                PathBuf::from("/tmp/.hand/extensions/alpha/data"),
            ]
        );
        assert_eq!(
            *b.seen.lock().unwrap(),
            vec![
                PathBuf::from("/tmp/.hand/extensions/beta/data"),
                PathBuf::from("/tmp/.hand/extensions/beta/data"),
            ]
        );
    }

    // -- Tests for dispatch_after_tool_call ----------------------------------

    fn result_event() -> ToolResultEvent {
        ToolResultEvent {
            tool_name: "read".into(),
            call_id: "call-1".into(),
            success: true,
            result: serde_json::json!({}),
        }
    }

    #[tokio::test]
    async fn after_calls_every_extension_even_when_one_errors() {
        let a = Arc::new(RecordingExt::new("a", vec![]).with_after(vec![Ok(())]));
        let b = Arc::new(RecordingExt::new("b", vec![]).with_after(vec![Err("boom".into())]));
        let c = Arc::new(RecordingExt::new("c", vec![]).with_after(vec![Ok(())]));

        let exts: Vec<Arc<dyn Extension>> = vec![a.clone(), b.clone(), c.clone()];
        dispatch_after_tool_call(&exts, &ctx(), &result_event()).await;

        assert_eq!(a.after_calls.lock().unwrap().len(), 1);
        assert_eq!(b.after_calls.lock().unwrap().len(), 1);
        assert_eq!(
            c.after_calls.lock().unwrap().len(),
            1,
            "after a sibling error, later extensions still get called"
        );
    }

    #[tokio::test]
    async fn after_no_extensions_is_a_no_op() {
        let exts: Vec<Arc<dyn Extension>> = Vec::new();
        // Just ensure it returns without panic.
        dispatch_after_tool_call(&exts, &ctx(), &result_event()).await;
    }
}
