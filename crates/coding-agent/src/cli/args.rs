//! CLI argument definitions for the `hand` binary.
//!
//! The `Args` struct mirrors the top-level command-line surface; it is parsed
//! by clap in `main.rs` and consumed by the dispatcher.

use clap::Parser;
use std::path::PathBuf;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser)]
#[command(
    name = "hand",
    about = "Hand — AI coding agent",
    version = VERSION,
    long_about = "Hand is an interactive AI coding agent that helps you write, edit, and understand code.",
    // Disable clap's auto-generated `-V/--version` so we can rebind
    // `-v` to `--version` without it colliding with `-v/--verbose`.
    // The replacement is declared as an explicit `Args::version_flag`
    // field below using ArgAction::Version.
    disable_version_flag = true,
)]
pub struct Args {
    /// Initial prompt (non-interactive mode). Accepts values that start
    /// with `-`/`--` (e.g. a markdown body whose first line is a YAML
    /// frontmatter fence `---`) so a piped prompt isn't mistaken for a
    /// flag.
    ///
    /// `-p` is intentionally NOT bound here: it is reserved as the
    /// short form of `--print` (a boolean toggle, not a value-taking
    /// flag), so `--prompt` is long-form only. Under `--print`, supply
    /// the prompt as a positional message (`hand -p "say hi"`) or via
    /// `--prompt "say hi"`.
    #[arg(long, allow_hyphen_values = true)]
    pub prompt: Option<String>,

    /// Model pattern (e.g., "sonnet", "claude-sonnet:high", "openai/gpt-4o")
    #[arg(short, long)]
    pub model: Option<String>,

    /// Provider (e.g., "anthropic", "openai", "google")
    #[arg(long)]
    pub provider: Option<String>,

    /// API key override (not persisted)
    #[arg(long)]
    pub api_key: Option<String>,

    /// Custom base URL for the provider (e.g.
    /// `https://open.bigmodel.cn/api/anthropic`). Useful for self-hosted
    /// proxies or alternative endpoints when the model isn't in the
    /// catalogue. Not persisted.
    #[arg(long)]
    pub base_url: Option<String>,

    /// Resume a previous session by ID (or path). `--session` is an
    /// alias. A bare `--resume` or `-r` (no value) resumes the most
    /// recent session; supplying a value selects an explicit session
    /// id or path.
    #[arg(short, long, alias = "session", num_args = 0..=1, default_missing_value = "")]
    pub resume: Option<String>,

    /// Continue the most recent session
    #[arg(short, long = "continue")]
    pub continue_session: bool,

    /// Fork from a session file path or ID prefix
    #[arg(long)]
    pub fork: Option<String>,

    /// Working directory
    #[arg(short = 'd', long)]
    pub cwd: Option<PathBuf>,

    /// Custom system prompt (overrides default). Auto-loaded from
    /// disk when the value resolves to an existing file path; otherwise
    /// treated as literal text.
    #[arg(long)]
    pub system_prompt: Option<String>,

    /// Append text or file contents to the system prompt. Can be used
    /// multiple times; the values are concatenated in order, joined by
    /// blank lines. Each value is auto-loaded from disk when it resolves
    /// to an existing file path.
    #[arg(long, action = clap::ArgAction::Append)]
    pub append_system_prompt: Vec<String>,

    /// Thinking level: off, minimal, low, medium, high, xhigh
    #[arg(long)]
    pub thinking: Option<String>,

    /// Comma-separated tools to enable (read,write,edit,bash,grep,find,ls)
    #[arg(short = 't', long)]
    pub tools: Option<String>,

    /// Disable all tools. `-nt` is accepted as a short-form alias.
    #[arg(long)]
    pub no_tools: bool,

    /// Disable hand's built-in tools, leaving only extension-provided
    /// tools registered. `-nbt` is the short-form alias.
    #[arg(long)]
    pub no_builtin_tools: bool,

