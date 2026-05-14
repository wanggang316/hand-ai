//! OpenAI Codex (ChatGPT Plus/Pro) OAuth — PKCE + loopback HTTP server.
//!
//! Differs from the Anthropic flow in two ways:
//!
//! - Token endpoint uses `application/x-www-form-urlencoded` (not JSON).
//! - Access tokens are JWTs whose payload contains the user's
//!   `chatgpt_account_id`; we decode it and store it under `extra` so the
//!   ChatGPT API client can attach it as a header later.

use std::sync::Arc;
use std::sync::mpsc::{Sender, channel};
use std::time::Duration;

type CallbackChannel = Result<CallbackResult, OAuthError>;

/// Hard cap on the browser-callback wait. Same rationale as the Anthropic
/// flow: tear down the listener if the user never completes the redirect.
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(300);

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use serde::Deserialize;
use serde_json::json;

use super::oauth_page::{error_html, success_html};
use super::pkce::generate_pkce;
use super::types::{
    OAuthCredentials, OAuthError, OAuthLoginCallbacks, OAuthProvider, OAuthProviderId,
};
use super::util::{parse_query, split_path_query};

const CALLBACK_HOST: &str = "127.0.0.1";
const CALLBACK_PORT: u16 = 1455;
const CALLBACK_PATH: &str = "/auth/callback";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const SCOPE: &str = "openid profile email offline_access";
const JWT_CLAIM_PATH: &str = "https://api.openai.com/auth";
const ORIGINATOR: &str = "pi";

fn redirect_uri() -> String {
    format!("http://localhost:{CALLBACK_PORT}{CALLBACK_PATH}")
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: u64,
    #[serde(default)]
    scope: Option<String>,
}

struct CallbackResult {
    code: String,
}

pub struct OpenAiCodexOAuthProvider {
    http: reqwest::Client,
    token_url: String,
}

impl OpenAiCodexOAuthProvider {
    pub fn new() -> Self {
        Self::with_token_url(TOKEN_URL.to_string())
    }

    /// Construct a provider that sends token-exchange / refresh requests to
    /// a custom endpoint. Used for tests against a local mock server.
    pub fn with_token_url(token_url: String) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("default reqwest client"),
            token_url,
        }
    }
}

impl Default for OpenAiCodexOAuthProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl OAuthProvider for OpenAiCodexOAuthProvider {
    fn id(&self) -> OAuthProviderId {
        OAuthProviderId::OpenAICodex
    }

    async fn login(&self, callbacks: &OAuthLoginCallbacks) -> Result<OAuthCredentials, OAuthError> {
        let pkce = generate_pkce();
        let state = random_state();
        let redirect = redirect_uri();
        let auth_url = build_authorize_url(&redirect, &pkce.challenge, &state);
        (callbacks.on_open_url)(&auth_url);

        let result = run_callback_server(&state).await?;
        let token = exchange_code(
            &self.http,
            &self.token_url,
            &result.code,
            &pkce.verifier,
            &redirect,
        )
        .await?;
        Ok(token)
    }

    async fn refresh(&self, creds: &OAuthCredentials) -> Result<OAuthCredentials, OAuthError> {
        let refresh_token = creds.refresh_token.as_deref().ok_or_else(|| {
            OAuthError::RefreshFailed("openai-codex credentials missing refresh_token".into())
        })?;
        let form = [
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", CLIENT_ID),
        ];
        let resp = self.http.post(&self.token_url).form(&form).send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            // Avoid echoing the response body — it can include refresh
            // tokens or other secrets in some failure modes.
            return Err(OAuthError::RefreshFailed(format!(
                "openai-codex refresh failed: HTTP {status}"
            )));
        }
        let parsed: TokenResponse = serde_json::from_str(&text)?;
        Ok(token_to_credentials(parsed))
    }
}

fn random_state() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    let mut hex = String::with_capacity(32);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

fn build_authorize_url(redirect_uri: &str, challenge: &str, state: &str) -> String {
    let params = [
        ("response_type", "code"),
        ("client_id", CLIENT_ID),
        ("redirect_uri", redirect_uri),
        ("scope", SCOPE),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("state", state),
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("originator", ORIGINATOR),
    ];
    let q: Vec<String> = params
        .iter()
        .map(|(k, v)| format!("{}={}", k, urlencoding::encode(v)))
        .collect();
    format!("{AUTHORIZE_URL}?{}", q.join("&"))
}

async fn exchange_code(
    http: &reqwest::Client,
    token_url: &str,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<OAuthCredentials, OAuthError> {
    let form = [
        ("grant_type", "authorization_code"),
        ("client_id", CLIENT_ID),
        ("code", code),
        ("code_verifier", verifier),
        ("redirect_uri", redirect_uri),
    ];
    let resp = http.post(token_url).form(&form).send().await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        // Status-only — same rationale as `refresh`.
        return Err(OAuthError::ProviderError(
            OAuthProviderId::OpenAICodex,
            format!("token exchange failed: HTTP {status}"),
        ));
    }
    let parsed: TokenResponse = serde_json::from_str(&text)?;
    Ok(token_to_credentials(parsed))
}

