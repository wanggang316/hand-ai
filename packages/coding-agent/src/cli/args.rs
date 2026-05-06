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
    /// Initial prompt (non-interactive mode)
    #[arg(short, long)]
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

    /// Resume a previous session by ID
    #[arg(short, long)]
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

    /// Append text to the system prompt
    #[arg(long)]
    pub append_system_prompt: Option<String>,

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

    /// Non-interactive print mode
    #[arg(long)]
    pub print: bool,

    /// Export session to file (HTML or JSONL based on extension)
    #[arg(long)]
    pub export: Option<PathBuf>,

    /// List available models (optional search filter)
    #[arg(long)]
    pub list_models: Option<Option<String>>,

    /// Enable verbose logging
    #[arg(short, long)]
    pub verbose: bool,
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
        assert!(args.append_system_prompt.is_none());
        assert!(args.thinking.is_none());
        assert!(args.tools.is_none());
        assert!(!args.no_tools);
        assert!(!args.no_session);
        assert!(!args.print);
        assert!(args.export.is_none());
        assert!(args.list_models.is_none());
        assert!(!args.verbose);
    }

    #[test]
    fn parses_print_flag() {
        let args = Args::try_parse_from(["hand", "--print"]).expect("--print should parse");
        assert!(args.print);
    }

    #[test]
    fn parses_short_prompt() {
        let args =
            Args::try_parse_from(["hand", "-p", "hello"]).expect("-p <prompt> should parse");
        assert_eq!(args.prompt, Some("hello".into()));
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
}
