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
    long_about = "Hand is an interactive AI coding agent that helps you write, edit, and understand code."
)]
pub struct Args {
    /// Initial prompt (non-interactive mode). Accepts values that start
    /// with `-`/`--` (e.g. a markdown body whose first line is a YAML
    /// frontmatter fence `---`) so a piped prompt isn't mistaken for a
    /// flag.
    #[arg(short, long, allow_hyphen_values = true)]
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
    /// alias for the same behavior.
    #[arg(short, long, alias = "session")]
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

    /// Custom system prompt (overrides default)
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
    #[arg(long)]
    pub tools: Option<String>,

    /// Disable all tools
    #[arg(long)]
    pub no_tools: bool,

    /// Run in ephemeral mode (don't save session)
    #[arg(long)]
    pub no_session: bool,

    /// Disable auto-loading of project context files (HAND.md,
    /// .hand/context.md). Useful when scripts need a reproducible system
    /// prompt that doesn't pick up uncommitted local files.
    #[arg(long)]
    pub no_context_files: bool,

    /// Override the directory used for session storage. Defaults to
    /// `<cwd>/.hand/sessions`. Useful for CI runs that want sessions
    /// written to a tmpfs / artifact directory.
    #[arg(long)]
    pub session_dir: Option<PathBuf>,

    /// Disable skill discovery (project, user, and builtin). Useful when
    /// scripts need a baseline system prompt that doesn't pick up
    /// auto-discovered skill files from user dotfiles.
    #[arg(long)]
    pub no_skills: bool,

    /// Non-interactive print mode
    #[arg(long)]
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

    /// Enable verbose logging
    #[arg(short, long)]
    pub verbose: bool,

    /// Print system diagnostics and exit
    #[arg(long)]
    pub diagnostics: bool,

    /// Suppress all auto-download/network operations. When set, the
    /// binary fetcher (fd/rg auto-install), version-check probes, and
    /// any other outbound network paths return `Ok(None)` instead of
    /// reaching out. Equivalent to setting `HAND_OFFLINE=1`. Useful in
    /// air-gapped CI or when a build needs to pin to whatever's already
    /// on disk.
    #[arg(long)]
    pub offline: bool,
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

    #[test]
    fn parses_short_prompt() {
        let args = Args::try_parse_from(["hand", "-p", "hello"]).expect("-p <prompt> should parse");
        assert_eq!(args.prompt, Some("hello".into()));
    }

    /// A prompt that starts with YAML frontmatter must not be rejected as
    /// an unknown flag. Without `allow_hyphen_values`, clap treats any
    /// value beginning with `--` (including `---`) as a flag and bails
    /// before reaching the value parser, so `hand -p "---\ntitle..."`
    /// would fail at parse time.
    #[test]
    fn parses_prompt_with_yaml_frontmatter() {
        let prompt = "---\ntitle: hello\n---\nSay hi.";
        let args =
            Args::try_parse_from(["hand", "-p", prompt]).expect("frontmatter prompt should parse");
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

    /// Pi-mono parity: `--offline` is a top-level flag, default false.
    /// Wiring through HAND_OFFLINE happens in main(); this test only
    /// pins the parse surface.
    #[test]
    fn parses_offline_flag() {
        let args =
            Args::try_parse_from(["hand", "--offline"]).expect("--offline should parse");
        assert!(args.offline);
        let default = Args::try_parse_from(["hand"]).expect("no-arg parse");
        assert!(!default.offline, "default must be false");
    }

    // ===== Pi-mono args.test.ts parity surface =====
    //
    // Each pi test that exercises an existing hand flag gets a direct
    // mirror so a future refactor (e.g. renaming a clap field, changing
    // a short alias) shows up in `cargo test` instead of in user
    // scripts.

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

    /// Pi-mono parity: --append-system-prompt is repeatable. Each invocation
    /// pushes another entry; main() concatenates them with blank-line
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

    /// Pi-mono parity: `--session <id>` is an alias for `--resume <id>`.
    /// Hand uses clap's alias mechanism; this test pins the binding so a
    /// refactor that drops the alias would break the parity contract.
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
        assert_eq!(args.export.as_deref().map(|p| p.to_string_lossy().to_string()),
                   Some("out.html".to_string()));
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
            args.session_dir.as_deref().map(|p| p.to_string_lossy().to_string()),
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

    /// Pi-mono parity: `--mode rpc` is accepted as an alias for `--rpc`
    /// so scripts written against pi's CLI surface keep working. Main's
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
}
