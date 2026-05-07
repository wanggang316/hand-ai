//! Hand — interactive AI coding agent CLI.

use clap::Parser;
use hand_coding_agent::cli::Args;
use hand_coding_agent::core::agent_session::{AgentSession, AgentSessionConfig, AgentSessionEvent};
use hand_coding_agent::core::export;
use hand_coding_agent::core::model_resolver;
use hand_coding_agent::modes;
use hand_coding_agent::modes::session_setup::SessionSetup;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use tracing_subscriber::EnvFilter;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Args::parse();

    // Handle --list-models
    if let Some(ref search) = cli.list_models {
        let models = model_resolver::list_models(search.as_deref());
        if models.is_empty() {
            println!("No models found.");
        } else {
            println!("{:<20} {:<40} NAME", "PROVIDER", "MODEL ID");
            println!("{}", "-".repeat(80));
            for m in &models {
                println!("{:<20} {:<40} {}", m.provider.as_str(), m.id, m.name);
            }
            println!("\n{} models total", models.len());
        }
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
    if cli.rpc {
        return run_rpc(cli).await;
    }

    // Non-interactive print mode: single prompt + exit.
    if cli.print {
        return modes::print::run(cli).await;
    }

    // Interactive flow.
    let setup = SessionSetup::resolve(&cli)?;
    let cwd = setup.cwd.clone();

    // Determine resume id, mirroring the pre-extraction logic: --continue
    // defers to `SessionManager::continue_recent` below; otherwise --resume
    // is honoured directly.
    let resume_session = if cli.continue_session {
        None
    } else {
        cli.resume.clone()
    };

    let base_config = setup.to_config(resume_session);
    let agent_tools = setup.agent_tools;

    let mut session = if cli.continue_session {
        // Continue most recent session
        match hand_coding_agent::SessionManager::continue_recent(&cwd) {
            Ok(sm) => {
                let config = AgentSessionConfig {
                    resume_session: Some(sm.id().to_string()),
                    ..base_config.clone()
                };
                drop(sm);
                AgentSession::new(config, agent_tools)?
            }
            Err(e) => {
                eprintln!("No session to continue: {}. Starting new session.", e);
                AgentSession::new(base_config, agent_tools)?
            }
        }
    } else if let Some(ref fork_source) = cli.fork {
        // Fork from existing session
        let fork_path = resolve_session_path(&cwd, fork_source);
        match hand_coding_agent::SessionManager::fork_from(&fork_path, &cwd) {
            Ok(sm) => {
                let config = AgentSessionConfig {
                    resume_session: Some(sm.id().to_string()),
                    ..base_config.clone()
                };
                drop(sm);
                AgentSession::new(config, agent_tools)?
            }
            Err(e) => {
                eprintln!("Failed to fork session: {}. Starting new session.", e);
                AgentSession::new(base_config, agent_tools)?
            }
        }
    } else {
        AgentSession::new(base_config, agent_tools)?
    };

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
    });

    // Handle --export
    if let Some(export_path) = cli.export {
        return handle_export(&session, &export_path);
    }

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
    let setup = SessionSetup::resolve(&cli)?;

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
            match session.compact().await {
                Ok(true) => println!("\x1b[32mCompaction complete.\x1b[0m"),
                Ok(false) => {
                    println!("\x1b[33mNo compaction needed (context within limits).\x1b[0m")
                }
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
            match registry
                .dispatch_extension_command(bare, args, &cx)
                .await
            {
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
                    eprintln!(
                        "\x1b[31mExtension command failed: {}\x1b[0m",
                        e
                    );
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
        } => {
            if *is_error {
                eprintln!("\x1b[31m[{} failed]\x1b[0m", tool_name);
            }
        }
        AgentEvent::ToolExecutionUpdate { update, .. } => {
            // Show progress updates from tools (e.g., bash streaming output)
            if let Some(text) = update.as_str() {
                eprint!("\x1b[2m{}\x1b[0m", text);
                let _ = io::stderr().flush();
            }
        }
        _ => {}
    }
}

fn handle_export(
    session: &AgentSession,
    path: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("html");

    match ext {
        "jsonl" => {
            export::export_to_jsonl(
                // Need session manager access — use messages for now
                &hand_coding_agent::SessionManager::in_memory(),
                path,
            )
            .map_err(|e| format!("JSONL export not available for active session: {}", e))?;
        }
        _ => {
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
    let mut groups: std::collections::BTreeMap<String, Vec<&hand_coding_agent::core::extensions::api::SlashCommandSpec>> =
        std::collections::BTreeMap::new();
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
                println!(
                    "    /{:<14}  {} ({})",
                    spec.name, spec.description, usage
                );
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

fn resolve_session_path(cwd: &std::path::Path, source: &str) -> PathBuf {
    let path = PathBuf::from(source);
    if path.exists() {
        return path;
    }
    // Try as session ID in default dir
    let session_dir = cwd.join(".hand").join("sessions");
    let candidate = session_dir.join(format!("{}.jsonl", source));
    if candidate.exists() {
        return candidate;
    }
    // Try prefix match
    if let Ok(entries) = std::fs::read_dir(&session_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if let Some(name_str) = name.to_str()
                && name_str.starts_with(source)
                && name_str.ends_with(".jsonl")
            {
                return entry.path();
            }
        }
    }
    path
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

fn copy_to_clipboard(text: &str) -> bool {
    use std::process::{Command, Stdio};

    // Try pbcopy (macOS)
    if let Ok(mut child) = Command::new("pbcopy").stdin(Stdio::piped()).spawn()
        && let Some(mut stdin) = child.stdin.take()
        && std::io::Write::write_all(&mut stdin, text.as_bytes()).is_ok()
    {
        drop(stdin);
        return child.wait().is_ok_and(|s| s.success());
    }

    // Try xclip (Linux)
    if let Ok(mut child) = Command::new("xclip")
        .args(["-selection", "clipboard"])
        .stdin(Stdio::piped())
        .spawn()
        && let Some(mut stdin) = child.stdin.take()
        && std::io::Write::write_all(&mut stdin, text.as_bytes()).is_ok()
    {
        drop(stdin);
        return child.wait().is_ok_and(|s| s.success());
    }

    false
}
