//! Hand — interactive AI coding agent CLI.

use clap::Parser;
use hand_coding_agent::core::agent_session::{AgentSession, AgentSessionConfig, AgentSessionEvent};
use hand_coding_agent::core::export;
use hand_coding_agent::core::model_resolver;
use hand_coding_agent::tools;
use model::SimpleStreamOptions;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use tracing_subscriber::EnvFilter;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser)]
#[command(
    name = "hand",
    about = "Hand — AI coding agent",
    version = VERSION,
    long_about = "Hand is an interactive AI coding agent that helps you write, edit, and understand code."
)]
struct Cli {
    /// Initial prompt (non-interactive mode)
    #[arg(short, long)]
    prompt: Option<String>,

    /// Model pattern (e.g., "sonnet", "claude-sonnet:high", "openai/gpt-4o")
    #[arg(short, long)]
    model: Option<String>,

    /// Provider (e.g., "anthropic", "openai", "google")
    #[arg(long)]
    provider: Option<String>,

    /// API key override (not persisted)
    #[arg(long)]
    api_key: Option<String>,

    /// Resume a previous session by ID
    #[arg(short, long)]
    resume: Option<String>,

    /// Continue the most recent session
    #[arg(short, long = "continue")]
    continue_session: bool,

    /// Fork from a session file path or ID prefix
    #[arg(long)]
    fork: Option<String>,

    /// Working directory
    #[arg(short = 'd', long)]
    cwd: Option<PathBuf>,

    /// Custom system prompt (overrides default)
    #[arg(long)]
    system_prompt: Option<String>,

    /// Append text to the system prompt
    #[arg(long)]
    append_system_prompt: Option<String>,

    /// Thinking level: off, minimal, low, medium, high, xhigh
    #[arg(long)]
    thinking: Option<String>,

    /// Comma-separated tools to enable (read,write,edit,bash,grep,find,ls)
    #[arg(long)]
    tools: Option<String>,

    /// Disable all tools
    #[arg(long)]
    no_tools: bool,

    /// Run in ephemeral mode (don't save session)
    #[arg(long)]
    no_session: bool,

    /// Non-interactive print mode
    #[arg(long)]
    print: bool,

    /// Export session to file (HTML or JSONL based on extension)
    #[arg(long)]
    export: Option<PathBuf>,

    /// List available models (optional search filter)
    #[arg(long)]
    list_models: Option<Option<String>>,

    /// Enable verbose logging
    #[arg(short, long)]
    verbose: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

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

    // Determine working directory
    let cwd = cli
        .cwd
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    // Resolve model
    let provider = cli.provider.as_deref().unwrap_or("anthropic");
    let model_pattern = cli
        .model
        .as_deref()
        .unwrap_or_else(|| model_resolver::default_model_for_provider(provider));

    let resolved = model_resolver::resolve_model(Some(provider), model_pattern);

    // Parse thinking level (CLI flag overrides model pattern suffix)
    let thinking_level = cli
        .thinking
        .as_deref()
        .and_then(model_resolver::parse_thinking_level)
        .or(resolved.thinking_level);

    // Build stream options
    let mut stream_options = SimpleStreamOptions::default();
    if let Some(level) = thinking_level {
        stream_options.reasoning = Some(level);
    }

    // Create tools
    let agent_tools = if cli.no_tools {
        vec![]
    } else if let Some(ref tool_list) = cli.tools {
        create_selected_tools(&cwd, tool_list)
    } else {
        tools::create_default_tools(&cwd)
    };

    // Determine custom guidelines (append-system-prompt)
    let custom_guidelines = cli.append_system_prompt;

    // Create session config
    let resume_session = if cli.continue_session {
        // Will be handled via continue_recent below
        None
    } else {
        cli.resume
    };

    let config = AgentSessionConfig {
        cwd: cwd.clone(),
        model: resolved.model,
        stream_options,
        custom_system_prompt: cli.system_prompt,
        custom_guidelines,
        resume_session,
    };

    let mut session = if cli.continue_session {
        // Continue most recent session
        match hand_coding_agent::SessionManager::continue_recent(&cwd) {
            Ok(sm) => {
                let config = AgentSessionConfig {
                    cwd: cwd.clone(),
                    model: config.model,
                    stream_options: config.stream_options,
                    custom_system_prompt: config.custom_system_prompt,
                    custom_guidelines: config.custom_guidelines,
                    resume_session: Some(sm.id().to_string()),
                };
                drop(sm);
                AgentSession::new(config, agent_tools)?
            }
            Err(e) => {
                eprintln!("No session to continue: {}. Starting new session.", e);
                AgentSession::new(config, agent_tools)?
            }
        }
    } else if let Some(ref fork_source) = cli.fork {
        // Fork from existing session
        let fork_path = resolve_session_path(&cwd, fork_source);
        match hand_coding_agent::SessionManager::fork_from(&fork_path, &cwd) {
            Ok(sm) => {
                let config = AgentSessionConfig {
                    cwd: cwd.clone(),
                    model: config.model,
                    stream_options: config.stream_options,
                    custom_system_prompt: config.custom_system_prompt,
                    custom_guidelines: config.custom_guidelines,
                    resume_session: Some(sm.id().to_string()),
                };
                drop(sm);
                AgentSession::new(config, agent_tools)?
            }
            Err(e) => {
                eprintln!("Failed to fork session: {}. Starting new session.", e);
                AgentSession::new(config, agent_tools)?
            }
        }
    } else {
        AgentSession::new(config, agent_tools)?
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

    if cli.print {
        // Non-interactive: process single prompt
        if let Some(prompt) = cli.prompt {
            session.send_message(&prompt).await?;
        } else {
            // Read from stdin
            let stdin = io::stdin();
            let input: String = stdin
                .lock()
                .lines()
                .map_while(Result::ok)
                .collect::<Vec<_>>()
                .join("\n");
            if !input.is_empty() {
                session.send_message(&input).await?;
            }
        }
    } else if let Some(prompt) = cli.prompt {
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
            println!("  Shell: {}", settings.shell_path());
            let cs = settings.compaction_settings();
            println!(
                "  Compaction: {} (reserve: {}k, keep: {}k)",
                if cs.enabled { "enabled" } else { "disabled" },
                cs.reserve_tokens / 1024,
                cs.keep_recent_tokens / 1024,
            );
            let rs = settings.retry_settings();
            println!(
                "  Retry: {} (max: {}, delay: {}ms–{}ms)",
                if rs.enabled { "enabled" } else { "disabled" },
                rs.max_retries,
                rs.base_delay_ms,
                rs.max_delay_ms,
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
            println!(
                "\x1b[33mUnknown command: {}. Type /help for available commands.\x1b[0m",
                input
            );
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

fn create_selected_tools(
    cwd: &std::path::Path,
    tool_list: &str,
) -> Vec<hand_agent::types::AgentTool> {
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
