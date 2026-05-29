//! Hand — interactive AI coding agent CLI.

use clap::Parser;
use hand_coding_agent::cli::Args;
use hand_coding_agent::core::agent_session::{AgentSession, AgentSessionConfig, AgentSessionEvent};
use hand_coding_agent::core::diagnostics;
use hand_coding_agent::core::export;
use hand_coding_agent::core::model_resolver;
use hand_coding_agent::core::timings;
use hand_coding_agent::modes;
use hand_coding_agent::modes::interactive::ExportFormat;
use hand_coding_agent::modes::session_setup::SessionSetup;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use tracing_subscriber::EnvFilter;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    timings::reset();
    // Rewrite multi-char short flags (`-nc`, `-nt`, `-nbt`, …) before
    // clap sees argv. Without this, scripts using them would be
    // rejected as e.g. `-n -c` (two unknown shorts).
    let argv = hand_coding_agent::cli::args::expand_short_aliases(std::env::args());
    // Parse errors yield exit 1 (clap's default is 2) and a single-line
    // `Error: <one-line>` message instead of a multi-line usage dump
    // on stderr. Help and version still use clap's built-in handler
    // (exit 0, full output).
    let mut cli = Args::try_parse_from(argv).unwrap_or_else(|e| {
        match e.kind() {
            clap::error::ErrorKind::DisplayHelp
            | clap::error::ErrorKind::DisplayVersion
            | clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
                // Let clap render help/version verbatim, then exit 0.
                e.exit();
            }
            _ => {
                // Collapse the multi-line clap rendering into the
                // single most informative line and surface as
                // `Error: <text>` on stderr, exit 1.
                let raw = format!("{e}");
                let first_line = raw.lines().find(|l| !l.is_empty()).unwrap_or("");
                let cleaned = first_line.trim_start_matches("error: ").trim();
                eprintln!("Error: {cleaned}");
                std::process::exit(1);
            }
        }
    });
    timings::time("parse_args");

    // Expand a leading `~` / `~/` in every CLI flag that takes a
    // path so `hand --resume ~/x.jsonl`, `--export ~/out.html`,
    // `--session-dir ~/sessions`, etc. work the same way the shell
    // would expand them. Sibling fix to #44 for the slash-command
    // surface (#79).
    cli.expand_tilde_paths();

    // Honest warnings for documented-but-unplumbed flags (#64). The
    // runtime doesn't yet wire `--theme`, `--extension`, or
    // `--prompt-template` into the corresponding discovery paths, and
    // silently ignoring them surprised users into thinking the load
    // worked. Surface a one-line warning per flag on stderr so the
    // mismatch is visible. `--skill` is fully plumbed (#63) so it's
    // not in the warning set.
    if !cli.themes.is_empty() {
        eprintln!(
            "warning: --theme is parsed but not yet plumbed into theme discovery; \
             the supplied path(s) have no effect. \
             Add themes via ~/.hand/themes/ or a project .hand/themes/ directory."
        );
    }
    if !cli.extensions.is_empty() {
        eprintln!(
            "warning: --extension is parsed but not yet plumbed; \
             the supplied path(s) have no effect. \
             Configure extensions via ~/.hand/agent/settings.yaml under `extensions:`."
        );
    }
    if !cli.prompt_templates.is_empty() {
        eprintln!(
            "warning: --prompt-template is parsed but not yet plumbed; \
             the supplied path(s) have no effect."
        );
    }

    // `--offline` flips on the same env-var guard the tools-manager
    // already honors. Setting the env var here means every downstream
    // caller (binary fetcher, version checker) sees offline mode
    // without needing to thread an explicit flag.
    if cli.offline {
        // SAFETY: single-threaded at this point — main() hasn't spawned
        // any tasks yet. std::env::set_var is otherwise multi-thread
        // hostile.
        unsafe {
            std::env::set_var("HAND_OFFLINE", "1");
        }
    }

    // upstream-parity stub subcommands. When the first positional matches a
    // upstream extension-management command, we surface a clean
    // "not implemented" exit-1 message instead of treating the keyword
    // as a free-text prompt. Once hand grows the package-manager
    // integration these can dispatch into real handlers.
    if let Some(first) = cli.positional.first()
        && !cli.print
        && cli.prompt.is_none()
        && matches!(
            first.as_str(),
            "install" | "remove" | "uninstall" | "config" | "update" | "list" | "search"
        )
    {
        eprintln!(
            "Error: `hand {first}` is an extension-management subcommand that is not yet implemented.\n\
             For now: edit ~/.hand/agent/settings.yaml directly to manage packages."
        );
        std::process::exit(1);
    }

    // Handle --diagnostics: print system report and exit. Runs before
    // logging setup so the report is the only thing on stdout.
    if cli.diagnostics {
        timings::print();
        let report = diagnostics::run_diagnostics();
        diagnostics::print_report(&report);
        if report.has_errors() {
            std::process::exit(1);
        }
        return Ok(());
    }

    // Handle --list-models — emit a six-column table (provider, model,
    // context, max-out, thinking, images) filtered to providers that
    // have credentials configured (env var or auth.json).
    if let Some(ref search) = cli.list_models {
        let models = hand_coding_agent::cli::list_models_for_cli(search.as_deref());
        if models.is_empty() {
            if let Some(pat) = search.as_deref().filter(|s| !s.is_empty()) {
                println!("No models matching \"{pat}\"");
            } else {
                println!("No models found.");
            }
            return Ok(());
        }
        hand_coding_agent::cli::print_models_table(&models);
        return Ok(());
    }

    // Setup logging
    let filter = if cli.verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::new("warn")
    };
    tracing_subscriber::fmt().with_env_filter(filter).init();

    // Headless RPC dispatch loop: takes precedence over interactive/print modes.
    // `--mode rpc` is an alias for `--rpc`.
    if cli.rpc || cli.mode == "rpc" {
        timings::print();
        return run_rpc(cli).await;
    }

    // Non-interactive print mode: single prompt + exit. Auto-promote
    // when stdin is piped — `echo "msg" | hand` should behave as
    // `echo "msg" | hand --print`, not drop into the line REPL with
    // its banner. Matches what scripts actually expect.
    let auto_print = {
        use std::io::IsTerminal;
        !cli.print && !io::stdin().is_terminal()
    };
    if cli.print || auto_print {
        timings::print();
        return modes::print::run(cli).await;
    }

    // Interactive TUI flow: only when stdin is a real terminal AND no
    // `--prompt`/`--export` workflow was requested. The TUI is a full-screen
    // diff renderer; piping into the binary or running in CI should fall
    // through to the legacy line REPL below so existing automation keeps
    // working.
    {
        use std::io::IsTerminal;
        let tui_eligible = io::stdin().is_terminal()
            && io::stdout().is_terminal()
            && cli.prompt.is_none()
            && cli.export.is_none();
        if tui_eligible {
            timings::print();
            return modes::interactive::run_interactive(cli)
                .await
                .map_err(|e| -> Box<dyn std::error::Error> { e });
        }
    }

    // Legacy line-REPL interactive flow (used when stdin is not a tty, or
    // when `--prompt` / `--export` requires the older subscriber semantics).
    let setup = match SessionSetup::resolve(&cli) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    };
    timings::time("session_setup");
    let cwd = setup.cwd.clone();

    // Determine resume id, mirroring the pre-extraction logic: --continue
    // defers to `SessionManager::continue_recent` below; otherwise --resume
    // is honoured directly. A bare `--resume` (no value) lands as `Some("")`
    // from clap; promote it to `--continue` semantics so users resume the
    // most-recent session instead of seeing `Session "" not found`.
    let bare_resume = matches!(cli.resume.as_deref(), Some(""));
    let continue_like = cli.continue_session || bare_resume;
    let resume_session = if continue_like {
        None
    } else {
        cli.resume.clone()
    };

    let base_config = setup.to_config(resume_session);
    let agent_tools = setup.agent_tools;

    let mut session = if continue_like {
        // Continue most recent session — honouring --session-dir so the
        // search and the resume-open agree on which directory holds the
        // session (#58).
        match hand_coding_agent::SessionManager::continue_recent_in(
            &cwd,
            base_config.session_dir.as_deref(),
        ) {
            Ok(sm) => {
                let config = AgentSessionConfig {
                    resume_session: Some(sm.id().to_string()),
                    ..base_config.clone()
                };
                drop(sm);
                AgentSession::new(config, agent_tools)?
            }
            Err(e) => {
                let _ = e;
                eprintln!("No previous session found. Starting a new session.");
                AgentSession::new(base_config, agent_tools)?
            }
        }
    } else if let Some(ref fork_source) = cli.fork {
        // Fork from existing session
        let fork_path = resolve_session_path_in(
            base_config.session_dir.as_deref(),
            &cwd,
            fork_source,
        );
        match hand_coding_agent::SessionManager::fork_from_in(
            &fork_path,
            &cwd,
            base_config.session_dir.as_deref(),
        ) {
            Ok(sm) => {
                let config = AgentSessionConfig {
                    resume_session: Some(sm.id().to_string()),
                    ..base_config.clone()
                };
                drop(sm);
                AgentSession::new(config, agent_tools)?
            }
            Err(_) => {
                // An explicit --fork <id> that can't be resolved must
                // surface a clear error and exit 1, not silently start
                // a fresh session.
                eprintln!("Error: No session found matching '{fork_source}'");
                std::process::exit(1);
            }
        }
    } else {
        AgentSession::new(base_config, agent_tools)?
    };
    timings::time("session_create");

    // Subscribe to events for output
    session.subscribe(|event| match event {
        AgentSessionEvent::Agent(agent_event) => {
            handle_agent_event(&agent_event);
        }
        AgentSessionEvent::CompactionStart => {
            eprintln!("\x1b[33m[Compacting context...]\x1b[0m");
        }
        AgentSessionEvent::CompactionEnd { .. } => {
            eprintln!("\x1b[33m[Compaction complete]\x1b[0m");
        }
        AgentSessionEvent::Error(err) => {
            eprintln!("\x1b[31mError: {}\x1b[0m", err);
        }
        // SessionInfoChanged is consumed by the interactive driver's
        // footer refresh. main()'s ad-hoc subscriber here is only for
        // the legacy interactive loop's progress notifications, so
        // we silently drop it — the next render tick picks up the
        // new name from session.label().
        AgentSessionEvent::SessionInfoChanged { .. } => {}
    });

    // Handle --export
    if let Some(export_path) = cli.export {
        return handle_export(&session, &export_path);
    }

    timings::print();

    if let Some(prompt) = cli.prompt {
        // Single prompt then interactive
        session.send_message(&prompt).await?;
        run_interactive(&mut session, &cwd).await?;
    } else {
        // Interactive mode
        print_welcome(&session);
        run_interactive(&mut session, &cwd).await?;
    }

    Ok(())
}

