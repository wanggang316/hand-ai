//! Shared resolution from [`Args`] into the values every run mode needs.
//!
//! The interactive flow, [`crate::modes::print`], and the headless RPC mode
//! all need to derive the same handful of values from CLI arguments: the
//! working directory, the resolved model, the stream options (carrying the
//! thinking level), the agent tool list, and the system-prompt overrides.
//! Centralising that here avoids two-copy drift between the modes.

use crate::cli::Args;
use crate::core::agent_session::AgentSessionConfig;
use crate::core::error::CodingAgentError;
use crate::core::model_resolver;
use crate::tools;
use hand_agent::types::AgentTool;
use model::SimpleStreamOptions;
use std::path::{Path, PathBuf};

/// Resolved values shared by all run modes.
pub struct SessionSetup {
    /// Working directory for the session.
    pub cwd: PathBuf,
    /// Resolved model.
    pub model: model::Model,
    /// Stream options with thinking level applied.
    pub stream_options: SimpleStreamOptions,
    /// Agent tool list (already filtered by `--tools` / `--no-tools`).
    pub agent_tools: Vec<AgentTool>,
    /// Custom system prompt (overrides default).
    pub custom_system_prompt: Option<String>,
    /// Text appended to the system prompt.
    pub custom_guidelines: Option<String>,
}

impl SessionSetup {
    /// Resolve CLI args into the values every mode needs.
    ///
    /// This consumes the `system_prompt` / `append_system_prompt` strings out
    /// of `args` via clones; the original `Args` is left untouched so the
    /// caller can keep reading other fields (`continue_session`, `fork`,
    /// `resume`, `no_session`, ...).
    pub fn resolve(args: &Args) -> Result<Self, CodingAgentError> {
        // Working directory: explicit `--cwd`, else current dir, else ".".
        let cwd = args
            .cwd
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

        // Reject typo'd `--provider` values up-front with pi-mono's exact
        // error text. Without this we'd silently fall back to the default
        // (anthropic) and surface a confusing "No API key found for
        // Anthropic" message at stream time, making it look like an auth
        // problem rather than a typo.
        if let Some(p) = args.provider.as_deref()
            && model::types::Provider::from_str(p).is_none()
        {
            return Err(CodingAgentError::Other(format!(
                "Unknown provider \"{p}\". Use --list-models to see available providers/models."
            )));
        }

        // Model: provider-default unless `--model` is explicit; thinking-level
        // CLI flag wins over the suffix embedded in the model pattern.
        //
        // When `--model` carries a gateway-style slashed id and no explicit
        // `--provider` was given, defer provider selection to the resolver
        // so the slash can drive routing (e.g. `--model deepseek/deepseek-r1`
        // resolves to openrouter without us pre-pinning anthropic).
        let explicit_provider = args.provider.as_deref();
        let model_pattern = args
            .model
            .as_deref()
            .unwrap_or_else(|| model_resolver::default_model_for_provider(
                explicit_provider.unwrap_or("anthropic"),
            ));
        let mut resolved = if explicit_provider.is_none() && model_pattern.contains('/') {
            model_resolver::resolve_model(None, model_pattern)
        } else {
            let provider = explicit_provider.unwrap_or("anthropic");
            model_resolver::resolve_model(Some(provider), model_pattern)
        };
        // When the user passes BOTH `--provider P -m a/b`, treat `a/b` as
        // the literal model id under P (e.g. `--provider openrouter -m
        // deepseek/deepseek-v4-flash`). resolve_model would otherwise split
        // the slash and resolve `b` under provider `a`, losing the `a/`
        // namespace that openrouter etc. require.
        if args.provider.is_some()
            && let Some(m) = args.model.as_deref()
            && m.contains('/')
            && !resolved.model.id.contains('/')
        {
            resolved.model.id = m
                .rsplit_once(':')
                .map(|(left, _)| left.to_string())
                .unwrap_or_else(|| m.to_string());
            if resolved.model.name.is_empty() || !resolved.model.name.contains('/') {
                resolved.model.name = resolved.model.id.clone();
            }
        }
        // `--base-url` overrides whatever default we picked. Useful for
        // self-hosted proxies / vendor-compat endpoints (e.g. pointing
        // anthropic at https://open.bigmodel.cn/api/anthropic).
        if let Some(base) = args.base_url.as_deref()
            && !base.is_empty()
        {
            resolved.model.base_url = base.to_string();
        }

        let thinking_level = args
            .thinking
            .as_deref()
            .and_then(model_resolver::parse_thinking_level)
            .or(resolved.thinking_level);

        let mut stream_options = SimpleStreamOptions::default();
        if let Some(level) = thinking_level {
            stream_options.reasoning = Some(level);
        }

        // Tool list: `--no-tools` empties it, `--tools` selects a subset,
        // otherwise the default set is used.
        //
        // NOTE: prior to the merge with origin/main, `Settings.shell_path`
        // was threaded into a `BashToolConfig` here. The bash tool was
        // rewritten on origin/main to hard-code `/bin/bash`; the
        // settings-driven shell override is dropped until a follow-up
        // re-introduces a `BashToolConfig` builder on the new tool factory.
        let agent_tools = if args.no_tools {
            Vec::new()
        } else if let Some(ref tool_list) = args.tools {
            create_selected_tools(&cwd, tool_list)
        } else {
            tools::create_default_tools(&cwd)
        };

        Ok(Self {
            cwd,
            model: resolved.model,
            stream_options,
            agent_tools,
            custom_system_prompt: args.system_prompt.clone(),
            custom_guidelines: args.append_system_prompt.clone(),
        })
    }

