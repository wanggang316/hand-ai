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

    /// Continue the most recent session. Mutually exclusive with
    /// `--resume <id>` -- specifying both was previously a silent
    /// noop: the bogus id got dropped and `--continue` won, hiding
    /// typos. clap surfaces a clear conflict-with error instead (#80).
    #[arg(short, long = "continue", conflicts_with = "resume")]
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
    /// Mutually exclusive with `--tools <list>` -- pairing them used
    /// to silently drop the explicit whitelist and run with no
    /// tools at all, faking a "I read /etc/hosts" reply the model
    /// fabricated (#83, sibling of #80 / #82).
    #[arg(long, conflicts_with = "tools")]
    pub no_tools: bool,

    /// Disable hand's built-in tools, leaving only extension-provided
    /// tools registered. `-nbt` is the short-form alias.
    #[arg(long)]
    pub no_builtin_tools: bool,

    /// Run in ephemeral mode (don't save session). Mutually
    /// exclusive with `--continue` -- combining the two used to
    /// silently drop --continue and run a fresh ephemeral session,
    /// surprising users who had --continue baked into a shell alias
    /// (#82, sibling of #80). clap surfaces a clean conflict error
    /// instead. --resume/--fork are intentionally NOT in the conflict
    /// list: those load a past session's history but writes (when
    /// --no-session) stay in memory, which is a reasonable mode.
    #[arg(long, conflicts_with = "continue_session")]
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

    /// Disable skill discovery (project, user, and builtin). Useful
    /// when scripts need a baseline system prompt that doesn't pick
    /// up auto-discovered skill files from user dotfiles. Mutually
    /// exclusive with `--skill <path>` -- pairing them used to
    /// silently drop the explicit path (#83).
    #[arg(long, conflicts_with = "skills")]
    pub no_skills: bool,

    /// Load an extra extension by path (repeatable). Each entry points
    /// at a subprocess-extension binary or directory, so scripts can
    /// list extensions on the CLI without writing a settings entry.
    #[arg(short = 'e', long = "extension")]
    pub extensions: Vec<String>,

    /// Disable all extension loading. Mutually exclusive with
    /// `--extension <path>` -- pairing them used to silently
    /// suppress the explicit override path (#83). The diagnostics-
    /// inspection use case noted in the previous doc-comment is
    /// rolled into the clap conflict error message.
    #[arg(long, conflicts_with = "extensions")]
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
    /// Mutually exclusive with `--prompt-template <path>` -- pairing
    /// them used to silently drop the explicit path (#83).
    #[arg(long, conflicts_with = "prompt_templates")]
    pub no_prompt_templates: bool,

    /// Load an extra theme path (repeatable).
    #[arg(long = "theme")]
    pub themes: Vec<String>,

    /// Disable theme discovery (project, user, and builtin).
    /// Mutually exclusive with `--theme <path>` -- pairing them
    /// used to silently drop the explicit path (#83).
    #[arg(long, conflicts_with = "themes")]
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

    /// Expand a leading `~` / `~/` to the user's home directory in
    /// every CLI field that takes a path. Run AFTER clap parsing,
    /// BEFORE downstream consumers see the values, so `--resume
    /// ~/sess.jsonl`, `--export ~/out.html`, `--session-dir
    /// ~/sessions`, `--cwd ~/proj`, `--skill ~/skills/foo`, etc. all
    /// work the same way the shell would expand them (#79). Fields
    /// that may carry literal text (--system-prompt,
    /// --append-system-prompt) only get expanded when the value
    /// actually starts with `~` or `~/`, so literal prose that does
    /// not start with a tilde is untouched.
    pub fn expand_tilde_paths(&mut self) {
        if let Some(ref s) = self.resume {
            self.resume = Some(expand_tilde(s));
        }
        if let Some(ref s) = self.fork {
            self.fork = Some(expand_tilde(s));
        }
        if let Some(ref p) = self.cwd {
            self.cwd = Some(expand_tilde_pathbuf(p));
        }
        if let Some(ref p) = self.session_dir {
            self.session_dir = Some(expand_tilde_pathbuf(p));
        }
        if let Some(ref p) = self.export {
            self.export = Some(expand_tilde_pathbuf(p));
        }
        if let Some(ref s) = self.system_prompt {
            self.system_prompt = Some(expand_tilde(s));
        }
        for s in self.append_system_prompt.iter_mut() {
            *s = expand_tilde(s);
        }
        for s in self.extensions.iter_mut() {
            *s = expand_tilde(s);
        }
        for s in self.skills.iter_mut() {
            *s = expand_tilde(s);
        }
        for s in self.themes.iter_mut() {
            *s = expand_tilde(s);
        }
        for s in self.prompt_templates.iter_mut() {
            *s = expand_tilde(s);
        }
    }
}

