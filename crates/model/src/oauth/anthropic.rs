//! Anthropic OAuth (Claude Pro/Max) — PKCE + loopback HTTP server.
//!
//! The flow:
//!
//! 1. Generate a PKCE pair. The verifier doubles as the OAuth `state`
//!    so Claude OAuth sessions stay interchangeable with other clients
//!    using the same shared trick.
//! 2. Spin up a loopback HTTP server on `127.0.0.1:53692/callback`.
//! 3. Print the authorize URL via `on_open_url` and wait for the browser to
//!    hit our callback.
//! 4. Exchange the captured code (+ verifier) for tokens at the token URL.

use std::sync::Arc;
use std::sync::mpsc::{Sender, channel};
use std::time::Duration;

type CallbackChannel = Result<CallbackResult, OAuthError>;

/// Hard cap on how long we wait for the user to complete the browser flow.
/// Mirrors the behaviour of Claude Code's CLI: if we don't get a callback
/// within five minutes, we tear the listener down and surface a timeout.
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(300);

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;
use serde_json::json;

use super::oauth_page::{error_html, success_html};
use super::pkce::generate_pkce;
use super::types::{
    OAuthCredentials, OAuthError, OAuthLoginCallbacks, OAuthProvider, OAuthProviderId,
};
use super::util::{parse_query, split_path_query};

const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const CALLBACK_HOST: &str = "127.0.0.1";
const CALLBACK_PORT: u16 = 53692;
const CALLBACK_PATH: &str = "/callback";
const SCOPES: &str = "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";

// CLIENT_ID is base64-obfuscated in the TS reference; preserve that encoding
// so this file matches upstream byte-for-byte at the obfuscation layer.
const CLIENT_ID_B64: &str = "OWQxYzI1MGEtZTYxYi00NGQ5LTg4ZWQtNTk0NGQxOTYyZjVl";

fn client_id() -> String {
    let bytes = STANDARD
        .decode(CLIENT_ID_B64)
        .expect("hard-coded base64 client id is well-formed");
    String::from_utf8(bytes).expect("hard-coded client id is utf-8")
}

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

/// Captured callback parameters.
struct CallbackResult {
    code: String,
}

pub struct AnthropicOAuthProvider {
    http: reqwest::Client,
    token_url: String,
}

impl AnthropicOAuthProvider {
    pub fn new() -> Self {
        Self::with_token_url(TOKEN_URL.to_string())
    }

    /// Construct a provider that sends token-exchange / refresh requests to
    /// a custom endpoint. Production code should use [`Self::new`]; this is
    /// here for tests that point the provider at a local mock server.
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

impl Default for AnthropicOAuthProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl OAuthProvider for AnthropicOAuthProvider {
    fn id(&self) -> OAuthProviderId {
        OAuthProviderId::Anthropic
    }

    async fn login(&self, callbacks: &OAuthLoginCallbacks) -> Result<OAuthCredentials, OAuthError> {
        let pkce = generate_pkce();
        // TS reference uses `state = verifier`. Keep the same convention so
        // stored sessions remain compatible.
        let state = pkce.verifier.clone();
        let redirect = redirect_uri();
        let cid = client_id();

        let auth_url = build_authorize_url(&cid, &redirect, &pkce.challenge, &state);
        (callbacks.on_open_url)(&auth_url);

        let result = run_callback_server(&state).await?;
        let token = exchange_code(
            &self.http,
            &self.token_url,
            &result.code,
            &state,
            &pkce.verifier,
            &redirect,
            &cid,
        )
        .await?;
        Ok(token)
    }

    async fn refresh(&self, creds: &OAuthCredentials) -> Result<OAuthCredentials, OAuthError> {
        let refresh_token = creds.refresh_token.as_deref().ok_or_else(|| {
            OAuthError::RefreshFailed("anthropic credentials missing refresh_token".into())
        })?;
        let cid = client_id();
        let body = json!({
            "grant_type": "refresh_token",
            "client_id": cid,
            "refresh_token": refresh_token,
        });
        let resp = self
            .http
            .post(&self.token_url)
            .header("Accept", "application/json")
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            // Body may echo refresh tokens or other secrets in some failure
            // modes — keep the error status-only.
            return Err(OAuthError::RefreshFailed(format!(
                "anthropic refresh failed: HTTP {status}"
            )));
        }
        let parsed: TokenResponse = serde_json::from_str(&text)?;
        Ok(token_to_credentials(parsed))
    }
}