fn token_to_credentials(t: TokenResponse) -> OAuthCredentials {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let expires_at = now_ms + t.expires_in.saturating_mul(1000);
    let account_id = decode_account_id(&t.access_token);
    let extra = account_id.map(|id| json!({ "chatgpt_account_id": id }));
    OAuthCredentials {
        access_token: t.access_token,
        refresh_token: Some(t.refresh_token),
        expires_at: Some(expires_at),
        scope: t.scope,
        extra,
    }
}

/// Decode the JWT payload and pull `chatgpt_account_id` from the OpenAI
/// claim namespace. Returns `None` for tokens we can't parse — callers
/// then surface that as a provider error if account id is required.
fn decode_account_id(access_token: &str) -> Option<String> {
    let payload_b64 = access_token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload_b64).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let claim = value.get(JWT_CLAIM_PATH)?;
    let id = claim.get("chatgpt_account_id")?.as_str()?;
    if id.is_empty() {
        None
    } else {
        Some(id.to_string())
    }
}

async fn run_callback_server(expected_state: &str) -> Result<CallbackResult, OAuthError> {
    let addr = format!("{CALLBACK_HOST}:{CALLBACK_PORT}");
    let server = Arc::new(tiny_http::Server::http(&addr).map_err(|e| {
        OAuthError::ProviderError(
            OAuthProviderId::OpenAICodex,
            format!("failed to bind {addr}: {e}"),
        )
    })?);

    let expected = expected_state.to_string();
    let (tx, rx) = channel::<CallbackChannel>();
    let server_for_thread = Arc::clone(&server);
    let join = tokio::task::spawn_blocking(move || serve_one(&server_for_thread, &expected, tx));

    let recv = tokio::task::spawn_blocking(move || rx.recv());
    let outcome = tokio::time::timeout(CALLBACK_TIMEOUT, recv).await;

    let result = match outcome {
        Ok(join_result) => join_result
            .map_err(|e| {
                OAuthError::ProviderError(
                    OAuthProviderId::OpenAICodex,
                    format!("callback receiver join: {e}"),
                )
            })?
            .map_err(|e| {
                OAuthError::ProviderError(
                    OAuthProviderId::OpenAICodex,
                    format!("callback channel closed: {e}"),
                )
            })?,
        Err(_) => {
            server.unblock();
            let _ = join.await;
            return Err(OAuthError::ProviderError(
                OAuthProviderId::OpenAICodex,
                "OAuth callback timed out (5 minutes)".into(),
            ));
        }
    };

    let _ = join.await;
    result
}

fn serve_one(server: &tiny_http::Server, expected_state: &str, tx: Sender<CallbackChannel>) {
    for request in server.incoming_requests() {
        let raw = request.url().to_string();
        let (path, query) = split_path_query(&raw);

        if path != CALLBACK_PATH {
            let _ = request.respond(html_response(
                404,
                &error_html("Callback route not found.", None),
            ));
            continue;
        }

        let (mut code, mut state, mut error) = (None, None, None);
        for (k, v) in parse_query(query) {
            match k.as_str() {
                "code" => code = Some(v),
                "state" => state = Some(v),
                "error" => error = Some(v),
                _ => {}
            }
        }

        if let Some(err) = error {
            let _ = request.respond(html_response(
                400,
                &error_html(
                    "OpenAI authentication did not complete.",
                    Some(&format!("Error: {err}")),
                ),
            ));
            let _ = tx.send(Err(OAuthError::ProviderError(
                OAuthProviderId::OpenAICodex,
                format!("oauth error: {err}"),
            )));
            return;
        }

        match (state, code) {
            (Some(s), _) if s != expected_state => {
                let _ = request.respond(html_response(400, &error_html("State mismatch.", None)));
                let _ = tx.send(Err(OAuthError::ProviderError(
                    OAuthProviderId::OpenAICodex,
                    "state mismatch".into(),
                )));
                return;
            }
            (_, Some(c)) => {
                let _ = request.respond(html_response(
                    200,
                    &success_html("OpenAI authentication completed. You can close this window."),
                ));
                let _ = tx.send(Ok(CallbackResult { code: c }));
                return;
            }
            _ => {
                let _ = request.respond(html_response(
                    400,
                    &error_html("Missing authorization code.", None),
                ));
                let _ = tx.send(Err(OAuthError::ProviderError(
                    OAuthProviderId::OpenAICodex,
                    "missing code".into(),
                )));
                return;
            }
        }
    }
}

fn html_response(status: u16, html: &str) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    let bytes = html.as_bytes().to_vec();
    let len = bytes.len();
    tiny_http::Response::new(
        tiny_http::StatusCode(status),
        vec![
            tiny_http::Header::from_bytes(
                b"Content-Type".as_ref(),
                b"text/html; charset=utf-8".as_ref(),
            )
            .expect("static header"),
        ],
        std::io::Cursor::new(bytes),
        Some(len),
        None,
    )
}