    /// Build an [`AgentSessionConfig`] from this setup.
    ///
    /// `resume_session` is wired straight from `--resume`; callers that need
    /// `--continue` or `--fork` should override the field after construction
    /// (or build the config themselves) since those paths require touching
    /// the [`crate::SessionManager`].
    pub fn to_config(&self, resume_session: Option<String>) -> AgentSessionConfig {
        AgentSessionConfig {
            cwd: self.cwd.clone(),
            model: self.model.clone(),
            stream_options: self.stream_options.clone(),
            custom_system_prompt: self.custom_system_prompt.clone(),
            custom_guidelines: self.custom_guidelines.clone(),
            resume_session,
        }
    }
}

/// Build the agent tool list for a comma-separated `--tools` argument.
///
/// Unknown names emit a warning and are skipped, matching the pre-extraction
/// behaviour from `main.rs`.
pub(crate) fn create_selected_tools(cwd: &Path, tool_list: &str) -> Vec<AgentTool> {
    let cwd = cwd.to_path_buf();
    let selected: Vec<&str> = tool_list.split(',').map(|s| s.trim()).collect();
    let mut result = Vec::new();

    for name in selected {
        match name {
            "read" => result.push(tools::read::create_read_tool(cwd.clone())),
            "write" => result.push(tools::write::create_write_tool(cwd.clone())),
            "edit" => result.push(tools::edit::create_edit_tool(cwd.clone())),
            "bash" => result.push(tools::bash::create_bash_tool(cwd.clone())),
            "grep" => result.push(tools::grep::create_grep_tool(cwd.clone())),
            "find" => result.push(tools::find::create_find_tool(cwd.clone())),
            "ls" => result.push(tools::ls::create_ls_tool(cwd.clone())),
            other => eprintln!("Warning: unknown tool '{}'", other),
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn resolves_default_args() {
        let args = Args::try_parse_from(["hand"]).expect("default parse");
        let setup = SessionSetup::resolve(&args).expect("resolve");
        // Default tool list should match the full built-in set.
        let default_len = tools::create_default_tools(&setup.cwd).len();
        assert_eq!(setup.agent_tools.len(), default_len);
        assert!(setup.custom_system_prompt.is_none());
        assert!(setup.custom_guidelines.is_none());
        assert!(setup.stream_options.reasoning.is_none());
    }

    #[test]
    fn no_tools_empties_tool_list() {
        let args = Args::try_parse_from(["hand", "--no-tools"]).expect("parse");
        let setup = SessionSetup::resolve(&args).expect("resolve");
        assert!(setup.agent_tools.is_empty());
    }

    /// Parity with pi-mono: a typo'd `--provider` must surface a clean
    /// "Unknown provider" error rather than silently falling back to the
    /// default provider and then erroring on a missing API key further
    /// downstream. Mirrors pi-mono's exact message text so scripts can
    /// pattern-match on it.
    #[test]
    fn unknown_provider_returns_descriptive_error() {
        let args = Args::try_parse_from([
            "hand",
            "--provider",
            "nonexistent",
            "--model",
            "fake",
        ])
        .expect("parse");
        let result = SessionSetup::resolve(&args);
        let err = match result {
            Ok(_) => panic!("must reject unknown provider"),
            Err(e) => e,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("Unknown provider \"nonexistent\""),
            "expected pi-style message, got: {msg}"
        );
        assert!(
            msg.contains("--list-models"),
            "must hint at --list-models for discoverability, got: {msg}"
        );
    }

    /// Known providers (in the registry) must still resolve. Sanity check
    /// so the validator doesn't accidentally over-reject.
    #[test]
    fn known_provider_does_not_error() {
        let args = Args::try_parse_from([
            "hand",
            "--provider",
            "openrouter",
            "--model",
            "deepseek/deepseek-v4-flash",
        ])
        .expect("parse");
        let setup = SessionSetup::resolve(&args).expect("resolve known provider");
        assert_eq!(setup.model.provider.as_str(), "openrouter");
    }
}