fn build_authorize_url(
    client_id: &str,
    redirect_uri: &str,
    challenge: &str,
    state: &str,
) -> String {
    // Mirrors the TS query order. `code=true` is intentional (Anthropic-specific
    // marker preserved from the reference implementation).
    let params = [
        ("code", "true"),
        ("client_id", client_id),
        ("response_type", "code"),
        ("redirect_uri", redirect_uri),
        ("scope", SCOPES),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("state", state),
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
    state: &str,
    verifier: &str,
    redirect_uri: &str,
    client_id: &str,
) -> Result<OAuthCredentials, OAuthError> {
    let body = json!({
        "grant_type": "authorization_code",
        "client_id": client_id,
        "code": code,
        "state": state,
        "redirect_uri": redirect_uri,
        "code_verifier": verifier,
    });
    let resp = http
        .post(token_url)
        .header("Accept", "application/json")
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        // See the corresponding comment in `refresh`: the server can echo
        // tokens or other secrets in failure bodies, so we surface status
        // only.
        return Err(OAuthError::ProviderError(
            OAuthProviderId::Anthropic,
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
    // Subtract a 5-minute safety window like the TS reference.
    let expires_at = now_ms
        + t.expires_in
            .saturating_mul(1000)
            .saturating_sub(5 * 60 * 1000);
    OAuthCredentials {
        access_token: t.access_token,
        refresh_token: Some(t.refresh_token),
        expires_at: Some(expires_at),
        scope: t.scope,
        extra: None,
    }
}

/// Spawn the loopback server on a blocking thread and await the captured
/// code, with a 5-minute hard timeout. On timeout we call `Server::unblock`
/// so the worker thread exits cleanly instead of holding the port forever.
async fn run_callback_server(expected_state: &str) -> Result<CallbackResult, OAuthError> {
    let addr = format!("{CALLBACK_HOST}:{CALLBACK_PORT}");
    let server = Arc::new(tiny_http::Server::http(&addr).map_err(|e| {
        OAuthError::ProviderError(
            OAuthProviderId::Anthropic,
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
                    OAuthProviderId::Anthropic,
                    format!("callback receiver join: {e}"),
                )
            })?
            .map_err(|e| {
                OAuthError::ProviderError(
                    OAuthProviderId::Anthropic,
                    format!("callback channel closed: {e}"),
                )
            })?,
        Err(_) => {
            // Wake the blocking server thread so it stops listening.
            server.unblock();
            let _ = join.await;
            return Err(OAuthError::ProviderError(
                OAuthProviderId::Anthropic,
                "OAuth callback timed out (5 minutes)".into(),
            ));
        }
    };

    // Reap the server task so we don't leak the JoinHandle.
    let _ = join.await;
    result
}

/// Run the loopback HTTP server until it receives a single callback request.
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
                    "Anthropic authentication did not complete.",
                    Some(&format!("Error: {err}")),
                ),
            ));
            let _ = tx.send(Err(OAuthError::ProviderError(
                OAuthProviderId::Anthropic,
                format!("oauth error: {err}"),
            )));
            return;
        }

        match (code, state) {
            (Some(c), Some(s)) if s == expected_state => {
                let _ = request.respond(html_response(
                    200,
                    &success_html("Anthropic authentication completed. You can close this window."),
                ));
                let _ = tx.send(Ok(CallbackResult { code: c }));
                return;
            }
            (_, Some(s)) if s != expected_state => {
                let _ = request.respond(html_response(400, &error_html("State mismatch.", None)));
                let _ = tx.send(Err(OAuthError::ProviderError(
                    OAuthProviderId::Anthropic,
                    "state mismatch".into(),
                )));
                return;
            }
            _ => {
                let _ = request.respond(html_response(
                    400,
                    &error_html("Missing code or state parameter.", None),
                ));
                let _ = tx.send(Err(OAuthError::ProviderError(
                    OAuthProviderId::Anthropic,
                    "missing code/state".into(),
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
