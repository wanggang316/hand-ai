//! Hook dispatcher for the Tier 1 extension chain.
//!
//! Aggregates `Vec<Arc<dyn Extension>>` into a single decision per hook
//! invocation. The ordering and aggregation rules are documented on each
//! function and exercised by the tests at the bottom of this file.
//!
//! See ADR-001 and Phase 3 task T3.3a for the design.

use super::api::{
    Extension, ExtensionContextFactory, HookDecision, ResultDecision, ToolCallEvent,
    ToolResultEvent, UserMessageEvent, UserMessageOutcome,
};
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

/// Dispatch `on_user_message` across an ordered chain of extensions.
///
/// Only extensions whose manifest declares `capabilities.on_user_message`
/// are called — unlike the tool-call hooks, this one fires on every turn,
/// so the host does not round-trip extensions that never asked for it.
///
/// Aggregation matches [`dispatch_before_tool_call`]: `Cancel` short-
/// circuits and wins, a `Replace` re-runs the chain from the head so an
/// extension ahead of the rewriter re-inspects the final text, errors are
/// logged and treated as `Continue`, and the chain is bounded to
/// [`MAX_BEFORE_TOOL_CALL_ROUNDS`] passes.
///
/// `Replace` must carry a JSON string; any other payload is a contract
/// violation, logged and ignored.
///
/// The returned decision's `Replace` variant always holds a
/// `serde_json::Value::String`.
pub(crate) async fn dispatch_user_message(
    extensions: &[Arc<dyn Extension>],
    contexts: &ExtensionContextFactory,
    event: &UserMessageEvent,
) -> UserMessageResolution {
    let subscribed: Vec<&Arc<dyn Extension>> = extensions
        .iter()
        .filter(|ext| ext.manifest().capabilities.on_user_message)
        .collect();
    if subscribed.is_empty() {
        return UserMessageResolution::cont();
    }

    let mut working = event.clone();
    let mut replaced: Option<String> = None;
    // Survives across rounds, keyed by extension: each one's most recent
    // contribution wins. Discarding all but the converging round would
    // lose the common case — a scrubber that rewrites the prompt and says
    // so has nothing left to report once its own rewrite has landed, so
    // its note would vanish on the very re-validation pass it triggered.
    // Keying by name is also what stops a rewriting chain from repeating
    // the same note once per round.
    let mut collected: Vec<(String, String)> = Vec::new();

    for _round in 0..MAX_BEFORE_TOOL_CALL_ROUNDS {
        let mut replaced_this_round = false;

        for ext in &subscribed {
            let name = ext.manifest().name.clone();
            let cx = contexts.for_extension(&name);
            let outcome = match ext.on_user_message(&cx, &working).await {
                Ok(o) => o,
                Err(err) => {
                    tracing::warn!(
                        extension = %name,
                        error = %err,
                        "extension on_user_message errored; treating as Continue"
                    );
                    UserMessageOutcome::cont()
                }
            };

            if let Some(text) = outcome.additional_context {
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    tracing::debug!(
                        extension = %name,
                        "on_user_message returned blank additional_context; ignoring"
                    );
                } else if let Some(slot) =
                    collected.iter_mut().find(|(existing, _)| existing == &name)
                {
                    slot.1 = trimmed.to_string();
                } else {
                    collected.push((name.clone(), trimmed.to_string()));
                }
            }

            match outcome.decision {
                HookDecision::Continue => {}
                HookDecision::Replace(value) => {
                    let Some(text) = value.as_str() else {
                        tracing::warn!(
                            extension = %name,
                            "on_user_message returned a non-string Replace payload; ignoring"
                        );
                        continue;
                    };
                    if text == working.text {
                        continue;
                    }
                    working.text = text.to_string();
                    replaced = Some(text.to_string());
                    replaced_this_round = true;
                }
                // A cancelled turn never reaches the model, so any context
                // gathered for it is dropped with the rest of the turn.
                HookDecision::Cancel(reason) => {
                    return UserMessageResolution {
                        decision: HookDecision::Cancel(reason),
                        contexts: Vec::new(),
                    };
                }
            }
        }

        if !replaced_this_round {
            return UserMessageResolution {
                decision: match replaced {
                    Some(text) => HookDecision::Replace(serde_json::Value::String(text)),
                    None => HookDecision::Continue,
                },
                contexts: collected,
            };
        }
    }

    tracing::warn!(
        rounds = MAX_BEFORE_TOOL_CALL_ROUNDS,
        "extension chain kept rewriting the user message; cancelling the turn"
    );
    UserMessageResolution {
        decision: HookDecision::Cancel(format!(
            "extension chain did not converge on the user message after \
             {MAX_BEFORE_TOOL_CALL_ROUNDS} rounds of rewriting"
        )),
        contexts: Vec::new(),
    }
}

