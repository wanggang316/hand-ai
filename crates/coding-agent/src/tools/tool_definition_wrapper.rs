//! Wrap a [`ToolDefinition`] into the runtime [`AgentTool`] type.
//!
//! A "definition" carries a tool's name, schema, prompt metadata, and
//! optional render hooks; the runtime [`AgentTool`] is what the agent
//! loop actually invokes. This wrapper bridges the two.
//!
//! ## Current scope
//!
//! The extension API in `core::extensions::api` does not yet model the
//! interactive-mode UI surface (`prompt_snippet`, `prompt_guidelines`,
//! `render_shell`, `render_call`, `render_result`). The
//! [`ToolDefinition`] struct here carries the *runtime* subset (the
//! fields needed to actually execute the tool) so callers can register
//! tools today; UI-only fields will be added once the interactive-mode
//! surface lands, and the wrapper will drop them when projecting onto
//! [`AgentTool`].

use std::path::PathBuf;
use std::sync::Arc;

use hand_agent::types::{
    AgentTool, PrepareArgumentsFn, ToolExecuteCtx, ToolExecuteFn, ToolExecutionMode,
};

use crate::core::extensions::api::ExtensionContext;

/// Factory that builds an [`ExtensionContext`] on demand.
///
/// `Arc<dyn Fn>` so the closure is cheap to clone into the per-call
/// execute closure. Returns `Some(ctx)` when the caller has live
/// session metadata; `None` when no context is available (in which
/// case the wrapped tool's `execute` receives a stub built from the
/// recorded cwd and a synthetic session id).
pub type ContextFactory = Arc<dyn Fn() -> Option<ExtensionContext> + Send + Sync>;

/// Boxed extension-aware tool executor.
///
/// Same shape as `AgentTool::execute` but with an extra trailing
/// `ExtensionContext`. The wrapper turns this into the runtime closure
/// by capturing a [`ContextFactory`].
pub type ExtensionToolExecuteFn = Box<
    dyn Fn(
            ToolExecuteCtx,
            ExtensionContext,
        ) -> futures::future::BoxFuture<
            'static,
            Result<hand_agent::types::ToolResult, hand_agent::types::ToolError>,
        > + Send
        + Sync,
>;

/// Runtime-flavoured `ToolDefinition`. See module docs.
pub struct ToolDefinition {
    pub name: String,
    pub label: String,
    pub description: String,
    pub parameters: serde_json::Value,
    pub execution_mode: Option<ToolExecutionMode>,
    pub prepare_arguments: Option<PrepareArgumentsFn>,
    pub execute: ExtensionToolExecuteFn,
}

/// Wrap a [`ToolDefinition`] into an [`AgentTool`].
///
/// `ctx_factory` is consulted on every tool call. When it returns
/// `None` (extension context not yet built — e.g. early in a non-
/// interactive run), a fallback context is synthesised from `fallback_cwd`.
pub fn wrap_tool_definition(
    definition: ToolDefinition,
    ctx_factory: Option<ContextFactory>,
    fallback_cwd: PathBuf,
) -> AgentTool {
    let exec = definition.execute;
    let execute: ToolExecuteFn = Box::new(move |call_ctx: ToolExecuteCtx| {
        let cx = ctx_factory
            .as_ref()
            .and_then(|f| f())
            .unwrap_or_else(|| ExtensionContext {
                cwd: fallback_cwd.clone(),
                session_id: String::new(),
                data_dir: fallback_cwd.clone(),
            });
        exec(call_ctx, cx)
    });

    let mut tool = AgentTool::new(
        definition.name,
        definition.description,
        definition.parameters,
        definition.label,
        execute,
    );
    tool.execution_mode = definition.execution_mode;
    tool.prepare_arguments = definition.prepare_arguments;
    tool
}