    /// Run in ephemeral mode (don't save session)
    #[arg(long)]
    pub no_session: bool,

    /// Disable auto-loading of project context files (HAND.md,
    /// .hand/context.md). Useful when scripts need a reproducible system
    /// prompt that doesn't pick up uncommitted local files. `-nc` is
    /// the short-form alias.
    #[arg(long)]
    pub no_context_files: bool,

    /// Override the directory used for session storage. Defaults to
    /// `~/.hand/agent/sessions/<flattened-cwd>/` (or the `base_dir`
    /// override). Useful for CI runs that want sessions written to a
    /// tmpfs / artifact directory, or for embedders that route state
    /// through a custom path. See also `--workspace-sessions` for the
    /// project-local shortcut.
    #[arg(long)]
    pub session_dir: Option<PathBuf>,

    /// Store the session under `<cwd>/.hand/sessions/` instead of the
    /// home-based default. Equivalent to passing
    /// `--session-dir <cwd>/.hand/sessions` but resolves the cwd at
    /// run time so the same invocation works from any directory.
    /// Ignored when `--session-dir` is also given (the explicit path
    /// wins).
    #[arg(long)]
    pub workspace_sessions: bool,

    /// Disable skill discovery (project, user, and builtin). Useful when
    /// scripts need a baseline system prompt that doesn't pick up
    /// auto-discovered skill files from user dotfiles.
    #[arg(long)]
    pub no_skills: bool,

    /// Load an extra extension by path (repeatable). Each entry points
    /// at a subprocess-extension binary or directory, so scripts can
    /// list extensions on the CLI without writing a settings entry.
    #[arg(short = 'e', long = "extension")]
    pub extensions: Vec<String>,

    /// Disable all extension loading — both the auto-discovered set
    /// and any explicit `--extension` entries. The flag does NOT clear
    /// the explicit list (so it can be inspected for diagnostics) but
    /// the runtime skips registration entirely.
    #[arg(long)]
    pub no_extensions: bool,

    /// Add an extra skill path (repeatable). Each entry points at a
    /// directory whose `SKILL.md` is loaded alongside the
    /// auto-discovered set.
    #[arg(long = "skill")]
    pub skills: Vec<String>,

    /// Load an extra prompt-template path (repeatable). The runtime
    /// no-ops gracefully when the file has no template metadata.
    #[arg(long = "prompt-template")]
    pub prompt_templates: Vec<String>,

    /// Disable prompt-template discovery (project, user, and builtin).
    #[arg(long)]
    pub no_prompt_templates: bool,

    /// Load an extra theme path (repeatable).
    #[arg(long = "theme")]
    pub themes: Vec<String>,

    /// Disable theme discovery (project, user, and builtin).
    #[arg(long)]
    pub no_themes: bool,

    /// Non-interactive print mode. `-p` is a bool, not a value-taking
    /// flag. The prompt comes from a positional message
    /// (`hand -p "say hi"`), `--prompt`, or piped stdin.
    #[arg(short = 'p', long)]
    pub print: bool,

    /// Output mode. `text` (default, final assistant content only) and
    /// `json` (JSONL event stream) apply to --print. `rpc` is an alias
    /// for `--rpc`.
    #[arg(long, default_value = "text", value_parser = ["text", "json", "rpc"])]
    pub mode: String,

    /// Run in headless RPC mode (JSONL on stdin/stdout). Mutually exclusive with --print.
    #[arg(long, conflicts_with = "print")]
    pub rpc: bool,

    /// Export session to file (HTML or JSONL based on extension)
    #[arg(long)]
    pub export: Option<PathBuf>,

    /// List available models (optional search filter)
    #[arg(long)]
    pub list_models: Option<Option<String>>,

    /// Comma-separated list of model patterns to enable for the
    /// session (e.g. `--models gpt-4o,claude-sonnet,gemini-pro`). The
    /// list is split on `,` and each entry resolves through the
    /// normal model registry rules. Distinct from `--list-models`,
    /// which only prints what's available without selecting any.
    #[arg(long, value_delimiter = ',')]
    pub models: Vec<String>,