/// Run in headless RPC mode: JSONL frames on stdin/stdout, no terminal UI.
///
/// Honors `--no-session` (in-memory session) and `--cwd`/`--model`/`--tools`
/// in the same way as interactive/print modes. SIGINT (Ctrl-C) cancels the
/// dispatcher future cleanly via `tokio::select!`; the writer task exits
/// when its sender is dropped, and the process returns `Ok`.
async fn run_rpc(cli: Args) -> Result<(), Box<dyn std::error::Error>> {
    // F5: --continue / --fork are not yet supported in RPC mode. Surface a
    // one-line warning and fall through; the session below is constructed
    // ignoring those flags.
    if cli.continue_session || cli.fork.is_some() {
        eprintln!("rpc: --continue/--fork are not yet supported in RPC mode (ignored)");
    }

    // F6: --no-session drops thinking level under RPC because in-memory
    // sessions do not yet carry through stream options. Warn the user
    // explicitly so the silent drop is visible.
    if cli.no_session && cli.thinking.is_some() {
        eprintln!(
            "rpc: --thinking is dropped under --no-session in this version (use a persisted session)"
        );
    }

    let setup = match SessionSetup::resolve(&cli) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    };

    // Build the session: persisted to disk by default, in-memory under --no-session.
    let session = if cli.no_session {
        AgentSession::in_memory_with_client(
            setup.model.clone(),
            setup.agent_tools,
            model::Client::new(),
        )
    } else {
        let config = setup.to_config(cli.resume.clone());
        AgentSession::new(config, setup.agent_tools)?
    };

    let stdin = tokio::io::BufReader::new(tokio::io::stdin());
    let stdout = tokio::io::stdout();

    tokio::select! {
        result = hand_coding_agent::rpc::run_rpc_server(stdin, stdout, session) => result?,
        _ = tokio::signal::ctrl_c() => {
            eprintln!("rpc: received SIGINT, shutting down");
            // Dropping the dispatcher future closes its mpsc senders; the
            // writer task drains and exits.
        }
    }
    Ok(())
}