/// Aggregated outcome of the `on_user_message` chain.
#[derive(Debug, Clone)]
pub(crate) struct UserMessageResolution {
    pub decision: HookDecision,
    /// `(extension name, context text)` in registration order. Empty when
    /// the turn was cancelled.
    pub contexts: Vec<(String, String)>,
}

impl UserMessageResolution {
    fn cont() -> Self {
        Self {
            decision: HookDecision::Continue,
            contexts: Vec::new(),
        }
    }
}

/// Dispatch `on_after_tool_call` across an ordered chain of extensions.
///
/// Sequential in registration order, and each extension observes its
/// predecessor's replacement rather than the tool's original output. That
/// ordering is what makes redaction composable: a summariser registered
/// after a scrubber works from the scrubbed text, so it cannot reintroduce
/// the secret the scrubber removed. When a single extension replaces, the
/// effect is identical to last-write-wins.
///
/// Errors are logged and ignored; one misbehaving extension never prevents
/// the rest from running, and never discards the result accumulated so far.
///
/// Returns the final replacement, or `None` when nothing changed.
pub(crate) async fn dispatch_after_tool_call(
    extensions: &[Arc<dyn Extension>],
    contexts: &ExtensionContextFactory,
    event: &ToolResultEvent,
) -> Option<serde_json::Value> {
    let mut working = event.clone();
    let mut replaced = false;

    for ext in extensions {
        let name = ext.manifest().name.clone();
        let cx = contexts.for_extension(&name);
        match ext.on_after_tool_call(&cx, &working).await {
            Ok(ResultDecision::Continue) => {}
            Ok(ResultDecision::Replace(value)) => {
                working.result = value;
                replaced = true;
            }
            Err(err) => {
                tracing::warn!(
                    extension = %name,
                    error = %err,
                    "extension on_after_tool_call errored; ignoring"
                );
            }
        }
    }

    replaced.then_some(working.result)
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
            timeouts: Default::default(),
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

    /// The after-hook counterpart. No `Cancel`: the tool already ran.
    enum AfterAction {
        Continue,
        Replace(serde_json::Value),
        Error(String),
    }

    /// A test extension that records every event it sees and returns a
    /// caller-configured response (Continue / Replace / Cancel / Error) per
    /// call.
    struct RecordingExt {
        manifest: ExtensionManifest,
        before_actions: Mutex<Vec<BeforeAction>>,
        before_calls: Mutex<Vec<ToolCallEvent>>,
        after_actions: Mutex<Vec<AfterAction>>,
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

        fn with_after(mut self, after: Vec<AfterAction>) -> Self {
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
        ) -> Result<ResultDecision, ExtensionError> {
            self.after_calls.lock().unwrap().push(event.clone());
            let mut actions = self.after_actions.lock().unwrap();
            if actions.is_empty() {
                return Ok(ResultDecision::Continue);
            }
            match actions.remove(0) {
                AfterAction::Continue => Ok(ResultDecision::Continue),
                AfterAction::Replace(v) => Ok(ResultDecision::Replace(v)),
                AfterAction::Error(msg) => Err(ExtensionError::Custom {
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
                Some(p) if !p.starts_with("/workspace") => Ok(HookDecision::Cancel(format!(
                    "path {p} is outside /workspace"
                ))),
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
        assert_eq!(
            guard_calls.len(),
            2,
            "guard is re-consulted after a replace"
        );
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
            ) -> Result<ResultDecision, ExtensionError> {
                self.seen.lock().unwrap().push(cx.data_dir.clone());
                Ok(ResultDecision::Continue)
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

    // -- Tests for dispatch_user_message -------------------------------------

    /// A test extension for the user-message hook. `subscribed` controls
    /// whether the manifest declares the capability.
    struct PromptExt {
        manifest: ExtensionManifest,
        action: Mutex<Vec<UserMessageOutcome>>,
        seen: Mutex<Vec<String>>,
    }

    impl PromptExt {
        fn new(name: &str, subscribed: bool, actions: Vec<UserMessageOutcome>) -> Arc<Self> {
            let mut manifest = manifest(name);
            manifest.capabilities.on_user_message = subscribed;
            Arc::new(Self {
                manifest,
                action: Mutex::new(actions),
                seen: Mutex::new(Vec::new()),
            })
        }
    }

    #[async_trait]
    impl Extension for PromptExt {
        fn manifest(&self) -> &ExtensionManifest {
            &self.manifest
        }

        async fn on_user_message(
            &self,
            _cx: &ExtensionContext,
            event: &UserMessageEvent,
        ) -> Result<UserMessageOutcome, ExtensionError> {
            self.seen.lock().unwrap().push(event.text.clone());
            let mut actions = self.action.lock().unwrap();
            if actions.is_empty() {
                return Ok(UserMessageOutcome::cont());
            }
            Ok(actions.remove(0))
        }
    }

    fn prompt(text: &str) -> UserMessageEvent {
        UserMessageEvent { text: text.into() }
    }

    #[tokio::test]
    async fn user_message_hook_fires_once_with_the_raw_prompt() {
        let ext = PromptExt::new("linter", true, vec![HookDecision::Continue.into()]);
        let exts: Vec<Arc<dyn Extension>> = vec![ext.clone()];

        let resolution = dispatch_user_message(&exts, &ctx(), &prompt("hello world")).await;
        assert!(matches!(resolution.decision, HookDecision::Continue));
        assert!(resolution.contexts.is_empty());
        assert_eq!(*ext.seen.lock().unwrap(), vec!["hello world".to_string()]);
    }

    #[tokio::test]
    async fn user_message_hook_skips_extensions_that_did_not_declare_it() {
        let ext = PromptExt::new(
            "silent",
            false,
            vec![HookDecision::Cancel("no".into()).into()],
        );
        let exts: Vec<Arc<dyn Extension>> = vec![ext.clone()];

        let resolution = dispatch_user_message(&exts, &ctx(), &prompt("hello")).await;
        assert!(matches!(resolution.decision, HookDecision::Continue));
        assert!(
            ext.seen.lock().unwrap().is_empty(),
            "an extension that did not declare the capability must never be called"
        );
    }

    #[tokio::test]
    async fn user_message_replace_rewrites_the_prompt_and_is_re_validated() {
        let scrubber = PromptExt::new(
            "scrubber",
            true,
            vec![HookDecision::Replace(serde_json::json!("token=[redacted]")).into()],
        );
        let auditor = PromptExt::new("auditor", true, vec![]);
        let exts: Vec<Arc<dyn Extension>> = vec![scrubber.clone(), auditor.clone()];

        let resolution = dispatch_user_message(&exts, &ctx(), &prompt("token=hunter2")).await;
        match resolution.decision {
            HookDecision::Replace(v) => assert_eq!(v, serde_json::json!("token=[redacted]")),
            other => panic!("expected Replace, got {other:?}"),
        }
        // The extension ahead of the rewrite sees the final text too.
        assert_eq!(
            *scrubber.seen.lock().unwrap(),
            vec!["token=hunter2".to_string(), "token=[redacted]".to_string()]
        );
        assert_eq!(
            auditor.seen.lock().unwrap().last().map(String::as_str),
            Some("token=[redacted]")
        );
    }

    #[tokio::test]
    async fn user_message_cancel_wins() {
        let a = PromptExt::new("a", true, vec![HookDecision::Continue.into()]);
        let b = PromptExt::new(
            "b",
            true,
            vec![HookDecision::Cancel("contains a secret".into()).into()],
        );
        let exts: Vec<Arc<dyn Extension>> = vec![a, b];

        match dispatch_user_message(&exts, &ctx(), &prompt("hi"))
            .await
            .decision
        {
            HookDecision::Cancel(reason) => assert_eq!(reason, "contains a secret"),
            other => panic!("expected Cancel, got {other:?}"),
        }
    }

    /// `Replace` on this hook carries prompt text; a non-string payload is a
    /// contract violation and must not silently corrupt the prompt.
    #[tokio::test]
    async fn user_message_non_string_replace_is_ignored() {
        let ext = PromptExt::new(
            "confused",
            true,
            vec![HookDecision::Replace(serde_json::json!({"text": "nope"})).into()],
        );
        let exts: Vec<Arc<dyn Extension>> = vec![ext];

        let resolution = dispatch_user_message(&exts, &ctx(), &prompt("hi")).await;
        assert!(matches!(resolution.decision, HookDecision::Continue));
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
        let a = Arc::new(RecordingExt::new("a", vec![]).with_after(vec![AfterAction::Continue]));
        let b = Arc::new(
            RecordingExt::new("b", vec![]).with_after(vec![AfterAction::Error("boom".into())]),
        );
        let c = Arc::new(RecordingExt::new("c", vec![]).with_after(vec![AfterAction::Continue]));

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
        assert!(
            dispatch_after_tool_call(&exts, &ctx(), &result_event())
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn after_replace_is_returned_as_the_final_result() {
        let ext =
            Arc::new(
                RecordingExt::new("scrubber", vec![]).with_after(vec![AfterAction::Replace(
                    serde_json::json!({"content": [], "details": "scrubbed"}),
                )]),
            );
        let exts: Vec<Arc<dyn Extension>> = vec![ext];

        let replaced = dispatch_after_tool_call(&exts, &ctx(), &result_event()).await;
        assert_eq!(
            replaced,
            Some(serde_json::json!({"content": [], "details": "scrubbed"}))
        );
    }

    #[tokio::test]
    async fn after_chain_feeds_each_replacement_to_the_next_extension() {
        let scrubber =
            Arc::new(
                RecordingExt::new("scrubber", vec![]).with_after(vec![AfterAction::Replace(
                    serde_json::json!({"details": "scrubbed"}),
                )]),
            );
        let summariser = Arc::new(RecordingExt::new("summariser", vec![]).with_after(vec![
            AfterAction::Replace(serde_json::json!({"details": "summarised"})),
        ]));
        let exts: Vec<Arc<dyn Extension>> = vec![scrubber.clone(), summariser.clone()];

        let replaced = dispatch_after_tool_call(&exts, &ctx(), &result_event()).await;

        // Last replacement wins, matching the before-chain rule…
        assert_eq!(replaced, Some(serde_json::json!({"details": "summarised"})));
        // …and the second extension worked from the first's output, not the
        // tool's. This is what keeps a scrubber from being undone by a
        // summariser registered behind it.
        assert_eq!(
            summariser.after_calls.lock().unwrap()[0].result,
            serde_json::json!({"details": "scrubbed"})
        );
    }

    /// An extension that errors must not discard a replacement an earlier
    /// one already made — failing open on the *original* would undo a
    /// redaction because of an unrelated extension's bug.
    #[tokio::test]
    async fn after_error_preserves_an_earlier_replacement() {
        let scrubber =
            Arc::new(
                RecordingExt::new("scrubber", vec![]).with_after(vec![AfterAction::Replace(
                    serde_json::json!({"details": "scrubbed"}),
                )]),
            );
        let broken = Arc::new(
            RecordingExt::new("broken", vec![]).with_after(vec![AfterAction::Error("boom".into())]),
        );
        let exts: Vec<Arc<dyn Extension>> = vec![scrubber, broken];

        let replaced = dispatch_after_tool_call(&exts, &ctx(), &result_event()).await;
        assert_eq!(replaced, Some(serde_json::json!({"details": "scrubbed"})));
    }

    #[tokio::test]
    async fn after_all_continue_reports_no_replacement() {
        let a = Arc::new(RecordingExt::new("a", vec![]).with_after(vec![AfterAction::Continue]));
        let exts: Vec<Arc<dyn Extension>> = vec![a];

        assert!(
            dispatch_after_tool_call(&exts, &ctx(), &result_event())
                .await
                .is_none(),
            "an untouched result must not be reported as a rewrite"
        );
    }

    // -- Tests for additional_context ----------------------------------------

    #[tokio::test]
    async fn user_message_context_is_collected_in_registration_order() {
        let a = PromptExt::new(
            "git",
            true,
            vec![UserMessageOutcome::context("on branch main")],
        );
        let b = PromptExt::new(
            "ci",
            true,
            vec![UserMessageOutcome::context("build is red")],
        );
        let exts: Vec<Arc<dyn Extension>> = vec![a, b];

        let resolution = dispatch_user_message(&exts, &ctx(), &prompt("ship it")).await;
        assert!(matches!(resolution.decision, HookDecision::Continue));
        assert_eq!(
            resolution.contexts,
            vec![
                ("git".to_string(), "on branch main".to_string()),
                ("ci".to_string(), "build is red".to_string()),
            ]
        );
    }

    /// Context and decision are orthogonal: informing the model must not
    /// cost the turn, which is the whole point of the separate channel.
    #[tokio::test]
    async fn user_message_context_rides_along_with_a_replace() {
        let ext = PromptExt::new(
            "scrubber",
            true,
            vec![UserMessageOutcome {
                decision: HookDecision::Replace(serde_json::json!("token=[redacted]")),
                additional_context: Some("a secret was removed from this prompt".into()),
            }],
        );
        let exts: Vec<Arc<dyn Extension>> = vec![ext];

        let resolution = dispatch_user_message(&exts, &ctx(), &prompt("token=hunter2")).await;
        match resolution.decision {
            HookDecision::Replace(v) => assert_eq!(v, serde_json::json!("token=[redacted]")),
            other => panic!("expected Replace, got {other:?}"),
        }
        // Reported once, not once per re-validation round.
        assert_eq!(resolution.contexts.len(), 1);
        assert_eq!(
            resolution.contexts[0].1,
            "a secret was removed from this prompt"
        );
    }

    #[tokio::test]
    async fn user_message_context_is_dropped_when_the_turn_is_cancelled() {
        let informer = PromptExt::new("informer", true, vec![UserMessageOutcome::context("fyi")]);
        let guard = PromptExt::new(
            "guard",
            true,
            vec![HookDecision::Cancel("contains a secret".into()).into()],
        );
        let exts: Vec<Arc<dyn Extension>> = vec![informer, guard];

        let resolution = dispatch_user_message(&exts, &ctx(), &prompt("hi")).await;
        assert!(matches!(resolution.decision, HookDecision::Cancel(_)));
        assert!(
            resolution.contexts.is_empty(),
            "a turn that never reaches the model carries no context"
        );
    }

    #[tokio::test]
    async fn user_message_blank_context_is_ignored() {
        let ext = PromptExt::new("noisy", true, vec![UserMessageOutcome::context("   \n  ")]);
        let exts: Vec<Arc<dyn Extension>> = vec![ext];

        let resolution = dispatch_user_message(&exts, &ctx(), &prompt("hi")).await;
        assert!(resolution.contexts.is_empty());
    }
}