/// Wrap a batch of definitions in one call.
pub fn wrap_tool_definitions(
    definitions: Vec<ToolDefinition>,
    ctx_factory: Option<ContextFactory>,
    fallback_cwd: PathBuf,
) -> Vec<AgentTool> {
    definitions
        .into_iter()
        .map(|d| wrap_tool_definition(d, ctx_factory.clone(), fallback_cwd.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hand_agent::types::ToolResult;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn empty_schema() -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }

    #[tokio::test]
    async fn wrapper_invokes_definition_execute_with_context() {
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_in_def = Arc::clone(&counter);
        let captured_cwd: Arc<std::sync::Mutex<Option<PathBuf>>> =
            Arc::new(std::sync::Mutex::new(None));
        let captured_cwd_in_def = Arc::clone(&captured_cwd);

        let def = ToolDefinition {
            name: "echo".into(),
            label: "Echo".into(),
            description: "echo args".into(),
            parameters: empty_schema(),
            execution_mode: None,
            prepare_arguments: None,
            execute: Box::new(move |_call, cx| {
                let counter = Arc::clone(&counter_in_def);
                let captured = Arc::clone(&captured_cwd_in_def);
                let cwd = cx.cwd;
                Box::pin(async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    *captured.lock().expect("mutex") = Some(cwd);
                    Ok(ToolResult::text("ok"))
                })
            }),
        };

        let factory: ContextFactory = Arc::new(|| {
            Some(ExtensionContext {
                cwd: PathBuf::from("/tmp/from-factory"),
                session_id: "sess-1".into(),
                data_dir: PathBuf::from("/tmp/data"),
            })
        });

        let tool = wrap_tool_definition(def, Some(factory), PathBuf::from("/tmp/fallback"));

        // Invoke the wrapped tool.
        let call = ToolExecuteCtx {
            tool_call_id: "call-1".into(),
            args: serde_json::json!({}),
            cancel: hand_agent::CancellationToken::new(),
            on_update: Arc::new(|_| {}),
        };
        (tool.execute)(call).await.expect("ok result");

        assert_eq!(counter.load(Ordering::SeqCst), 1);
        assert_eq!(
            captured_cwd.lock().expect("mutex").as_deref(),
            Some(std::path::Path::new("/tmp/from-factory")),
            "wrapper should pass the factory-built context through"
        );
    }

    #[tokio::test]
    async fn wrapper_falls_back_when_factory_returns_none() {
        let captured_cwd: Arc<std::sync::Mutex<Option<PathBuf>>> =
            Arc::new(std::sync::Mutex::new(None));
        let captured_cwd_in_def = Arc::clone(&captured_cwd);

        let def = ToolDefinition {
            name: "echo".into(),
            label: "Echo".into(),
            description: "echo args".into(),
            parameters: empty_schema(),
            execution_mode: None,
            prepare_arguments: None,
            execute: Box::new(move |_call, cx| {
                let captured = Arc::clone(&captured_cwd_in_def);
                let cwd = cx.cwd;
                Box::pin(async move {
                    *captured.lock().expect("mutex") = Some(cwd);
                    Ok(ToolResult::text("ok"))
                })
            }),
        };

        let factory: ContextFactory = Arc::new(|| None);
        let tool = wrap_tool_definition(def, Some(factory), PathBuf::from("/tmp/fallback"));

        let call = ToolExecuteCtx {
            tool_call_id: "call-1".into(),
            args: serde_json::json!({}),
            cancel: hand_agent::CancellationToken::new(),
            on_update: Arc::new(|_| {}),
        };
        (tool.execute)(call).await.expect("ok result");

        assert_eq!(
            captured_cwd.lock().expect("mutex").as_deref(),
            Some(std::path::Path::new("/tmp/fallback")),
            "wrapper should synthesise a fallback context when the factory is empty"
        );
    }

    #[tokio::test]
    async fn wrap_tool_definitions_handles_batches() {
        let def_a = ToolDefinition {
            name: "a".into(),
            label: "A".into(),
            description: "a".into(),
            parameters: empty_schema(),
            execution_mode: None,
            prepare_arguments: None,
            execute: Box::new(|_, _| Box::pin(async { Ok(ToolResult::text("a")) })),
        };
        let def_b = ToolDefinition {
            name: "b".into(),
            label: "B".into(),
            description: "b".into(),
            parameters: empty_schema(),
            execution_mode: None,
            prepare_arguments: None,
            execute: Box::new(|_, _| Box::pin(async { Ok(ToolResult::text("b")) })),
        };

        let tools = wrap_tool_definitions(vec![def_a, def_b], None, PathBuf::from("/tmp"));
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "a");
        assert_eq!(tools[1].name, "b");
    }
}