async fn run_interactive(
    session: &mut AgentSession,
    cwd: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    loop {
        print!("\n\x1b[1;35m>\x1b[0m ");
        stdout.flush()?;

        let mut input = String::new();
        if stdin.lock().read_line(&mut input)? == 0 {
            break; // EOF
        }

        let input = input.trim();
        if input.is_empty() {
            continue;
        }

        // Handle file include syntax: @filepath
        let input = if let Some(path) = input.strip_prefix('@') {
            match std::fs::read_to_string(path) {
                Ok(content) => content,
                Err(e) => {
                    eprintln!("\x1b[31mFailed to read {}: {}\x1b[0m", path, e);
                    continue;
                }
            }
        } else {
            input.to_string()
        };

        // Handle shell command shortcuts
        if let Some(cmd) = input.strip_prefix("!!") {
            // Execute without adding to context
            execute_shell(cmd);
            continue;
        }
        if let Some(cmd) = input.strip_prefix('!') {
            // Execute and add to context
            let output = execute_shell_capture(cmd);
            let context_msg = format!(
                "I ran this shell command:\n```\n$ {}\n```\nOutput:\n```\n{}\n```",
                cmd, output
            );
            match session.send_message(&context_msg).await {
                Ok(_) => {}
                Err(e) => eprintln!("\x1b[31mError: {}\x1b[0m", e),
            }
            continue;
        }

        // Handle slash commands
        if input.starts_with('/') {
            if handle_slash_command(&input, session, cwd).await? {
                break;
            }
            continue;
        }

        match session.send_message(&input).await {
            Ok(_) => {}
            Err(e) => {
                eprintln!("\x1b[31mError: {}\x1b[0m", e);
            }
        }
    }

    Ok(())
}