    /// Enable verbose logging. Note: there is NO `-v` short binding —
    /// `-v` is reserved for `--version`. Use the long form `--verbose`.
    #[arg(long)]
    pub verbose: bool,

    /// Print the binary version and exit. Bound to `-v` and
    /// `--version`; `-V` is also accepted for cargo-style invocations.
    #[arg(short = 'v', short_alias = 'V', long = "version", action = clap::ArgAction::Version)]
    pub version_flag: Option<bool>,

    /// Print system diagnostics and exit
    #[arg(long)]
    pub diagnostics: bool,

    /// Trailing positional arguments. Each entry is either:
    /// - a `@<path>` reference — the leading `@` is stripped and the
    ///   file's contents are loaded into the prompt at run time.
    /// - any other string — plain prompt text.
    // No `trailing_var_arg`: flags are accepted anywhere on the
    // command line. With trailing_var_arg, anything after the first
    // positional arg would also be captured as positional, so
    // `hand "msg" --provider X` would drop `--provider` into the
    // positional vec. Disabling it costs us nothing because we strip
    // `@<path>` tokens at the message-builder level.
    pub positional: Vec<String>,

    /// Suppress all auto-download/network operations. When set, the
    /// binary fetcher (fd/rg auto-install), version-check probes, and
    /// any other outbound network paths return `Ok(None)` instead of
    /// reaching out. Equivalent to setting `HAND_OFFLINE=1`. Useful in
    /// air-gapped CI or when a build needs to pin to whatever's already
    /// on disk.
    #[arg(long)]
    pub offline: bool,
}

impl Args {
    /// The plain-text subset of positional arguments — entries that
    /// do NOT start with `@`.
    pub fn messages(&self) -> Vec<String> {
        self.positional
            .iter()
            .filter(|s| !s.starts_with('@'))
            .cloned()
            .collect()
    }

    /// The `@<path>` subset of positional arguments — entries that
    /// start with `@`, with the leading `@` stripped.
    pub fn file_args(&self) -> Vec<String> {
        self.positional
            .iter()
            .filter_map(|s| s.strip_prefix('@').map(|p| p.to_string()))
            .collect()
    }
}

