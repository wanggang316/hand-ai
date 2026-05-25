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
    /// `--no-session`: skip on-disk persistence and run with an in-memory
    /// session.
    pub no_session: bool,
    /// `--no-context-files`: skip auto-loading project context files.
    pub no_context_files: bool,
    /// `--session-dir <dir>`: override the default `<cwd>/.hand/sessions`
    /// storage directory.
    pub session_dir: Option<PathBuf>,
    /// `--no-skills`: skip skill discovery for a reproducible system
    /// prompt.
    pub no_skills: bool,
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

        // Reject typo'd `--provider` values up-front with a clear error.
        // Without this we'd silently fall back to the default
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

        // Load the merged project + global settings. Used as the
        // fallback layer below `--provider`/`--model`/`--thinking`
        // CLI flags. A read failure (missing dir, malformed YAML
        // already surfaced upstream) silently drops to defaults so a
        // bad project file never blocks `--help`-style smoke runs.
        let settings_manager = crate::core::settings::SettingsManager::from_cwd(&cwd).ok();
        let settings_defaults = settings_manager.as_ref().map(|m| m.current());
        let settings_provider: Option<&str> =
            settings_defaults.and_then(|s| s.default_provider.as_deref());
        let settings_model: Option<&str> =
            settings_defaults.and_then(|s| s.default_model.as_deref());

        // Model: provider-default unless `--model` is explicit; thinking-level
        // CLI flag wins over the suffix embedded in the model pattern.
        //
        // Provider selection precedence (highest first):
        //
        // 1. `--provider` flag — explicit caller intent.
        // 2. Project / global `default_provider` from `.hand/settings.yaml`.
        //    Issue #16 / UAT-013: a user with the YAML set to
        //    `anthropic` was instead landing on whichever provider
        //    `pick_default_provider`'s auth-walk found first (e.g.
        //    `zai`), making the setting silently ignored.
        // 3. Slashed `--model a/b` defers to the resolver — the slash
        //    drives routing (e.g. `--model deepseek/deepseek-r1` →
        //    openrouter).
        // 4. Bare `--model <id>` (no slash) looks the id up in the
        //    catalogue. If exactly one provider hosts that id, use it.
        //    This prevents `--model gemini-2.5-flash` from silently
        //    falling back to anthropic and erroring on auth.
        // 5. No `--model` at all auto-picks the first configured
        //    provider (auth.json record OR env-var key) in a known
        //    priority order. So a user with only OPENROUTER_API_KEY
        //    exported lands on openrouter rather than anthropic.
        let explicit_provider = args.provider.as_deref();
        let auto_picked: Option<String> = if explicit_provider.is_some() {
            None
        } else if let Some(p) = settings_provider {
            Some(p.to_string())
        } else if let Some(ref model_pat) = args.model {
            // Only attempt inference for bare ids; slashed ids
            // already self-route in resolve_model(None, …) below.
            if model_pat.contains('/') {
                None
            } else {
                model_resolver::infer_provider_for_model_id(
                    model_pat,
                    PROVIDER_PRIORITY,
                )
            }
        } else {
            Some(pick_default_provider())
        };
        // Effective provider: explicit > auto-picked > "anthropic" hard-default.
        let effective_provider: String = explicit_provider
            .map(String::from)
            .or_else(|| auto_picked.clone())
            .unwrap_or_else(|| "anthropic".to_string());
        // Model pattern precedence: `--model` flag > settings.default_model >
        // provider's catalogue default. Settings-driven model defaults
        // round out the UAT-013 fix so users can pin "anthropic +
        // claude-opus-4-7" in YAML and have both halves honoured.
        let model_pattern = args
            .model
            .as_deref()
            .or(settings_model)
            .unwrap_or_else(|| {
                model_resolver::default_model_for_provider(effective_provider.as_str())
            });
        let mut resolved = if explicit_provider.is_none() && auto_picked.is_none()
            && model_pattern.contains('/')
        {
            // Only the gateway-style slash routing fires when NO provider
            // is locked in (neither explicit nor auto-picked). When
            // auto-picked an openrouter default like
            // `anthropic/claude-sonnet-4-20250514`, we must keep
            // openrouter as the provider — re-routing on the slash would
            // pivot to anthropic and silently fail auth.
            model_resolver::resolve_model(None, model_pattern)
        } else {
            model_resolver::resolve_model(Some(effective_provider.as_str()), model_pattern)
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

        // Thinking-level precedence (highest first):
        //   1. `--thinking` flag (typo → warn + fall through).
        //   2. `default_thinking_level` from `.hand/settings.yaml`.
        //   3. Suffix on the model pattern (`:high`, `:medium`, …).
        //   4. Whatever the model resolver picked (often None).
        // An unrecognised `--thinking` value yields a stderr warning
        // and falls back to the settings-then-pattern chain — silently
        // ignoring would let a typo silently disable reasoning.
        let settings_thinking = settings_defaults
            .and_then(|s| s.default_thinking_level)
            .map(thinking_setting_to_runtime);
        let thinking_level = match args.thinking.as_deref() {
            Some(raw) if !raw.is_empty() => match model_resolver::parse_thinking_level(raw) {
                Some(level) => Some(level),
                None => {
                    eprintln!(
                        "Warning: Invalid thinking level \"{raw}\". \
                         Valid values: off, minimal, low, medium, high, xhigh"
                    );
                    settings_thinking.or(resolved.thinking_level)
                }
            },
            _ => settings_thinking.or(resolved.thinking_level),
        };

        let mut stream_options = SimpleStreamOptions::default();
        if let Some(level) = thinking_level {
            stream_options.reasoning = Some(level);
        }
        // `--api-key` is an explicit override; it must win over env
        // vars / OAuth resolution so users debugging auth issues can
        // pin the exact key going on the wire.
        if let Some(key) = args.api_key.as_deref()
            && !key.is_empty()
        {
            stream_options.base.api_key = Some(key.to_string());
        } else {
            // Otherwise, resolve the API key for the chosen provider
            // from auth.json (the on-disk store) plus env-var fallback.
            // Without this, when the user has `~/.hand/agent/auth.json`
            // configured but the env var is NOT exported, the provider
            // client falls through to its own env-only check and
            // surfaces `No API key for provider: …` even though the
            // key is sitting in auth.json.
            if let Ok(auth_storage) = crate::core::auth_storage::AuthStorage::new()
                && let Some(key) =
                    auth_storage.get_api_key(resolved.model.provider.as_str())
            {
                stream_options.base.api_key = Some(key);
            }
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

        // --append-system-prompt and --system-prompt both auto-load
        // from disk when the value resolves to an existing file —
        // otherwise the argument is treated as literal text. Silent
        // fallthrough to literal on read errors (with a stderr warning)
        // so a transient FS issue doesn't kill the run.
        let custom_system_prompt = args.system_prompt.as_deref().map(resolve_prompt_input);
        // --append-system-prompt can be supplied multiple times. Each
        // value is resolved (literal-or-file), then the non-empty
        // resolved sections are joined by blank lines so the model sees
        // them as distinct paragraphs in the system prompt. None when
        // the flag was never given so the builder skips the section
        // entirely.
        let custom_guidelines = if args.append_system_prompt.is_empty() {
            None
        } else {
            let joined = args
                .append_system_prompt
                .iter()
                .map(|s| resolve_prompt_input(s))
                .filter(|s| !s.trim().is_empty())
                .collect::<Vec<_>>()
                .join("\n\n");
            if joined.is_empty() {
                None
            } else {
                Some(joined)
            }
        };

        Ok(Self {
            cwd,
            model: resolved.model,
            stream_options,
            agent_tools,
            custom_system_prompt,
            custom_guidelines,
            no_session: args.no_session,
            no_context_files: args.no_context_files,
            session_dir: args.session_dir.clone(),
            no_skills: args.no_skills,
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
            no_session: self.no_session,
            no_context_files: self.no_context_files,
            session_dir: self.session_dir.clone(),
            no_skills: self.no_skills,
        }
    }
}

/// Auto-pick a default provider when neither `--provider` nor `--model`
/// is supplied. Two-pass strategy so an explicit `auth.json` always
/// outranks an env-var fallback:
///
/// 1. Look for any provider that has an `auth.json` record (any key
///    that resolves through the on-disk path). The user explicitly
///    registered this provider via `hand --provider X --api-key …`,
///    so respect that intent.
/// 2. Walk the priority list and return the first provider whose
///    env-var (`OPENROUTER_API_KEY`, `GEMINI_API_KEY`, etc.) resolves.
/// 3. Fall back to `"anthropic"` so the eventual error message is
///    actionable.
const PROVIDER_PRIORITY: &[&str] = &[
    "anthropic",
    "openrouter",
    "google",
    "openai",
    "vercel-ai-gateway",
    "zai",
    "deepseek",
    "groq",
    "cerebras",
    "xai",
    "mistral",
    "kimi-coding",
    "huggingface",
    "fireworks",
    "minimax",
];

/// Translate the YAML-shaped [`crate::core::settings::ThinkingLevelSetting`]
/// into the runtime [`model::types::ThinkingLevel`] consumed by the
/// stream options. The two enums are intentionally separate (one is
/// settings-layer with `Off`, the other is provider-layer where
/// `Off` is represented by the absence of a level), so the mapping
/// lives here at the seam.
fn thinking_setting_to_runtime(
    s: crate::core::settings::ThinkingLevelSetting,
) -> model::types::ThinkingLevel {
    use crate::core::settings::ThinkingLevelSetting;
    use model::types::ThinkingLevel;
    match s {
        // `Off` in settings means "explicit no-reasoning". The runtime
        // enum represents that as Minimal — the lowest non-absent
        // tier — because the resolved.thinking_level chain treats
        // `None` as "use the model's natural default", which is the
        // wrong fallback if the user explicitly set Off.
        ThinkingLevelSetting::Off => ThinkingLevel::Minimal,
        ThinkingLevelSetting::Minimal => ThinkingLevel::Minimal,
        ThinkingLevelSetting::Low => ThinkingLevel::Low,
        ThinkingLevelSetting::Medium => ThinkingLevel::Medium,
        ThinkingLevelSetting::High => ThinkingLevel::High,
        ThinkingLevelSetting::Xhigh => ThinkingLevel::Xhigh,
    }
}

fn pick_default_provider() -> String {
    let auth = match crate::core::auth_storage::AuthStorage::new() {
        Ok(a) => a,
        Err(_) => return "anthropic".to_string(),
    };
    // Pass 1: explicit on-disk auth.json record wins. Walk the
    // priority list in order so the result is deterministic when
    // multiple providers are configured.
    if let Ok(records) = auth.load() {
        for provider in PROVIDER_PRIORITY {
            if records.contains_key(*provider) {
                return (*provider).to_string();
            }
        }
    }
    // Pass 2: env-var fallback. Walk the priority list again, this
    // time hitting `get_api_key` (which layers env vars in) but
    // skipping providers that already lost pass 1.
    for provider in PROVIDER_PRIORITY {
        if auth.get_api_key(provider).is_some() {
            return (*provider).to_string();
        }
    }
    "anthropic".to_string()
}

/// Resolve a prompt input string. If `input` resolves to an existing
/// file on disk, return that file's contents; otherwise return `input`
/// verbatim as the literal prompt text. A read error (e.g. permission
/// denied on an existing path) emits a stderr warning and falls
/// through to the literal value rather than aborting setup.
///
/// Used for both `--system-prompt` and `--append-system-prompt` so
/// users can pin guidelines/prompts in a file and feed the path on
/// the CLI without an `@` sigil.
pub(crate) fn resolve_prompt_input(input: &str) -> String {
    if input.is_empty() {
        return String::new();
    }
    let path = std::path::Path::new(input);
    if path.exists()
        && let Ok(meta) = std::fs::metadata(path)
        && meta.is_file()
    {
        match std::fs::read_to_string(path) {
            Ok(content) => return content,
            Err(e) => {
                eprintln!(
                    "Warning: could not read prompt file {}: {e}",
                    path.display()
                );
            }
        }
    }
    input.to_string()
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

    /// A typo'd `--provider` must surface a clean "Unknown provider"
    /// error rather than silently falling back to the default provider
    /// and then erroring on a missing API key further downstream. The
    /// message text is stable so scripts can pattern-match against it.
    #[test]
    fn unknown_provider_returns_descriptive_error() {
        let args = Args::try_parse_from(["hand", "--provider", "nonexistent", "--model", "fake"])
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

    /// `--no-session` must propagate from CLI args through SessionSetup
    /// into AgentSessionConfig so the session manager runs in-memory
    /// and no JSONL file is written under `.hand/sessions/`. The
    /// default (flag absent) keeps persistence on.
    #[test]
    fn no_session_flag_propagates_to_config() {
        let args = Args::try_parse_from(["hand", "--no-session"]).expect("parse");
        let setup = SessionSetup::resolve(&args).expect("resolve");
        assert!(setup.no_session, "setup must carry the flag");
        let cfg = setup.to_config(None);
        assert!(cfg.no_session, "to_config must propagate the flag");
    }

    /// `--api-key` must flow into `stream_options.base.api_key` so the
    /// request hits the wire with the user-supplied credential. An
    /// earlier implementation parsed the flag but never wired it up —
    /// `hand --api-key bogus` silently fell back to env vars / stored
    /// creds, masking the user's intent.
    #[test]
    fn api_key_flag_populates_stream_options() {
        let args =
            Args::try_parse_from(["hand", "--api-key", "sk-test-override-12345"]).expect("parse");
        let setup = SessionSetup::resolve(&args).expect("resolve");
        assert_eq!(
            setup.stream_options.base.api_key.as_deref(),
            Some("sk-test-override-12345"),
        );
    }

    /// Issue #16 / UAT-013: a project `.hand/settings.yaml` with
    /// `default-provider: anthropic` and `default-thinking-level: high`
    /// must drive the session's effective provider and thinking level,
    /// even when no CLI flag is given. The prior bug was that
    /// `SessionSetup::resolve` only consulted CLI args + auth.json,
    /// silently ignoring the YAML and landing on whatever provider
    /// `pick_default_provider`'s auth-walk found first (e.g. `zai`).
    #[test]
    fn settings_yaml_drives_provider_and_thinking_defaults() {
        let tmp = tempfile::TempDir::new().expect("tmp");
        let cwd = tmp.path();
        std::fs::create_dir_all(cwd.join(".hand")).unwrap();
        std::fs::write(
            cwd.join(".hand/settings.yaml"),
            "default-provider: anthropic\ndefault-thinking-level: high\n",
        )
        .unwrap();

        let args = Args::try_parse_from([
            "hand",
            "--cwd",
            cwd.to_str().unwrap(),
        ])
        .expect("parse");
        let setup = SessionSetup::resolve(&args).expect("resolve");

        assert_eq!(
            setup.model.provider.as_str(),
            "anthropic",
            "settings default-provider must drive effective provider"
        );
        assert_eq!(
            setup.stream_options.reasoning,
            Some(model::types::ThinkingLevel::High),
            "settings default-thinking-level must drive stream reasoning"
        );
    }

    /// An explicit `--provider` on the CLI must beat the settings
    /// fallback. Precedence is CLI > settings > auto-pick.
    #[test]
    fn cli_provider_overrides_settings_yaml() {
        let tmp = tempfile::TempDir::new().expect("tmp");
        let cwd = tmp.path();
        std::fs::create_dir_all(cwd.join(".hand")).unwrap();
        std::fs::write(
            cwd.join(".hand/settings.yaml"),
            "default-provider: anthropic\n",
        )
        .unwrap();

        let args = Args::try_parse_from([
            "hand",
            "--cwd",
            cwd.to_str().unwrap(),
            "--provider",
            "openai",
        ])
        .expect("parse");
        let setup = SessionSetup::resolve(&args).expect("resolve");
        assert_eq!(
            setup.model.provider.as_str(),
            "openai",
            "--provider must win over settings.default-provider"
        );
    }

    /// `default-model` from settings.yaml should drive the model
    /// pattern when no `--model` flag was given. CLI flag still wins.
    #[test]
    fn settings_yaml_drives_default_model() {
        let tmp = tempfile::TempDir::new().expect("tmp");
        let cwd = tmp.path();
        std::fs::create_dir_all(cwd.join(".hand")).unwrap();
        std::fs::write(
            cwd.join(".hand/settings.yaml"),
            "default-provider: anthropic\ndefault-model: claude-opus-4-7\n",
        )
        .unwrap();
        let args = Args::try_parse_from([
            "hand",
            "--cwd",
            cwd.to_str().unwrap(),
        ])
        .expect("parse");
        let setup = SessionSetup::resolve(&args).expect("resolve");
        assert!(
            setup.model.id.contains("opus-4-7") || setup.model.id.contains("opus-4.7"),
            "settings default-model must drive model id, got {}",
            setup.model.id
        );
    }

    #[test]
    fn default_args_persist_sessions() {
        let args = Args::try_parse_from(["hand"]).expect("parse");
        let setup = SessionSetup::resolve(&args).expect("resolve");
        assert!(!setup.no_session);
        let cfg = setup.to_config(None);
        assert!(!cfg.no_session);
    }

    /// `--no-context-files` must propagate so that AgentSession skips
    /// HAND.md / .hand/context.md loading at system-prompt build time.
    /// Default keeps the load-everything behavior.
    #[test]
    fn no_context_files_flag_propagates() {
        let args = Args::try_parse_from(["hand", "--no-context-files"]).expect("parse");
        let setup = SessionSetup::resolve(&args).expect("resolve");
        assert!(setup.no_context_files);
        let cfg = setup.to_config(None);
        assert!(cfg.no_context_files);
    }

    #[test]
    fn default_loads_context_files() {
        let args = Args::try_parse_from(["hand"]).expect("parse");
        let setup = SessionSetup::resolve(&args).expect("resolve");
        assert!(!setup.no_context_files);
        let cfg = setup.to_config(None);
        assert!(!cfg.no_context_files);
    }

    /// `--session-dir <dir>` must flow into
    /// AgentSessionConfig.session_dir so SessionManager writes/reads
    /// under the override path instead of the default
    /// `<cwd>/.hand/sessions`.
    #[test]
    fn session_dir_flag_propagates() {
        let args =
            Args::try_parse_from(["hand", "--session-dir", "/tmp/custom-sessions"]).expect("parse");
        let setup = SessionSetup::resolve(&args).expect("resolve");
        assert_eq!(
            setup.session_dir.as_deref(),
            Some(std::path::Path::new("/tmp/custom-sessions")),
        );
        let cfg = setup.to_config(None);
        assert_eq!(
            cfg.session_dir.as_deref(),
            Some(std::path::Path::new("/tmp/custom-sessions")),
        );
    }

    /// --system-prompt and --append-system-prompt auto-load file
    /// contents when the value resolves to an existing file. Non-file
    /// values pass through as literal text.
    #[test]
    fn resolve_prompt_input_reads_existing_file() {
        let path = std::env::temp_dir().join(format!(
            "hand-prompt-load-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::write(&path, "loaded from disk").unwrap();
        let got = resolve_prompt_input(&path.display().to_string());
        assert_eq!(got, "loaded from disk");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn resolve_prompt_input_passes_literal_text_through() {
        let got = resolve_prompt_input("just a sentence, not a path");
        assert_eq!(got, "just a sentence, not a path");
    }

    #[test]
    fn resolve_prompt_input_passes_missing_path_through_as_text() {
        // A path-shaped string that doesn't exist becomes the literal
        // text rather than erroring.
        let got = resolve_prompt_input("/definitely/not/a/real/path/zz.md");
        assert_eq!(got, "/definitely/not/a/real/path/zz.md");
    }

    /// `--append-system-prompt` is repeatable. Each invocation's value
    /// gets concatenated into a single guidelines section, separated by
    /// blank lines.
    #[test]
    fn multiple_append_system_prompts_concatenate() {
        let args = Args::try_parse_from([
            "hand",
            "--append-system-prompt",
            "first directive",
            "--append-system-prompt",
            "second directive",
            "--append-system-prompt",
            "third directive",
        ])
        .expect("parse");
        let setup = SessionSetup::resolve(&args).expect("resolve");
        let guidelines = setup.custom_guidelines.expect("guidelines must be Some");
        assert!(
            guidelines.contains("first directive"),
            "must include first, got: {guidelines}"
        );
        assert!(guidelines.contains("second directive"));
        assert!(guidelines.contains("third directive"));
        // Ordering: first appears before second appears before third.
        let p1 = guidelines.find("first").unwrap();
        let p2 = guidelines.find("second").unwrap();
        let p3 = guidelines.find("third").unwrap();
        assert!(p1 < p2 && p2 < p3, "must preserve invocation order");
    }

    /// No --append-system-prompt invocations → None on SessionSetup
    /// (system prompt builder skips the section entirely).
    #[test]
    fn no_append_system_prompt_produces_none() {
        let args = Args::try_parse_from(["hand"]).expect("parse");
        let setup = SessionSetup::resolve(&args).expect("resolve");
        assert!(setup.custom_guidelines.is_none());
    }

    /// All empty append values collapse to None so the guidelines
    /// section header isn't emitted with no body.
    #[test]
    fn all_empty_append_system_prompts_yield_none() {
        let args = Args::try_parse_from([
            "hand",
            "--append-system-prompt",
            "",
            "--append-system-prompt",
            "   ",
        ])
        .expect("parse");
        let setup = SessionSetup::resolve(&args).expect("resolve");
        assert!(setup.custom_guidelines.is_none());
    }

    #[test]
    fn resolve_prompt_input_empty_stays_empty() {
        assert_eq!(resolve_prompt_input(""), "");
    }

    /// `--session <id>` is accepted as a compatibility alias for
    /// `--resume <id>` so scripts written against other clients work
    /// against this binary unchanged.
    #[test]
    fn session_alias_is_accepted_for_resume() {
        let args = Args::try_parse_from(["hand", "--session", "abc123"]).expect("parse");
        assert_eq!(args.resume.as_deref(), Some("abc123"));
    }

    /// `--no-skills` must propagate so that AgentSession skips skill
    /// discovery entirely. Default keeps the auto-discover behavior.
    #[test]
    fn no_skills_flag_propagates() {
        let args = Args::try_parse_from(["hand", "--no-skills"]).expect("parse");
        let setup = SessionSetup::resolve(&args).expect("resolve");
        assert!(setup.no_skills);
        let cfg = setup.to_config(None);
        assert!(cfg.no_skills);
    }

    #[test]
    fn default_discovers_skills() {
        let args = Args::try_parse_from(["hand"]).expect("parse");
        let setup = SessionSetup::resolve(&args).expect("resolve");
        assert!(!setup.no_skills);
        let cfg = setup.to_config(None);
        assert!(!cfg.no_skills);
    }

    #[test]
    fn default_session_dir_is_none() {
        let args = Args::try_parse_from(["hand"]).expect("parse");
        let setup = SessionSetup::resolve(&args).expect("resolve");
        assert!(setup.session_dir.is_none());
    }

    /// Known providers (in the registry) must still resolve.
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
        assert_eq!(
            setup.model.provider.as_str(),
            "openrouter",
            "explicit --provider must NOT cross over to native deepseek even when openrouter lacks the model"
        );
    }
}