/// Handle a slash command. Returns true if the session should quit.
async fn handle_slash_command(
    input: &str,
    session: &mut AgentSession,
    cwd: &Path,
) -> Result<bool, Box<dyn std::error::Error>> {
    let parts: Vec<&str> = input.splitn(2, ' ').collect();
    let command = parts[0];
    let args = parts.get(1).copied().unwrap_or("");

    match command {
        "/quit" | "/exit" | "/q" => return Ok(true),

        "/help" | "/h" => {
            print_help();
            print_extension_commands(session);
        }

        "/model" => {
            if args.is_empty() {
                println!("Current model: \x1b[1m{}\x1b[0m", session.model().id);
                println!("Provider: {}", session.model().provider.as_str());
            } else {
                let resolved = model_resolver::resolve_model(None, args);
                session.set_model(resolved.model.clone());
                println!("Model changed to: \x1b[1m{}\x1b[0m", resolved.model.id);
            }
        }

        "/session" => {
            println!("Session: {}", session.session_id());
            println!("Messages: {}", session.message_count());
            println!("Model: {}", session.model().id);
            println!("CWD: {}", session.cwd().display());
        }

        "/compact" => {
            println!("\x1b[36mRunning compaction...\x1b[0m");
            // `session.compact()` now returns the summary string and
            // forces compaction unconditionally (matching the user's
            // intent when they type `/compact` explicitly).
            match session.compact().await {
                Ok(summary) => println!("\x1b[32mCompaction complete.\x1b[0m\n{}", summary,),
                Err(e) => eprintln!("\x1b[31mCompaction failed: {}\x1b[0m", e),
            }
        }

        "/new" => {
            println!(
                "\x1b[33m[Starting new session requires restarting. Use --no-session for ephemeral mode.]\x1b[0m"
            );
        }

        "/export" => {
            let path = if args.is_empty() {
                PathBuf::from(format!("{}.html", session.session_id()))
            } else {
                PathBuf::from(args)
            };
            match handle_export(session, &path) {
                Ok(()) => println!("Exported to: {}", path.display()),
                Err(e) => eprintln!("\x1b[31mExport failed: {}\x1b[0m", e),
            }
        }

        "/name" => {
            if args.is_empty() {
                let label = session.label().unwrap_or("(unnamed)");
                println!("Session: {} ({})", session.session_id(), label);
            } else {
                match session.set_label(args) {
                    Ok(()) => println!("Session named: \x1b[36m{}\x1b[0m", args),
                    Err(e) => eprintln!("\x1b[31mFailed to set name: {}\x1b[0m", e),
                }
            }
        }

        "/copy" => {
            let messages = session.messages();
            if let Some(last_assistant) = messages.iter().rev().find_map(|m| {
                if let model::Message::Assistant(a) = m {
                    Some(a)
                } else {
                    None
                }
            }) {
                let text: String = last_assistant
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        model::AssistantContentBlock::Text(t) => Some(t.text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

                // Try to copy to clipboard via pbcopy (macOS) or xclip (Linux)
                if copy_to_clipboard(&text) {
                    println!("\x1b[32mCopied to clipboard.\x1b[0m");
                } else {
                    println!("{}", text);
                    println!("\n\x1b[33m[Could not access clipboard — text printed above]\x1b[0m");
                }
            } else {
                println!("No assistant message to copy.");
            }
        }

        "/settings" => {
            let settings = session.settings();
            println!("Settings:");
            match settings.shell_path() {
                Some(p) => println!("  Shell: {}", p.display()),
                None => println!("  Shell: <system default>"),
            }
            let cs = settings.compaction_settings();
            println!(
                "  Compaction: {} (threshold: {:.0}%, keep recent: {}k, max ctx: {}k)",
                if cs.enabled() { "enabled" } else { "disabled" },
                cs.threshold() * 100.0,
                cs.keep_recent_tokens() / 1024,
                cs.max_context_tokens() / 1024,
            );
            let rs = settings.retry_settings();
            println!(
                "  Retry: {} (max: {}, delay: {}ms-{}ms)",
                if rs.enabled() { "enabled" } else { "disabled" },
                rs.max_retries(),
                rs.initial_delay_ms(),
                rs.max_delay_ms(),
            );
        }

        "/hotkeys" | "/keybindings" => {
            println!("Keyboard shortcuts:");
            println!("  Ctrl+C  — Cancel current operation / clear input");
            println!("  Ctrl+D  — Exit the session");
            println!("  !cmd    — Run shell command (added to context)");
            println!("  !!cmd   — Run shell command (not added to context)");
            println!("  @path   — Include file contents in message");
        }

        "/changelog" => {
            println!("Hand v{}", VERSION);
            println!("See the project repository for the full changelog.");
        }

        "/resume" => match hand_coding_agent::SessionManager::list(cwd) {
            Ok(sessions) => {
                if sessions.is_empty() {
                    println!("No sessions found.");
                } else {
                    println!("Recent sessions:");
                    for (i, s) in sessions.iter().take(10).enumerate() {
                        let dt = chrono::DateTime::from_timestamp_millis(s.timestamp)
                            .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                            .unwrap_or_default();
                        println!("  {}. {} ({} msgs, {})", i + 1, s.id, s.message_count, dt,);
                    }
                    println!("\nUse --resume <session-id> to resume.");
                }
            }
            Err(e) => eprintln!("\x1b[31mFailed to list sessions: {}\x1b[0m", e),
        },

        "/thinking" => {
            if args.is_empty() {
                println!("Usage: /thinking <off|minimal|low|medium|high|xhigh>");
            } else if let Some(level) = model_resolver::parse_thinking_level(args) {
                let mut opts = session.stream_options().clone();
                opts.reasoning = Some(level);
                session.set_stream_options(opts);
                println!("Thinking level set to: {:?}", level);
            } else {
                println!(
                    "Invalid thinking level: {}. Use: off, minimal, low, medium, high, xhigh",
                    args
                );
            }
        }

        "/models" => {
            let search = if args.is_empty() { None } else { Some(args) };
            let models = model_resolver::list_models(search);
            if models.is_empty() {
                println!("No models found.");
            } else {
                for m in models.iter().take(20) {
                    println!("  {:<16} {}", m.provider.as_str(), m.id,);
                }
                if models.len() > 20 {
                    println!("  ... and {} more", models.len() - 20);
                }
            }
        }

        _ => {
            // Try routing to an extension-contributed slash command before
            // declaring the input unknown. Strip the leading slash from the
            // command name; `args` is already the raw remainder.
            let bare = command.strip_prefix('/').unwrap_or(command);
            let mut registry = hand_coding_agent::SlashCommandRegistry::new();
            for (spec, ext) in session.collected_slash_commands() {
                registry.register_extension_command(spec, ext);
            }
            let cx = session.extension_context();
            match registry.dispatch_extension_command(bare, args, &cx).await {
                Ok(Some(output)) => {
                    println!("{}", output);
                }
                Ok(None) => {
                    println!(
                        "\x1b[33mUnknown command: {}. Type /help for available commands.\x1b[0m",
                        input
                    );
                }
                Err(e) => {
                    eprintln!("\x1b[31mExtension command failed: {}\x1b[0m", e);
                }
            }
        }
    }

    Ok(false)
}

fn handle_agent_event(event: &hand_agent::types::AgentEvent) {
    use hand_agent::types::AgentEvent;
    match event {
        AgentEvent::MessageUpdate {
            assistant_message_event,
            ..
        } => {
            use model::AssistantMessageEvent;
            match assistant_message_event.as_ref() {
                AssistantMessageEvent::TextDelta { delta, .. } => {
                    print!("{}", delta);
                    let _ = io::stdout().flush();
                }
                AssistantMessageEvent::ThinkingDelta { delta, .. } => {
                    print!("\x1b[2m{}\x1b[0m", delta);
                    let _ = io::stdout().flush();
                }
                _ => {}
            }
        }
        AgentEvent::MessageEnd { .. } => {
            println!();
        }
        AgentEvent::ToolExecutionStart { tool_name, .. } => {
            eprintln!("\x1b[36m[{}]\x1b[0m", tool_name);
        }
        AgentEvent::ToolExecutionEnd {
            tool_name,
            is_error,
            ..
        } if *is_error => {
            eprintln!("\x1b[31m[{} failed]\x1b[0m", tool_name);
        }
        AgentEvent::ToolExecutionUpdate { partial_result, .. } => {
            // Show progress updates from tools (e.g., bash streaming output)
            for block in &partial_result.content {
                if let model::ToolResultContent::Text(t) = block {
                    eprint!("\x1b[2m{}\x1b[0m", t.text);
                }
            }
            let _ = io::stderr().flush();
        }
        _ => {}
    }
}

fn handle_export(
    session: &AgentSession,
    path: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    // Route through ExportFormat::from_path so unknown extensions
    // (.md, .txt, no ext, ...) fail fast with the same diagnostic
    // /export shows in the TUI, instead of silently writing HTML
    // into the user's arbitrarily-named file (#84).
    let Some(format) = ExportFormat::from_path(path) else {
        return Err(format!(
            "--export: unsupported extension on {}. Expected .jsonl, .json, or .html.",
            path.display()
        )
        .into());
    };
    match format {
        ExportFormat::Jsonl => {
            export::export_to_jsonl(
                // Interactive-mode export still hands the writer a
                // fresh in-memory SessionManager and bails when it
                // has no path. Pre-existing limitation; the #84 fix
                // only tightens the extension-validation step.
                &hand_coding_agent::SessionManager::in_memory(),
                path,
            )
            .map_err(|e| format!("JSONL export not available for active session: {}", e))?;
        }
        ExportFormat::Json => {
            export::export_to_json(
                &hand_coding_agent::SessionManager::in_memory(),
                path,
            )
            .map_err(|e| format!("JSON export not available for active session: {}", e))?;
        }
        ExportFormat::Html => {
            export::export_to_html(
                session.messages(),
                session.session_id(),
                &session.model().id,
                path,
            )?;
        }
    }
    Ok(())
}

fn print_welcome(session: &AgentSession) {
    println!("\x1b[1;35mHand\x1b[0m v{} — AI Coding Agent", VERSION);
    println!(
        "Model: \x1b[1m{}\x1b[0m ({})",
        session.model().id,
        session.model().provider.as_str()
    );
    println!("Session: {}", session.session_id());
    println!("Type \x1b[1m/help\x1b[0m for commands, \x1b[1m/quit\x1b[0m to exit.\n");
}

/// Print extension-contributed slash commands grouped by their owning
/// extension. No-ops if no extensions are registered or none contribute
/// any slash commands.
fn print_extension_commands(session: &AgentSession) {
    let collected = session.collected_slash_commands();
    if collected.is_empty() {
        return;
    }
    // Group by extension name preserving registration order within a group.
    let mut groups: std::collections::BTreeMap<
        String,
        Vec<&hand_coding_agent::core::extensions::api::SlashCommandSpec>,
    > = std::collections::BTreeMap::new();
    for (spec, ext) in &collected {
        groups
            .entry(ext.manifest().name.clone())
            .or_default()
            .push(spec);
    }
    println!();
    println!("\x1b[1mExtensions:\x1b[0m");
    for (ext_name, specs) in &groups {
        println!("  [{}]", ext_name);
        for spec in specs {
            let usage = spec.usage.as_deref().unwrap_or("");
            if usage.is_empty() {
                println!("    /{:<14}  {}", spec.name, spec.description);
            } else {
                println!("    /{:<14}  {} ({})", spec.name, spec.description, usage);
            }
        }
    }
}

fn print_help() {
    println!("\x1b[1mCommands:\x1b[0m");
    println!("  /quit, /exit, /q     Exit the session");
    println!("  /help, /h            Show this help");
    println!("  /model [pattern]     Show or change model");
    println!("  /models [search]     List available models");
    println!("  /session             Show session info");
    println!("  /settings            Show current settings");
    println!("  /thinking <level>    Set thinking level (off/minimal/low/medium/high/xhigh)");
    println!("  /compact             Compact context (free up token space)");
    println!("  /export [path]       Export session (HTML by default, or .jsonl)");
    println!("  /copy                Copy last assistant message to clipboard");
    println!("  /name [name]         Show or set session name");
    println!("  /resume              List recent sessions");
    println!("  /new                 Start a new session");
    println!("  /hotkeys             Show keyboard shortcuts");
    println!("  /changelog           Show version info");
    println!();
    println!("\x1b[1mShortcuts:\x1b[0m");
    println!("  !<command>           Run shell command (added to context)");
    println!("  !!<command>          Run shell command (not added to context)");
    println!("  @<filepath>          Include file contents in message");
}

/// Resolve a `--fork <source>` argument to an on-disk path. Probes
/// `--session-dir <X>` (when set) before the home-based default so
/// `--fork <id> --session-dir <X>` matches the plumbing
/// `--continue` / `--resume` already have (#77).
fn resolve_session_path_in(
    session_dir: Option<&std::path::Path>,
    cwd: &std::path::Path,
    source: &str,
) -> PathBuf {
    hand_coding_agent::SessionManager::resolve_session_source_in(
        session_dir,
        None,
        cwd,
        source,
    )
}

fn execute_shell(cmd: &str) {
    use std::process::Command;
    let status = Command::new("sh").arg("-c").arg(cmd).status();
    match status {
        Ok(s) => {
            if !s.success() {
                eprintln!(
                    "\x1b[33m[Command exited with {}]\x1b[0m",
                    s.code().unwrap_or(-1)
                );
            }
        }
        Err(e) => eprintln!("\x1b[31mFailed to run command: {}\x1b[0m", e),
    }
}

fn execute_shell_capture(cmd: &str) -> String {
    use std::process::Command;
    match Command::new("sh").arg("-c").arg(cmd).output() {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.is_empty() {
                eprint!("{}", stderr);
            }
            print!("{}", stdout);
            let _ = io::stdout().flush();
            format!("{}{}", stdout, stderr)
        }
        Err(e) => {
            eprintln!("\x1b[31mFailed to run command: {}\x1b[0m", e);
            format!("Error: {}", e)
        }
    }
}