/// Rewrite multi-character short flags (`-nc`, `-nt`, `-nbt`, …) into
/// their canonical long forms before clap parses argv.
///
/// clap can bind `-X` (single char) or `--name` (long) but not `-Xyz`
/// (multi-char single-dash), so a user scripting `hand -nc` against
/// clap's native parser would see `-n -c` interpreted as two separate
/// short flags — neither of which exists. Pre-rewriting argv keeps
/// the canonical clap shape AND lets user scripts keep working.
///
/// Returns a new Vec; the input is consumed.
pub fn expand_short_aliases(argv: impl IntoIterator<Item = String>) -> Vec<String> {
    argv.into_iter()
        .map(|arg| match arg.as_str() {
            "-nc" => "--no-context-files".to_string(),
            "-nt" => "--no-tools".to_string(),
            "-nbt" => "--no-builtin-tools".to_string(),
            "-ns" => "--no-skills".to_string(),
            "-ne" => "--no-extensions".to_string(),
            "-np" => "--no-prompt-templates".to_string(),
            _ => arg,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_with_no_args() {
        let args = Args::try_parse_from(["hand"]).expect("no-arg parse should succeed");
        assert!(args.prompt.is_none());
        assert!(args.model.is_none());
        assert!(args.provider.is_none());
        assert!(args.api_key.is_none());
        assert!(args.resume.is_none());
        assert!(!args.continue_session);
        assert!(args.fork.is_none());
        assert!(args.cwd.is_none());
        assert!(args.system_prompt.is_none());
        assert!(args.append_system_prompt.is_empty());
        assert!(args.thinking.is_none());
        assert!(args.tools.is_none());
        assert!(!args.no_tools);
        assert!(!args.no_session);
        assert!(!args.print);
        assert!(!args.rpc);
        assert!(args.export.is_none());
        assert!(args.list_models.is_none());
        assert!(!args.verbose);
        assert!(!args.diagnostics);
    }

    #[test]
    fn parses_diagnostics_flag() {
        let args =
            Args::try_parse_from(["hand", "--diagnostics"]).expect("--diagnostics should parse");
        assert!(args.diagnostics);
    }

    #[test]
    fn parses_print_flag() {
        let args = Args::try_parse_from(["hand", "--print"]).expect("--print should parse");
        assert!(args.print);
    }

    /// `-p` is the short form of `--print` (bool), not of `--prompt`.
    /// A bare `-p` toggles print mode; the prompt is supplied via the
    /// positional message or `--prompt`.
    #[test]
    fn dash_p_is_print_flag_not_prompt_value() {
        let args = Args::try_parse_from(["hand", "-p", "hello"]).expect("-p hello should parse");
        assert!(args.print, "-p must set the print bool");
        assert!(
            args.prompt.is_none(),
            "-p must not consume the next token as a --prompt value"
        );
        // The trailing token lands in the positional vec so the
        // initial-message builder picks it up.
        assert_eq!(args.messages(), vec!["hello"]);
    }

    #[test]
    fn parses_long_prompt_only() {
        let args = Args::try_parse_from(["hand", "--prompt", "hello"])
            .expect("--prompt hello should parse");
        assert_eq!(args.prompt, Some("hello".into()));
    }

    /// A prompt that starts with YAML frontmatter must not be rejected as
    /// an unknown flag. Without `allow_hyphen_values`, clap treats any
    /// value beginning with `--` (including `---`) as a flag and bails
    /// before reaching the value parser, so `hand --prompt "---\ntitle..."`
    /// would fail at parse time.
    #[test]
    fn parses_prompt_with_yaml_frontmatter() {
        let prompt = "---\ntitle: hello\n---\nSay hi.";
        let args = Args::try_parse_from(["hand", "--prompt", prompt])
            .expect("frontmatter prompt should parse");
        assert_eq!(args.prompt.as_deref(), Some(prompt));
    }

    #[test]
    fn parses_long_prompt_with_yaml_frontmatter() {
        let prompt = "---\ntitle: hello\n---\nSay hi.";
        let args = Args::try_parse_from(["hand", "--prompt", prompt])
            .expect("frontmatter prompt should parse");
        assert_eq!(args.prompt.as_deref(), Some(prompt));
    }

    #[test]
    fn parses_model() {
        let args = Args::try_parse_from(["hand", "--model", "sonnet:high"])
            .expect("--model <pattern> should parse");
        assert_eq!(args.model, Some("sonnet:high".into()));
    }

    #[test]
    fn parses_tools_csv() {
        let args = Args::try_parse_from(["hand", "--tools", "read,grep"])
            .expect("--tools <csv> should parse");
        assert_eq!(args.tools, Some("read,grep".into()));
    }

    #[test]
    fn parses_no_tools_flag() {
        let args = Args::try_parse_from(["hand", "--no-tools"]).expect("--no-tools should parse");
        assert!(args.no_tools);
    }

    /// UC-args-044 — `--verbose` toggles the verbose flag. Hand
    /// dropped the `-v` short to make room for `--version`, so only
    /// the long form parses (this is documented in the conversion
    /// notes alongside UC-args-002).
    #[test]
    fn parses_verbose_long_form() {
        let args = Args::try_parse_from(["hand", "--verbose"]).expect("--verbose should parse");
        assert!(args.verbose, "--verbose must set the flag");
    }

    /// UC-args-052 — supplying both `--no-tools` and `--tools <csv>`
    /// together is accepted at the parse layer; both fields land on
    /// the `Args` struct unchanged. Precedence (no_tools wins at
    /// runtime tool selection) is enforced downstream rather than at
    /// the clap level so a config-driven `--tools` from a wrapper
    /// script does not error out when an interactive user supplies
    /// `--no-tools` on top.
    #[test]
    fn parses_no_tools_and_tools_together() {
        let args = Args::try_parse_from(["hand", "--no-tools", "--tools", "read"])
            .expect("combination must parse — runtime resolves precedence");
        assert!(args.no_tools, "--no-tools sets the flag");
        assert_eq!(
            args.tools.as_deref(),
            Some("read"),
            "--tools value still lands on the struct"
        );
    }

    /// UC-args-060 — end-to-end "kitchen sink" composition: provider,
    /// model, several extensions, a tools CSV, a positional message,
    /// and a `@file` reference all parse and land on their respective
    /// fields without colliding. This is a smoke test for the
    /// trailing_var_arg + named-flag interaction.
    #[test]
    fn parses_complex_combo_end_to_end() {
        let args = Args::try_parse_from([
            "hand",
            "--provider",
            "anthropic",
            "--model",
            "claude-sonnet-4",
            "--extension",
            "ext-one",
            "--extension",
            "ext-two",
            "--tools",
            "read,grep",
            "review",
            "the",
            "patch",
            "@notes.md",
        ])
        .expect("kitchen-sink combo must parse");
        assert_eq!(args.provider, Some("anthropic".into()));
        assert_eq!(args.model, Some("claude-sonnet-4".into()));
        assert_eq!(args.extensions, vec!["ext-one", "ext-two"]);
        assert_eq!(args.tools, Some("read,grep".into()));
        assert_eq!(args.messages(), vec!["review", "the", "patch"]);
        assert_eq!(args.file_args(), vec!["notes.md"]);
    }

    #[test]
    fn rejects_unknown_flag() {
        let result = Args::try_parse_from(["hand", "--unknown-flag"]);
        assert!(result.is_err(), "unknown flag should be rejected by clap");
    }

    #[test]
    fn parses_rpc_flag() {
        let args = Args::try_parse_from(["hand", "--rpc"]).expect("--rpc should parse");
        assert!(args.rpc);
        assert!(!args.print);
    }

    #[test]
    fn rpc_and_print_are_mutually_exclusive() {
        let result = Args::try_parse_from(["hand", "--rpc", "--print"]);
        assert!(
            result.is_err(),
            "--rpc and --print should be mutually exclusive"
        );
    }

    /// `--offline` is a top-level flag, default false. Wiring through
    /// HAND_OFFLINE happens in main(); this test only pins the parse
    /// surface.
    #[test]
    fn parses_offline_flag() {
        let args = Args::try_parse_from(["hand", "--offline"]).expect("--offline should parse");
        assert!(args.offline);
        let default = Args::try_parse_from(["hand"]).expect("no-arg parse");
        assert!(!default.offline, "default must be false");
    }

    // ===== CLI surface parse tests =====
    //
    // Each flag gets a direct parse-surface test so a future refactor
    // (e.g. renaming a clap field, changing a short alias) shows up in
    // `cargo test` instead of in user scripts.

    #[test]
    fn parses_provider_flag() {
        let args = Args::try_parse_from(["hand", "--provider", "openai"]).unwrap();
        assert_eq!(args.provider.as_deref(), Some("openai"));
    }

    #[test]
    fn parses_api_key_flag() {
        let args = Args::try_parse_from(["hand", "--api-key", "sk-test"]).unwrap();
        assert_eq!(args.api_key.as_deref(), Some("sk-test"));
    }

    #[test]
    fn parses_system_prompt_flag() {
        let args = Args::try_parse_from(["hand", "--system-prompt", "Be concise"]).unwrap();
        assert_eq!(args.system_prompt.as_deref(), Some("Be concise"));
    }

    /// `--system-prompt` auto-loads file contents when the value
    /// resolves to an existing path (same as `--append-system-prompt`).
    /// The doc-comment / `--help` output must mention this so users can
    /// discover the behaviour without reading the source.
    #[test]
    fn system_prompt_help_text_mentions_auto_load() {
        use clap::CommandFactory;
        let mut cmd = Args::command();
        let help = cmd.render_long_help().to_string();
        let lower = help.to_ascii_lowercase();
        // Quick locator — the long flag is present in help.
        assert!(lower.contains("--system-prompt"), "help missing flag: {help}");
        // The sentence/words that document auto-loading on
        // `--system-prompt` specifically — the existing line on
        // `--append-system-prompt` doesn't satisfy users opening
        // `--help` for the override flag.
        let lower_no_ws: String = lower.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            lower_no_ws.contains("auto-loaded from disk")
                && lower_no_ws.contains("existing file path"),
            "--system-prompt help text does not document auto-loading: {help}"
        );
    }

    /// `--append-system-prompt` is repeatable. Each invocation pushes
    /// another entry; main() concatenates them with blank-line
    /// separators when building the system prompt. Critical for scripts
    /// that compose a prompt from multiple sources.
    #[test]
    fn parses_repeated_append_system_prompt() {
        let args = Args::try_parse_from([
            "hand",
            "--append-system-prompt",
            "first",
            "--append-system-prompt",
            "second",
        ])
        .unwrap();
        assert_eq!(args.append_system_prompt, vec!["first", "second"]);
    }

    #[test]
    fn parses_continue_short_and_long() {
        let long = Args::try_parse_from(["hand", "--continue"]).unwrap();
        assert!(long.continue_session);
        let short = Args::try_parse_from(["hand", "-c"]).unwrap();
        assert!(short.continue_session);
    }

    #[test]
    fn parses_resume_short_and_long() {
        let long = Args::try_parse_from(["hand", "--resume", "sess-1"]).unwrap();
        assert_eq!(long.resume.as_deref(), Some("sess-1"));
        let short = Args::try_parse_from(["hand", "-r", "sess-2"]).unwrap();
        assert_eq!(short.resume.as_deref(), Some("sess-2"));
    }

    /// `--session <id>` is an alias for `--resume <id>`. Hand uses
    /// clap's alias mechanism; this test pins the binding so a refactor
    /// that drops the alias would break user scripts.
    #[test]
    fn parses_session_alias_for_resume() {
        let args = Args::try_parse_from(["hand", "--session", "sid-42"]).unwrap();
        assert_eq!(args.resume.as_deref(), Some("sid-42"));
    }

    #[test]
    fn parses_fork_flag() {
        let args = Args::try_parse_from(["hand", "--fork", "src-session"]).unwrap();
        assert_eq!(args.fork.as_deref(), Some("src-session"));
    }

    #[test]
    fn parses_export_flag() {
        let args = Args::try_parse_from(["hand", "--export", "out.html"]).unwrap();
        assert_eq!(
            args.export
                .as_deref()
                .map(|p| p.to_string_lossy().to_string()),
            Some("out.html".to_string())
        );
    }

    #[test]
    fn parses_thinking_flag() {
        let args = Args::try_parse_from(["hand", "--thinking", "high"]).unwrap();
        assert_eq!(args.thinking.as_deref(), Some("high"));
    }

    #[test]
    fn parses_no_session_flag() {
        let args = Args::try_parse_from(["hand", "--no-session"]).unwrap();
        assert!(args.no_session);
    }

    #[test]
    fn parses_no_context_files_flag() {
        let args = Args::try_parse_from(["hand", "--no-context-files"]).unwrap();
        assert!(args.no_context_files);
    }

    #[test]
    fn parses_no_skills_flag() {
        let args = Args::try_parse_from(["hand", "--no-skills"]).unwrap();
        assert!(args.no_skills);
    }

    #[test]
    fn parses_session_dir_flag() {
        let args = Args::try_parse_from(["hand", "--session-dir", "/tmp/sessions"]).unwrap();
        assert_eq!(
            args.session_dir
                .as_deref()
                .map(|p| p.to_string_lossy().to_string()),
            Some("/tmp/sessions".to_string())
        );
    }

    #[test]
    fn parses_mode_text_and_json() {
        let text = Args::try_parse_from(["hand", "--mode", "text"]).unwrap();
        assert_eq!(text.mode, "text");
        let json = Args::try_parse_from(["hand", "--mode", "json"]).unwrap();
        assert_eq!(json.mode, "json");
        // Default mode is "text".
        let default = Args::try_parse_from(["hand"]).unwrap();
        assert_eq!(default.mode, "text");
    }

    /// `--mode rpc` is accepted as an alias for `--rpc`. Main's
    /// dispatcher checks `cli.mode == "rpc"` alongside `cli.rpc`.
    #[test]
    fn parses_mode_rpc() {
        let args = Args::try_parse_from(["hand", "--mode", "rpc"]).unwrap();
        assert_eq!(args.mode, "rpc");
    }

    /// `--mode` rejects values outside the allowed set so a typo
    /// surfaces immediately rather than silently picking the default.
    #[test]
    fn mode_rejects_unknown_value() {
        let res = Args::try_parse_from(["hand", "--mode", "binary"]);
        assert!(res.is_err(), "--mode binary should be rejected");
    }

    /// `-t` is the short form of `--tools` (CSV-shaped value).
    #[test]
    fn parses_tools_short_t() {
        let args = Args::try_parse_from(["hand", "-t", "read,bash"]).unwrap();
        assert_eq!(args.tools.as_deref(), Some("read,bash"));
    }

    /// `-nt` is the short-form alias for `--no-tools`. Because clap
    /// can't bind multi-char tokens to a single `-`, the argv rewrite
    /// `expand_short_aliases` translates `-nt`→`--no-tools` before
    /// clap parses.
    #[test]
    fn nt_short_alias_rewrites_to_no_tools() {
        let argv = expand_short_aliases(vec!["hand".to_string(), "-nt".to_string()]);
        let args = Args::try_parse_from(argv).expect("-nt should rewrite to --no-tools");
        assert!(args.no_tools);
    }

    /// `-nbt` is the short-form alias for `--no-builtin-tools`.
    #[test]
    fn nbt_short_alias_rewrites_to_no_builtin_tools() {
        let argv = expand_short_aliases(vec!["hand".to_string(), "-nbt".to_string()]);
        let args = Args::try_parse_from(argv).expect("-nbt should rewrite to --no-builtin-tools");
        assert!(args.no_builtin_tools);
    }

    /// `-nc` is the short-form alias for `--no-context-files`.
    #[test]
    fn nc_short_alias_rewrites_to_no_context_files() {
        let argv = expand_short_aliases(vec!["hand".to_string(), "-nc".to_string()]);
        let args = Args::try_parse_from(argv).expect("-nc should rewrite to --no-context-files");
        assert!(args.no_context_files);
    }

    /// `--models <csv>` splits on `,` and yields a Vec<String> so the
    /// downstream caller can iterate.
    #[test]
    fn parses_models_csv() {
        let args =
            Args::try_parse_from(["hand", "--models", "gpt-4o,claude-sonnet,gemini-pro"]).unwrap();
        assert_eq!(
            args.models,
            vec![
                "gpt-4o".to_string(),
                "claude-sonnet".to_string(),
                "gemini-pro".to_string(),
            ]
        );
    }

    /// `--no-builtin-tools` parses on its own.
    #[test]
    fn parses_no_builtin_tools_flag() {
        let args =
            Args::try_parse_from(["hand", "--no-builtin-tools"]).expect("--no-builtin-tools");
        assert!(args.no_builtin_tools);
    }

    /// Bare `--resume` (no value following) is accepted; it
    /// resolves to `Some("")` which downstream code interprets as
    /// "resume the most recent session".
    #[test]
    fn parses_bare_resume_without_value() {
        let args = Args::try_parse_from(["hand", "--resume"]).expect("--resume bare");
        assert_eq!(
            args.resume.as_deref(),
            Some(""),
            "bare --resume should land as Some empty string"
        );
    }

    /// Bare `-r` (no value following) mirrors the long-form bare
    /// `--resume`.
    #[test]
    fn parses_bare_resume_short_without_value() {
        let args = Args::try_parse_from(["hand", "-r"]).expect("-r bare");
        assert_eq!(args.resume.as_deref(), Some(""));
    }

    /// `-r <id>` still works — a value following the short flag binds
    /// as the session id/path.
    #[test]
    fn parses_resume_short_with_value_still_works() {
        let args = Args::try_parse_from(["hand", "-r", "session-42"]).unwrap();
        assert_eq!(args.resume.as_deref(), Some("session-42"));
    }

    /// `--extension <path>` (and the `-e` short) collects a Vec of
    /// extension paths. Repeated invocations append in order.
    #[test]
    fn parses_extension_single_and_repeated() {
        let single = Args::try_parse_from(["hand", "--extension", "./my-ext"]).unwrap();
        assert_eq!(single.extensions, vec!["./my-ext".to_string()]);
        let short = Args::try_parse_from(["hand", "-e", "./short-ext"]).unwrap();
        assert_eq!(short.extensions, vec!["./short-ext".to_string()]);
        let repeated = Args::try_parse_from(["hand", "-e", "./a", "--extension", "./b"]).unwrap();
        assert_eq!(
            repeated.extensions,
            vec!["./a".to_string(), "./b".to_string()]
        );
    }

    /// `--no-extensions` is a boolean toggle. It does NOT clear the
    /// `extensions` Vec (so diagnostics can still inspect what was
    /// requested), but the runtime is expected to skip registration
    /// when this flag is set.
    #[test]
    fn parses_no_extensions_with_explicit_entries() {
        let args = Args::try_parse_from(["hand", "--no-extensions", "-e", "a", "-e", "b"]).unwrap();
        assert!(args.no_extensions);
        assert_eq!(args.extensions, vec!["a".to_string(), "b".to_string()]);
    }

    /// Plain-text positional arguments land in `messages()`.
    #[test]
    fn positional_plain_text_lands_in_messages() {
        let args = Args::try_parse_from(["hand", "hello", "world"]).unwrap();
        assert_eq!(
            args.messages(),
            vec!["hello".to_string(), "world".to_string()]
        );
        assert!(args.file_args().is_empty());
    }

    /// `@<path>` positionals land in `file_args()` with the leading
    /// `@` stripped.
    #[test]
    fn positional_at_file_lands_in_file_args() {
        let args = Args::try_parse_from(["hand", "@README.md", "@src/main.ts"]).unwrap();
        assert_eq!(
            args.file_args(),
            vec!["README.md".to_string(), "src/main.ts".to_string()]
        );
        assert!(args.messages().is_empty());
    }

    /// Mixed positional invocation splits correctly: plain text lands
    /// in `messages()`, `@`-prefixed entries land in `file_args()`.
    #[test]
    fn positional_mixed_messages_and_file_args() {
        let args =
            Args::try_parse_from(["hand", "@file.txt", "explain this", "@image.png"]).unwrap();
        assert_eq!(
            args.file_args(),
            vec!["file.txt".to_string(), "image.png".to_string()]
        );
        assert_eq!(args.messages(), vec!["explain this".to_string()]);
    }

    /// `--skill <path>` collects a Vec of skill paths, repeatable.
    #[test]
    fn parses_skill_single_and_repeated() {
        let single = Args::try_parse_from(["hand", "--skill", "./skill-a"]).unwrap();
        assert_eq!(single.skills, vec!["./skill-a".to_string()]);
        let repeated = Args::try_parse_from(["hand", "--skill", "./a", "--skill", "./b"]).unwrap();
        assert_eq!(repeated.skills, vec!["./a".to_string(), "./b".to_string()]);
    }
}
