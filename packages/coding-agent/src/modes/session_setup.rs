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
use crate::core::settings::SettingsManager;
use crate::tools;
use crate::tools::bash::BashToolConfig;
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

        // Model: provider-default unless `--model` is explicit; thinking-level
        // CLI flag wins over the suffix embedded in the model pattern.
        let provider = args.provider.as_deref().unwrap_or("anthropic");
        let model_pattern = args
            .model
            .as_deref()
            .unwrap_or_else(|| model_resolver::default_model_for_provider(provider));
        let resolved = model_resolver::resolve_model(Some(provider), model_pattern);

        let thinking_level = args
            .thinking
            .as_deref()
            .and_then(model_resolver::parse_thinking_level)
            .or(resolved.thinking_level);

        let mut stream_options = SimpleStreamOptions::default();
        if let Some(level) = thinking_level {
            stream_options.reasoning = Some(level);
        }

        // Resolve the bash shell path from settings (best-effort): if
        // settings load fails for any reason, fall back to the default
        // (`/bin/bash`). The session itself will surface the error later
        // when it reads settings for compaction/retry/etc.
        let bash_config = match SettingsManager::from_cwd(&cwd) {
            Ok(mgr) => BashToolConfig {
                shell_path: mgr
                    .shell_path()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| PathBuf::from("/bin/bash")),
            },
            Err(_) => BashToolConfig::default(),
        };

        // Tool list: `--no-tools` empties it, `--tools` selects a subset,
        // otherwise the default set is used.
        let agent_tools = if args.no_tools {
            Vec::new()
        } else if let Some(ref tool_list) = args.tools {
            create_selected_tools(&cwd, tool_list, &bash_config)
        } else {
            tools::create_default_tools_with_config(&cwd, bash_config)
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
pub(crate) fn create_selected_tools(
    cwd: &Path,
    tool_list: &str,
    bash_config: &BashToolConfig,
) -> Vec<AgentTool> {
    let cwd = cwd.to_path_buf();
    let selected: Vec<&str> = tool_list.split(',').map(|s| s.trim()).collect();
    let mut result = Vec::new();

    for name in selected {
        match name {
            "read" => result.push(tools::read::create_read_tool(cwd.clone())),
            "write" => result.push(tools::write::create_write_tool(cwd.clone())),
            "edit" => result.push(tools::edit::create_edit_tool(cwd.clone())),
            "bash" => result.push(tools::bash::create_bash_tool_with_config(
                cwd.clone(),
                bash_config.clone(),
            )),
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

}
