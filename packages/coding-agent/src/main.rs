//! Hand — interactive AI coding agent CLI.

use clap::Parser;
use hand_coding_agent::core::agent_session::{AgentSession, AgentSessionConfig, AgentSessionEvent};
use hand_coding_agent::tools;
use model::SimpleStreamOptions;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "hand", about = "Hand — AI coding agent")]
struct Cli {
    /// Initial prompt (non-interactive mode)
    #[arg(short, long)]
    prompt: Option<String>,

    /// Model to use (e.g., "claude-sonnet-4-20250514")
    #[arg(short, long)]
    model: Option<String>,

    /// Provider (e.g., "anthropic", "openai")
    #[arg(long)]
    provider: Option<String>,

    /// Resume a previous session by ID
    #[arg(long)]
    resume: Option<String>,

    /// Working directory
    #[arg(short = 'd', long)]
    cwd: Option<PathBuf>,

    /// Enable verbose logging
    #[arg(short, long)]
    verbose: bool,

    /// Non-interactive print mode
    #[arg(long)]
    print: bool,

    /// Custom system prompt
    #[arg(long)]
    system_prompt: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

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
    let provider = cli
        .provider
        .as_deref()
        .unwrap_or("anthropic");
    let model_id = cli
        .model
        .as_deref()
        .unwrap_or("claude-sonnet-4-20250514");

    let model = resolve_model(provider, model_id);

    // Create tools
    let agent_tools = tools::create_default_tools(&cwd);

    // Create session config
    let config = AgentSessionConfig {
        cwd: cwd.clone(),
        model,
        stream_options: SimpleStreamOptions::default(),
        custom_system_prompt: cli.system_prompt,
        custom_guidelines: None,
        resume_session: cli.resume,
    };

    let mut session = AgentSession::new(config, agent_tools)?;

    // Subscribe to events for output
    session.subscribe(|event| match event {
        AgentSessionEvent::Agent(agent_event) => {
            handle_agent_event(&agent_event);
        }
        AgentSessionEvent::CompactionStart => {
            eprintln!("[Compacting context...]");
        }
        AgentSessionEvent::CompactionEnd { .. } => {
            eprintln!("[Compaction complete]");
        }
        AgentSessionEvent::Error(err) => {
            eprintln!("Error: {}", err);
        }
    });

    if cli.print {
        // Non-interactive: process single prompt
        if let Some(prompt) = cli.prompt {
            session.send_message(&prompt).await?;
        } else {
            // Read from stdin
            let stdin = io::stdin();
            let input: String = stdin.lock().lines().map_while(Result::ok).collect::<Vec<_>>().join("\n");
            if !input.is_empty() {
                session.send_message(&input).await?;
            }
        }
    } else if let Some(prompt) = cli.prompt {
        // Single prompt then interactive
        session.send_message(&prompt).await?;
        run_interactive(&mut session).await?;
    } else {
        // Interactive mode
        print_welcome(&session);
        run_interactive(&mut session).await?;
    }

    Ok(())
}

async fn run_interactive(
    session: &mut AgentSession,
) -> Result<(), Box<dyn std::error::Error>> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    loop {
        print!("\n> ");
        stdout.flush()?;

        let mut input = String::new();
        if stdin.lock().read_line(&mut input)? == 0 {
            break; // EOF
        }

        let input = input.trim();
        if input.is_empty() {
            continue;
        }

        // Handle slash commands
        match input {
            "/quit" | "/exit" | "/q" => break,
            "/help" | "/h" => {
                print_help();
                continue;
            }
            "/model" => {
                println!("Current model: {}", session.model().id);
                continue;
            }
            "/session" => {
                println!("Session: {}", session.session_id());
                println!("Messages: {}", session.message_count());
                continue;
            }
            _ if input.starts_with('/') => {
                println!("Unknown command: {}. Type /help for available commands.", input);
                continue;
            }
            _ => {}
        }

        match session.send_message(input).await {
            Ok(_) => {}
            Err(e) => {
                eprintln!("Error: {}", e);
            }
        }
    }

    Ok(())
}

fn handle_agent_event(event: &hand_agent::types::AgentEvent) {
    use hand_agent::types::AgentEvent;
    match event {
        AgentEvent::MessageUpdate { assistant_message_event, .. } => {
            use model::AssistantMessageEvent;
            match assistant_message_event.as_ref() {
                AssistantMessageEvent::TextDelta { delta, .. } => {
                    print!("{}", delta);
                    let _ = io::stdout().flush();
                }
                AssistantMessageEvent::ThinkingDelta { delta, .. } => {
                    // Show thinking in dim
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
        AgentEvent::ToolExecutionEnd { tool_name, is_error, .. } => {
            if *is_error {
                eprintln!("\x1b[31m[{} failed]\x1b[0m", tool_name);
            }
        }
        _ => {}
    }
}

fn print_welcome(session: &AgentSession) {
    println!("Hand — AI Coding Agent");
    println!("Model: {}", session.model().id);
    println!("Session: {}", session.session_id());
    println!("Type /help for commands, /quit to exit.\n");
}

fn print_help() {
    println!("Commands:");
    println!("  /quit, /exit, /q  — Exit the session");
    println!("  /help, /h         — Show this help");
    println!("  /model            — Show current model");
    println!("  /session          — Show session info");
}

fn resolve_model(provider: &str, model_id: &str) -> model::Model {
    // Try to find a built-in model
    if let Some(m) = model::get_model(provider, model_id) {
        return m;
    }

    // Create a custom model definition
    let provider_enum = match provider {
        "anthropic" => model::types::Provider::Anthropic,
        "openai" => model::types::Provider::OpenAI,
        "google" => model::types::Provider::Google,
        _ => model::types::Provider::Anthropic,
    };

    let api = match provider {
        "anthropic" => model::types::Api::AnthropicMessages,
        "openai" => model::types::Api::OpenAICompletions,
        "google" => model::types::Api::GoogleGenerativeAi,
        _ => model::types::Api::AnthropicMessages,
    };

    model::Model {
        id: model_id.to_string(),
        name: model_id.to_string(),
        api,
        provider: provider_enum,
        base_url: String::new(),
        reasoning: false,
        input: vec![model::InputType::Text],
        cost: model::Cost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        },
        context_window: 200_000,
        max_tokens: 8192,
        headers: None,
        compat: None,
    }
}
