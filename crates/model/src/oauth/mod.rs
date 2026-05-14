//! OAuth credential management for AI providers.
//!
//! Provides login, refresh, and persistence for OAuth-based providers:
//!
//! - Anthropic (Claude Pro/Max) — PKCE + loopback HTTP server
//! - OpenAI Codex (ChatGPT Plus/Pro) — PKCE + loopback HTTP server
//! - GitHub Copilot — Device Flow

pub mod anthropic;
pub mod github_copilot;
pub mod oauth_page;
pub mod openai_codex;
pub mod pkce;
pub mod registry;
pub mod types;
mod util;

pub use github_copilot::{github_copilot_base_url, normalize_domain};
pub use registry::OAuthRegistry;
pub use types::{
    OAuthAuthInfo, OAuthCredentials, OAuthError, OAuthLoginCallbacks, OAuthProvider,
    OAuthProviderId,
};