/// Replace a leading `~` / `~/` with the user's home directory.
/// Returns the input verbatim when it does not start with a tilde
/// or when `dirs::home_dir()` fails. Internal helper for
/// [`Args::expand_tilde_paths`]; the bigger
/// [`crate::tools::path_utils::expand_path`] helper also strips a
/// leading `@` sigil which is the wrong semantics for CLI flags.
fn expand_tilde(s: &str) -> String {
    if s == "~" {
        return dirs::home_dir()
            .map(|h| h.to_string_lossy().into_owned())
            .unwrap_or_else(|| s.to_string());
    }
    if let Some(rest) = s.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest).to_string_lossy().into_owned();
    }
    s.to_string()
}

fn expand_tilde_pathbuf(p: &std::path::Path) -> PathBuf {
    let s = p.to_string_lossy();
    if s == "~" || s.starts_with("~/") {
        PathBuf::from(expand_tilde(&s))
    } else {
        p.to_path_buf()
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

    /// UC-args-052 (post-#83) -- `--no-tools` and `--tools <csv>` are
    /// mutually exclusive at parse time. Pre-#83, the combination
    /// was permitted on the theory that --no-tools would silently
    /// win at runtime; in practice the model fabricated tool
    /// results (e.g. claiming to have read /etc/hosts) because the
    /// user's explicit whitelist was nuked without warning. clap
    /// surfaces an ArgumentConflict instead so wrappers and
    /// interactive users have to settle precedence explicitly
    /// (sibling of #80 / #82).
    #[test]
    fn no_tools_and_tools_are_mutually_exclusive() {
        let result = Args::try_parse_from(["hand", "--no-tools", "--tools", "read"]);
        let err = match result {
            Ok(_) => panic!("conflicting flags must error at parse time"),
            Err(e) => e,
        };
        assert!(
            matches!(err.kind(), clap::error::ErrorKind::ArgumentConflict),
            "expected ArgumentConflict, got {:?}: {err}",
            err.kind()
        );
        let msg = err.to_string();
        assert!(
            msg.contains("--no-tools"),
            "error must name --no-tools: {msg}"
        );
        assert!(msg.contains("--tools"), "error must name --tools: {msg}");
    }

    /// Regression for #83: every `--no-X` flag that has an explicit
    /// additive sibling now errors at parse time when paired. This
    /// table pins all five pairs in one place so a future flag
    /// addition cannot quietly slip through without picking a
    /// precedence rule.
    #[test]
    fn no_x_flags_conflict_with_their_additive_siblings() {
        let cases: &[(&str, &[&str])] = &[
            ("--no-skills", &["--skill", "/tmp/skill"]),
            ("--no-extensions", &["--extension", "/tmp/ext"]),
            ("--no-themes", &["--theme", "/tmp/theme"]),
            ("--no-prompt-templates", &["--prompt-template", "/tmp/tpl"]),
        ];
        for (negative, additive_argv) in cases {
            let mut argv = vec!["hand", negative];
            argv.extend_from_slice(additive_argv);
            let result = Args::try_parse_from(&argv);
            let err = match result {
                Ok(_) => panic!("{negative} + {additive_argv:?} must conflict"),
                Err(e) => e,
            };
            assert!(
                matches!(err.kind(), clap::error::ErrorKind::ArgumentConflict),
                "{negative} + {additive_argv:?}: expected ArgumentConflict, got {:?}: {err}",
                err.kind()
            );
        }
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

    /// Regression for #79: a leading `~` in CLI path flags must
    /// expand to the user's home dir. Sibling fix to #44 (which
    /// covered the slash-command surface but not the CLI flags).
    /// Verify across a representative subset of fields: Option<String>
    /// (resume / fork), Option<PathBuf> (export / session-dir / cwd),
    /// and Vec<String> (skills / extensions).
    #[test]
    fn expand_tilde_paths_replaces_leading_tilde_across_flags() {
        // dirs::home_dir() must be available on the test host; the
        // assertion below depends on it returning Some.
        let home = match dirs::home_dir() {
            Some(h) => h,
            None => return,
        };
        let home_str = home.to_string_lossy().into_owned();

        let mut args = Args::try_parse_from([
            "hand",
            "--resume",
            "~/sess.jsonl",
            "--fork",
            "~/forks/abc.jsonl",
            "--export",
            "~/out.html",
            "--session-dir",
            "~/sessions",
            "--cwd",
            "~/proj",
            "--system-prompt",
            "~/prompts/sys.txt",
            "--append-system-prompt",
            "~/prompts/extra.txt",
            "--skill",
            "~/skills/a",
            "--skill",
            "~/skills/b",
            "--extension",
            "~/ext/p.so",
        ])
        .unwrap();

        // Pre: clap preserves the literal `~`.
        assert!(args.resume.as_deref().unwrap().starts_with('~'));

        args.expand_tilde_paths();

        let expected_resume = format!("{home_str}/sess.jsonl");
        let expected_fork = format!("{home_str}/forks/abc.jsonl");
        let expected_export = home.join("out.html");
        let expected_sessions = home.join("sessions");
        let expected_cwd = home.join("proj");
        let expected_sys = format!("{home_str}/prompts/sys.txt");
        let expected_append = format!("{home_str}/prompts/extra.txt");
        assert_eq!(args.resume.as_deref(), Some(expected_resume.as_str()));
        assert_eq!(args.fork.as_deref(), Some(expected_fork.as_str()));
        assert_eq!(args.export.as_deref(), Some(expected_export.as_path()));
        assert_eq!(
            args.session_dir.as_deref(),
            Some(expected_sessions.as_path())
        );
        assert_eq!(args.cwd.as_deref(), Some(expected_cwd.as_path()));
        assert_eq!(args.system_prompt.as_deref(), Some(expected_sys.as_str()));
        assert_eq!(args.append_system_prompt, vec![expected_append]);
        assert_eq!(
            args.skills,
            vec![
                format!("{home_str}/skills/a"),
                format!("{home_str}/skills/b"),
            ]
        );
        assert_eq!(args.extensions, vec![format!("{home_str}/ext/p.so")]);
    }

    /// Boundary: paths that do NOT start with `~` are returned
    /// verbatim. A literal `--system-prompt "be concise"` must not
    /// get rewritten by the expander even though strings are the
    /// same field type.
    #[test]
    fn expand_tilde_paths_leaves_non_tilde_values_untouched() {
        let mut args = Args::try_parse_from([
            "hand",
            "--system-prompt",
            "be concise",
            "--resume",
            "s_abc123",
            "--skill",
            "/abs/path",
            "--skill",
            "relative/path",
        ])
        .unwrap();
        args.expand_tilde_paths();
        assert_eq!(args.system_prompt.as_deref(), Some("be concise"));
        assert_eq!(args.resume.as_deref(), Some("s_abc123"));
        assert_eq!(args.skills, vec!["/abs/path", "relative/path"]);
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
        assert!(
            lower.contains("--system-prompt"),
            "help missing flag: {help}"
        );
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

    /// Every `--<long-flag>` advertised by `--help` must appear in the
    /// `crates/coding-agent/README.md` CLI Reference section so the
    /// documentation can't drift out from under the runtime surface.
    /// Known exclusions: `--help` (universal, not in clap doc-comments)
    /// and `--session` (documented as an alias of `--resume` in the
    /// same row, but its bare token may not appear).
    #[test]
    fn readme_documents_every_clap_long_flag() {
        use clap::CommandFactory;
        let cmd = Args::command();
        let readme_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md");
        let readme = std::fs::read_to_string(&readme_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", readme_path.display()));
        let mut missing = Vec::new();
        for arg in cmd.get_arguments() {
            let Some(long) = arg.get_long() else { continue };
            if matches!(long, "help" | "session") {
                continue;
            }
            let needle = format!("--{long}");
            if !readme.contains(&needle) {
                missing.push(needle);
            }
        }
        assert!(
            missing.is_empty(),
            "README CLI Reference is missing these clap flags: {missing:?}"
        );
    }

    #[test]
    fn parses_continue_short_and_long() {
        let long = Args::try_parse_from(["hand", "--continue"]).unwrap();
        assert!(long.continue_session);
        let short = Args::try_parse_from(["hand", "-c"]).unwrap();
        assert!(short.continue_session);
    }

    /// Regression for #80: `--continue` and `--resume <id>` are
    /// mutually exclusive. Before this enforcement, supplying both
    /// silently dropped the resume id and fell back to --continue's
    /// most-recent semantics, hiding typos and silently swapping
    /// the session the user actually wanted. clap surfaces a clean
    /// conflict-with error instead.
    #[test]
    fn continue_and_resume_are_mutually_exclusive() {
        let result = Args::try_parse_from(["hand", "--continue", "--resume", "s_anything"]);
        let err = match result {
            Ok(_) => panic!("conflicting flags must error at parse time"),
            Err(e) => e,
        };
        let kind = err.kind();
        assert!(
            matches!(kind, clap::error::ErrorKind::ArgumentConflict),
            "expected ArgumentConflict, got {kind:?}: {err}"
        );
        let msg = err.to_string();
        // Mention both flags so users can self-diagnose without
        // re-running with --help.
        assert!(
            msg.contains("--continue"),
            "error must name --continue: {msg}"
        );
        assert!(msg.contains("--resume"), "error must name --resume: {msg}");
    }

    /// `-c -r <id>` (the short-form pair) must also conflict, not
    /// just the long forms.
    #[test]
    fn continue_short_and_resume_short_are_mutually_exclusive() {
        let result = Args::try_parse_from(["hand", "-c", "-r", "s_anything"]);
        let err = match result {
            Ok(_) => panic!("conflicting short flags must error"),
            Err(e) => e,
        };
        assert!(matches!(
            err.kind(),
            clap::error::ErrorKind::ArgumentConflict
        ));
    }

    /// Regression for #82: `--no-session` and `--continue` are
    /// mutually exclusive. Pre-fix, the combination silently dropped
    /// --continue and ran a fresh ephemeral session, surprising
    /// users with --continue baked into a shell alias. clap surfaces
    /// a clean conflict error.
    #[test]
    fn no_session_and_continue_are_mutually_exclusive() {
        let result = Args::try_parse_from(["hand", "--no-session", "--continue"]);
        let err = match result {
            Ok(_) => panic!("conflicting flags must error at parse time"),
            Err(e) => e,
        };
        assert!(
            matches!(err.kind(), clap::error::ErrorKind::ArgumentConflict),
            "expected ArgumentConflict, got {:?}: {err}",
            err.kind()
        );
        let msg = err.to_string();
        assert!(
            msg.contains("--no-session"),
            "error must name --no-session: {msg}"
        );
        assert!(
            msg.contains("--continue"),
            "error must name --continue: {msg}"
        );
    }

    /// `--no-session -c` short-form pair must also error.
    #[test]
    fn no_session_and_continue_short_are_mutually_exclusive() {
        let result = Args::try_parse_from(["hand", "--no-session", "-c"]);
        let err = match result {
            Ok(_) => panic!("conflicting short --continue must error"),
            Err(e) => e,
        };
        assert!(matches!(
            err.kind(),
            clap::error::ErrorKind::ArgumentConflict
        ));
    }

    /// Adjacent surface check: `--no-session --resume <id>` is
    /// intentionally NOT a conflict -- loading a past session's
    /// history into ephemeral memory is a coherent mode. Pin this
    /// non-conflict so a future "tighten everything" patch can't
    /// silently break the documented intent in args.rs.
    #[test]
    fn no_session_with_resume_does_not_conflict() {
        let args = Args::try_parse_from(["hand", "--no-session", "--resume", "s_some_id"]).expect(
            "--no-session --resume is intentionally allowed; load history into in-memory session",
        );
        assert!(args.no_session);
        assert_eq!(args.resume.as_deref(), Some("s_some_id"));
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

    /// Post-#83: `--no-extensions` and `--extension <path>` are
    /// mutually exclusive at parse time. The pre-fix design kept
    /// the explicit `-e` entries on the parsed struct "so
    /// diagnostics could inspect what was requested" while the
    /// runtime silently dropped them; that worst-of-both-worlds
    /// shape is the bug #83 reports. Pin the new conflict so a
    /// future loosening can't silently bring back the swallow.
    #[test]
    fn no_extensions_and_extension_are_mutually_exclusive() {
        let result = Args::try_parse_from(["hand", "--no-extensions", "-e", "a", "-e", "b"]);
        let err = match result {
            Ok(_) => panic!("conflicting flags must error"),
            Err(e) => e,
        };
        assert!(matches!(
            err.kind(),
            clap::error::ErrorKind::ArgumentConflict
        ));
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
