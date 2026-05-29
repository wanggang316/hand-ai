//! Local web server entry point for the hand coding agent web UI.
//!
//! Serves the browser frontend over HTTP and bridges a `/ws` WebSocket onto
//! the existing agent RPC dispatcher (see [`ws`]). Binds loopback only.

mod app;
mod session;
mod ws;

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;

/// Command-line options for the web UI server.
#[derive(Debug, Parser)]
#[command(
    name = "hand-web-ui",
    about = "Local web server for the hand coding agent"
)]
struct Args {
    /// Port to bind on (loopback only).
    #[arg(long, default_value_t = 4137)]
    port: u16,

    /// Model id used for new sessions.
    #[arg(long, default_value = "deepseek/deepseek-v4-flash")]
    model: String,

    /// Optional provider override for the model.
    #[arg(long)]
    provider: Option<String>,

    /// Working directory for agent sessions (defaults to the current dir).
    #[arg(long)]
    cwd: Option<PathBuf>,

    /// Directory holding the built frontend assets to serve.
    #[arg(long, default_value = "crates/web-ui/web/dist")]
    web_dir: PathBuf,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "hand_web_ui=info,tower_http=warn".into()),
        )
        .init();

    let args = Args::parse();
    let cwd = match args.cwd {
        Some(dir) => dir,
        None => std::env::current_dir()?,
    };

    let state = app::AppState {
        cwd,
        model: args.model,
        provider: args.provider,
        web_dir: args.web_dir,
    };

    let router = app::router(state);
    let addr = SocketAddr::from(([127, 0, 0, 1], args.port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("listening on http://{addr}");
    println!("hand web ui: open http://{addr}");
    axum::serve(listener, router).await?;
    Ok(())
}