/// Ordered list of external commands used to write text to the system
/// clipboard. The first one that successfully spawns and accepts the
/// payload on stdin wins.
///
/// Order matters:
/// 1. `pbcopy` — macOS native.
/// 2. `wl-copy` — Wayland (Hyprland, Niri, GNOME on Wayland). Must be
///    tried before `xclip`: on Wayland-only compositors `xclip` either
///    isn't present or silently fails because there is no X server to
///    own the selection.
/// 3. `xclip` — X11 / XWayland. Common on traditional Linux desktops.
/// 4. `xsel` — alternate X11 tool, used on systems that ship xsel
///    instead of xclip (some Arch / minimal setups).
fn clipboard_writers() -> &'static [(&'static str, &'static [&'static str])] {
    &[
        ("pbcopy", &[]),
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["--clipboard", "--input"]),
    ]
}

/// Cap on the base64 payload emitted via OSC 52. Some terminals truncate
/// very large escape sequences mid-flight (xterm, tmux pass-through),
/// which leaves the clipboard half-populated. 100 KB encoded ≈ 75 KB
/// decoded — comfortably above most session-export sizes and well below
/// the limit shipping terminals enforce.
const OSC52_MAX_ENCODED_LEN: usize = 100_000;

/// Heuristic: are we running inside an SSH / mosh session? Native
/// clipboard tools on a remote host write to that host's clipboard, not
/// the user's local one, so we still need OSC 52 even when a native
/// writer "succeeded".
fn is_remote_session() -> bool {
    ["SSH_CONNECTION", "SSH_CLIENT", "MOSH_CONNECTION"]
        .iter()
        .any(|k| std::env::var_os(k).is_some_and(|v| !v.is_empty()))
}

