//! Hook dispatcher for the Tier 1 extension chain.
//!
//! Aggregates `Vec<Arc<dyn Extension>>` into a single decision per hook
//! invocation. The ordering and aggregation rules are documented on each
//! function and exercised by the tests at the bottom of this file.
//!
//! See ADR-001 and Phase 3 task T3.3a for the design.

use super::api::{Extension, ExtensionContext, HookDecision, ToolCallEvent, ToolResultEvent};
use std::sync::Arc;

/// Dispatch `on_before_tool_call` across an ordered chain of extensions.
///
/// # Aggregation rules
///
/// Extensions are called sequentially in the order they appear in `extensions`:
///
/// - `Continue` — args are unchanged; continue to the next extension.
/// - `Replace(new_args)` — the working `ToolCallEvent::arguments` is updated to
///   `new_args` so subsequent extensions see the replaced value. The chain
///   keeps running.
/// - `Cancel(reason)` — the chain short-circuits; no further extensions are
///   called, and `Cancel(reason)` is returned immediately.
/// - `Err(_)` — the error is logged via `tracing::warn!` and treated as
///   `Continue` for that extension. A misbehaving extension never aborts the
///   chain.
///
/// If multiple extensions return `Replace`, **the last one wins** because each
/// `Replace` overwrites the previous working arguments before the next
/// extension is called.
///
/// The returned `HookDecision`:
/// - `Continue` — every extension returned `Continue` (or errored);
///   `event.arguments` is unchanged from the caller's input.
/// - `Replace(final_args)` — at least one extension returned `Replace` and no
///   later one returned `Cancel`; `final_args` is the last replacement value.
/// - `Cancel(reason)` — some extension cancelled.
pub(crate) async fn dispatch_before_tool_call(
    extensions: &[Arc<dyn Extension>],
    cx: &ExtensionContext,
    event: &ToolCallEvent,
) -> HookDecision {
    let mut working = event.clone();
    let mut replaced: Option<serde_json::Value> = None;

    for ext in extensions {
        let decision = match ext.on_before_tool_call(cx, &working).await {
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
                working.arguments = new_args.clone();
                replaced = Some(new_args);
            }
            HookDecision::Cancel(reason) => {
                return HookDecision::Cancel(reason);
            }
        }
    }

    match replaced {
        Some(args) => HookDecision::Replace(args),
        None => HookDecision::Continue,
    }
}

/// Dispatch `on_after_tool_call` across an ordered chain of extensions.
///
/// Every extension is called with the same `event` in registration order.
/// Errors are logged and ignored; one misbehaving extension never prevents
/// the rest from running. There is no aggregated return value — `after`
/// hooks are observational only.
pub(crate) async fn dispatch_after_tool_call(
    extensions: &[Arc<dyn Extension>],
    cx: &ExtensionContext,
    event: &ToolResultEvent,
) {
    for ext in extensions {
        if let Err(err) = ext.on_after_tool_call(cx, event).await {
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
        Extension, ExtensionCapabilities, ExtensionError, ExtensionManifest,
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
        }
    }

    fn ctx() -> ExtensionContext {
        ExtensionContext {
            cwd: PathBuf::from("/tmp"),
            session_id: "test-session".into(),
            data_dir: PathBuf::from("/tmp/data"),
        }
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
        let decision = dispatch_before_tool_call(&exts, &ctx(), &event(serde_json::json!({}))).await;
        assert!(matches!(decision, HookDecision::Continue));
    }

    #[tokio::test]
    async fn single_continue_returns_continue() {
        let ext = Arc::new(RecordingExt::new("a", vec![BeforeAction::Continue]));
        let exts: Vec<Arc<dyn Extension>> = vec![ext.clone()];
        let decision = dispatch_before_tool_call(&exts, &ctx(), &event(serde_json::json!({}))).await;
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
        let decision = dispatch_before_tool_call(&exts, &ctx(), &event(serde_json::json!({}))).await;

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

    #[tokio::test]
    async fn replace_then_cancel_returns_cancel() {
        let args1 = serde_json::json!({"step": 1});
        let a = Arc::new(RecordingExt::new("a", vec![BeforeAction::Replace(args1)]));
        let b = Arc::new(RecordingExt::new(
            "b",
            vec![BeforeAction::Cancel("blocked".into())],
        ));

        let exts: Vec<Arc<dyn Extension>> = vec![a, b];
        let decision = dispatch_before_tool_call(&exts, &ctx(), &event(serde_json::json!({}))).await;
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
        let decision = dispatch_before_tool_call(&exts, &ctx(), &event(serde_json::json!({}))).await;
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