/// Render the OSC 52 escape sequence for `text`. Returns `None` when the
/// base64 payload is over [`OSC52_MAX_ENCODED_LEN`] — emitting an
/// over-cap sequence usually corrupts the user's terminal session.
fn osc52_sequence(text: &str) -> Option<String> {
    use base64::Engine as _;
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    if encoded.len() > OSC52_MAX_ENCODED_LEN {
        return None;
    }
    Some(format!("\x1b]52;c;{encoded}\x07"))
}

/// Emit the OSC 52 escape sequence to stdout. Returns whether anything
/// was written (false when the payload exceeded the cap).
fn emit_osc52(text: &str) -> bool {
    match osc52_sequence(text) {
        Some(seq) => {
            use std::io::Write as _;
            let _ = std::io::stdout().write_all(seq.as_bytes());
            let _ = std::io::stdout().flush();
            true
        }
        None => false,
    }
}

fn copy_to_clipboard(text: &str) -> bool {
    use std::process::{Command, Stdio};

    let mut native_ok = false;
    for (cmd, args) in clipboard_writers() {
        let Ok(mut child) = Command::new(cmd).args(*args).stdin(Stdio::piped()).spawn() else {
            continue;
        };
        let Some(mut stdin) = child.stdin.take() else {
            continue;
        };
        if std::io::Write::write_all(&mut stdin, text.as_bytes()).is_err() {
            continue;
        }
        drop(stdin);
        if child.wait().is_ok_and(|s| s.success()) {
            native_ok = true;
            break;
        }
    }

    // On a remote shell the native writer wrote to the remote host's
    // clipboard, not the user's. Always also try OSC 52. On a local
    // shell, only fall back to OSC 52 if no native writer worked.
    let remote = is_remote_session();
    let osc52_ok = if remote || !native_ok {
        emit_osc52(text)
    } else {
        false
    };

    native_ok || osc52_ok
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `wl-copy` must come before `xclip` so Wayland-only compositors
    /// (Hyprland, Niri, GNOME on Wayland) get a working writer instead of
    /// silently failing through `xclip`. `pbcopy` stays first for macOS,
    /// `xsel` is the final X11 fallback.
    #[test]
    fn clipboard_writers_have_expected_order() {
        let names: Vec<&str> = clipboard_writers().iter().map(|(name, _)| *name).collect();
        assert_eq!(names, vec!["pbcopy", "wl-copy", "xclip", "xsel"]);
    }

    #[test]
    fn clipboard_writers_carry_selection_args_for_x11_tools() {
        let table: std::collections::HashMap<&str, &[&str]> =
            clipboard_writers().iter().map(|(n, a)| (*n, *a)).collect();
        assert_eq!(table.get("xclip"), Some(&&["-selection", "clipboard"][..]));
        assert_eq!(table.get("xsel"), Some(&&["--clipboard", "--input"][..]));
        assert!(table.get("pbcopy").unwrap().is_empty());
        assert!(table.get("wl-copy").unwrap().is_empty());
    }

    /// OSC 52 escape sequences must wrap the base64 payload exactly:
    /// `ESC ] 52 ; c ; <base64> BEL`. Terminals parse this strictly —
    /// any extra bytes leak onto the user's screen.
    #[test]
    fn osc52_sequence_has_correct_wrapper() {
        let seq = osc52_sequence("hi").expect("under cap");
        assert!(seq.starts_with("\x1b]52;c;"), "bad prefix: {seq:?}");
        assert!(seq.ends_with('\x07'), "must end with BEL: {seq:?}");
        // "hi" base64 = "aGk="
        assert!(seq.contains("aGk="), "must contain base64 payload: {seq:?}");
    }

    /// Payloads above the cap return `None` so callers can fall back
    /// to printing the text rather than corrupting the terminal with
    /// a partial escape sequence.
    #[test]
    fn osc52_sequence_rejects_oversize_payload() {
        // Each input byte → ~1.33 base64 bytes; pick a size that lands
        // well over OSC52_MAX_ENCODED_LEN encoded.
        let big = "a".repeat(OSC52_MAX_ENCODED_LEN);
        assert!(
            osc52_sequence(&big).is_none(),
            "payloads larger than the encoded cap must be rejected"
        );
    }

    /// SSH_CONNECTION present implies a remote session. The empty-string
    /// case is treated as "not present" to match how shells unset vars.
    #[test]
    fn is_remote_session_detects_ssh_env_vars() {
        let prev = std::env::var_os("SSH_CONNECTION");
        // SAFETY: tests in this crate run single-threaded against this env
        // var pair; no parallel test reads SSH_CONNECTION.
        unsafe {
            std::env::set_var("SSH_CONNECTION", "10.0.0.1 22 10.0.0.2 1234");
        }
        assert!(is_remote_session());
        unsafe {
            std::env::set_var("SSH_CONNECTION", "");
        }
        assert!(!is_remote_session() || std::env::var_os("SSH_CLIENT").is_some());
        unsafe {
            match prev {
                Some(v) => std::env::set_var("SSH_CONNECTION", v),
                None => std::env::remove_var("SSH_CONNECTION"),
            }
        }
    }
}
