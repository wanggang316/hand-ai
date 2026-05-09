//! Integration tests for the OAuth subsystem.
//!
//! The shared paths (PKCE, persistence, expiry semantics, credential serde)
//! exercise generic helpers. The per-provider flow tests stand up a local
//! `tiny_http` mock server, point a provider at it via `with_token_url` /
//! `with_endpoints`, and verify the full request/response shape matches what
//! the TS reference produces. Live OAuth flows (real `login()` against the
//! provider's IdP) remain out of scope.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use model::oauth::anthropic::AnthropicOAuthProvider;
use model::oauth::github_copilot::{GithubCopilotOAuthProvider, GithubEndpoints};
use model::oauth::openai_codex::OpenAiCodexOAuthProvider;
use model::oauth::pkce::generate_pkce;
use model::oauth::types::{OAuthLoginCallbacks, OAuthProvider};
use model::{
    OAuthAuthInfo, OAuthCredentials, OAuthProviderId, OAuthRegistry, github_copilot_base_url,
    normalize_domain,
};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

#[test]
fn pkce_generates_valid_pair() {
    let pair = generate_pkce();
    assert!(
        pair.verifier.len() >= 43 && pair.verifier.len() <= 128,
        "verifier length out of bounds: {}",
        pair.verifier.len()
    );
    assert!(
        pair.verifier
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "verifier contains non URL-safe chars: {}",
        pair.verifier
    );
    let decoded = URL_SAFE_NO_PAD
        .decode(&pair.challenge)
        .expect("challenge is base64url");
    assert_eq!(decoded.len(), 32, "sha256 output is 32 bytes");
}

#[test]
fn pkce_challenge_is_sha256_of_verifier() {
    // RFC 7636 §4.2: code_challenge = BASE64URL(SHA256(ASCII(code_verifier))).
    // Re-derive the challenge here and assert byte-for-byte equality.
    let pair = generate_pkce();
    let mut hasher = Sha256::new();
    hasher.update(pair.verifier.as_bytes());
    let expected = URL_SAFE_NO_PAD.encode(hasher.finalize());
    assert_eq!(
        pair.challenge, expected,
        "challenge != base64url(sha256(verifier))"
    );
}

#[test]
fn pkce_pair_is_unique() {
    let mut verifiers = HashSet::new();
    let mut challenges = HashSet::new();
    for _ in 0..100 {
        let p = generate_pkce();
        assert!(verifiers.insert(p.verifier.clone()), "duplicate verifier");
        assert!(
            challenges.insert(p.challenge.clone()),
            "duplicate challenge"
        );
    }
}

#[tokio::test]
async fn oauth_registry_load_save_roundtrip() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("oauth.json");
    let registry = OAuthRegistry::with_storage_path(path);

    let info = OAuthAuthInfo {
        provider_id: OAuthProviderId::Anthropic,
        credentials: OAuthCredentials {
            access_token: "access-1".into(),
            refresh_token: Some("refresh-1".into()),
            expires_at: Some(1_000_000),
            scope: Some("user:profile".into()),
            extra: Some(serde_json::json!({"foo":"bar"})),
        },
        created_at_ms: 42,
    };

    registry.save(&info).await.expect("save");
    let loaded = registry.load().await.expect("load");
    let got = loaded.get(&OAuthProviderId::Anthropic).expect("present");
    assert_eq!(got.created_at_ms, 42);
    assert_eq!(got.credentials.access_token, "access-1");
    assert_eq!(got.credentials.refresh_token.as_deref(), Some("refresh-1"));
    assert_eq!(got.credentials.expires_at, Some(1_000_000));
    assert_eq!(got.credentials.scope.as_deref(), Some("user:profile"));
    assert_eq!(
        got.credentials.extra,
        Some(serde_json::json!({"foo":"bar"}))
    );
}

#[tokio::test]
async fn oauth_registry_remove_clears_id() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("oauth.json");
    let registry = OAuthRegistry::with_storage_path(path);

    let info = OAuthAuthInfo {
        provider_id: OAuthProviderId::OpenAICodex,
        credentials: OAuthCredentials {
            access_token: "a".into(),
            refresh_token: None,
            expires_at: None,
            scope: None,
            extra: None,
        },
        created_at_ms: 0,
    };
    registry.save(&info).await.unwrap();
    registry.remove(OAuthProviderId::OpenAICodex).await.unwrap();
    let loaded = registry.load().await.unwrap();
    assert!(!loaded.contains_key(&OAuthProviderId::OpenAICodex));
}

#[tokio::test]
async fn oauth_registry_load_missing_file_is_empty() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("does-not-exist.json");
    let registry = OAuthRegistry::with_storage_path(path);
    let loaded = registry.load().await.unwrap();
    assert!(loaded.is_empty());
}

#[tokio::test]
async fn oauth_registry_save_preserves_other_providers() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("oauth.json");
    let registry = OAuthRegistry::with_storage_path(path);

    let a = OAuthAuthInfo {
        provider_id: OAuthProviderId::Anthropic,
        credentials: OAuthCredentials {
            access_token: "A".into(),
            refresh_token: None,
            expires_at: None,
            scope: None,
            extra: None,
        },
        created_at_ms: 1,
    };
    let b = OAuthAuthInfo {
        provider_id: OAuthProviderId::GithubCopilot,
        credentials: OAuthCredentials {
            access_token: "B".into(),
            refresh_token: None,
            expires_at: None,
            scope: None,
            extra: None,
        },
        created_at_ms: 2,
    };
    registry.save(&a).await.unwrap();
    registry.save(&b).await.unwrap();
    let loaded = registry.load().await.unwrap();
    assert_eq!(loaded.len(), 2);
    assert_eq!(
        loaded[&OAuthProviderId::Anthropic].credentials.access_token,
        "A"
    );
    assert_eq!(
        loaded[&OAuthProviderId::GithubCopilot]
            .credentials
            .access_token,
        "B"
    );
}

#[test]
fn is_expired_with_60s_buffer() {
    let registry = OAuthRegistry::new();
    let provider = registry.get(OAuthProviderId::Anthropic).expect("provider");

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    let expiring_soon = OAuthCredentials {
        access_token: "x".into(),
        refresh_token: None,
        expires_at: Some(now_ms + 30_000),
        scope: None,
        extra: None,
    };
    assert!(provider.is_expired(&expiring_soon), "30s -> expired");

    let expiring_later = OAuthCredentials {
        access_token: "x".into(),
        refresh_token: None,
        expires_at: Some(now_ms + 90_000),
        scope: None,
        extra: None,
    };
    assert!(
        !provider.is_expired(&expiring_later),
        "90s -> not yet expired"
    );

    let no_expiry = OAuthCredentials {
        access_token: "x".into(),
        refresh_token: None,
        expires_at: None,
        scope: None,
        extra: None,
    };
    assert!(!provider.is_expired(&no_expiry), "no expiry -> not expired");
}

#[test]
fn oauth_credentials_serde_roundtrip() {
    let creds = OAuthCredentials {
        access_token: "tok".into(),
        refresh_token: Some("ref".into()),
        expires_at: Some(123),
        scope: Some("a b c".into()),
        extra: Some(serde_json::json!({"account_id": "acc-1"})),
    };
    let json = serde_json::to_string(&creds).unwrap();
    let parsed: OAuthCredentials = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.access_token, creds.access_token);
    assert_eq!(parsed.refresh_token, creds.refresh_token);
    assert_eq!(parsed.expires_at, creds.expires_at);
    assert_eq!(parsed.scope, creds.scope);
    assert_eq!(parsed.extra, creds.extra);
}

#[test]
fn provider_id_serializes_kebab_case() {
    let json = serde_json::to_string(&OAuthProviderId::OpenAICodex).unwrap();
    assert_eq!(json, "\"openai-codex\"");
    let parsed: OAuthProviderId = serde_json::from_str("\"github-copilot\"").unwrap();
    assert_eq!(parsed, OAuthProviderId::GithubCopilot);
}

#[test]
fn registry_lists_all_builtin_ids() {
    let registry = OAuthRegistry::new();
    let ids = registry.ids();
    assert_eq!(ids.len(), 3);
    assert!(ids.contains(&OAuthProviderId::Anthropic));
    assert!(ids.contains(&OAuthProviderId::OpenAICodex));
    assert!(ids.contains(&OAuthProviderId::GithubCopilot));
}

#[test]
fn normalize_domain_handles_common_inputs() {
    // Bare hostname.
    assert_eq!(normalize_domain("github.com"), Some("github.com".into()));
    // Whitespace is trimmed.
    assert_eq!(
        normalize_domain("  github.com  "),
        Some("github.com".into())
    );
    // Schemes are stripped.
    assert_eq!(
        normalize_domain("https://company.ghe.com"),
        Some("company.ghe.com".into())
    );
    // Path/query are stripped, hostname preserved.
    assert_eq!(
        normalize_domain("https://company.ghe.com/some/path?x=1"),
        Some("company.ghe.com".into())
    );
    // Port is dropped.
    assert_eq!(
        normalize_domain("https://company.ghe.com:8443/path"),
        Some("company.ghe.com".into())
    );
    // Hostname is normalized to lower case.
    assert_eq!(
        normalize_domain("HTTPS://Company.GHE.com"),
        Some("company.ghe.com".into())
    );
    // Empty / whitespace-only -> None.
    assert_eq!(normalize_domain(""), None);
    assert_eq!(normalize_domain("   "), None);
}

#[test]
fn github_copilot_base_url_extracts_proxy_ep_from_token() {
    // Tokens are `key=value;key=value;...` strings; we want the `proxy-ep`
    // host translated from `proxy.<rest>` to `https://api.<rest>`.
    let token = "tid=abc;exp=123;proxy-ep=proxy.individual.githubcopilot.com;more=1";
    assert_eq!(
        github_copilot_base_url(Some(token), None),
        "https://api.individual.githubcopilot.com"
    );
    // Trailing segment (no semicolon after proxy-ep) is also accepted.
    let token_tail = "tid=abc;proxy-ep=proxy.business.githubcopilot.com";
    assert_eq!(
        github_copilot_base_url(Some(token_tail), None),
        "https://api.business.githubcopilot.com"
    );
}

#[test]
fn github_copilot_base_url_falls_back_to_enterprise_or_default() {
    // No token, no enterprise: documented public default.
    assert_eq!(
        github_copilot_base_url(None, None),
        "https://api.individual.githubcopilot.com"
    );
    // No token, enterprise domain supplied: copilot-api subdomain.
    assert_eq!(
        github_copilot_base_url(None, Some("company.ghe.com")),
        "https://copilot-api.company.ghe.com"
    );
    // Token present but missing proxy-ep: still falls back through token-first
    // path to enterprise default.
    let token = "tid=abc;exp=123";
    assert_eq!(
        github_copilot_base_url(Some(token), Some("company.ghe.com")),
        "https://copilot-api.company.ghe.com"
    );
}

// ---------------------------------------------------------------------------
// Per-provider flow tests
//
// These ports of the TS suites
//   pi-mono/.../test/anthropic-oauth.test.ts
//   pi-mono/.../test/openai-codex-oauth.test.ts
//   pi-mono/.../test/github-copilot-oauth.test.ts
// stand up a tiny_http mock server on a free port, route a small set of
// scripted responses, and assert the provider builds the correct request and
// surfaces fields from the response correctly.
// ---------------------------------------------------------------------------

/// Configure a tiny_http server on `127.0.0.1:0` (kernel-assigned port) and
/// run `handler` on a background thread for each incoming request. The
/// returned `(base_url, shutdown)` tuple lets tests build full URLs and
/// terminate the server when done.
struct MockServer {
    base_url: String,
    _join: thread::JoinHandle<()>,
    server: Arc<tiny_http::Server>,
}

impl MockServer {
    fn start<F>(mut handler: F) -> Self
    where
        F: FnMut(tiny_http::Request) + Send + 'static,
    {
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").expect("bind mock"));
        let addr = server.server_addr();
        let port = addr.to_ip().expect("ip addr").port();
        let base_url = format!("http://127.0.0.1:{port}");
        let server_clone = Arc::clone(&server);
        let join = thread::spawn(move || {
            for req in server_clone.incoming_requests() {
                handler(req);
            }
        });
        MockServer {
            base_url,
            _join: join,
            server,
        }
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        // Unblock incoming_requests() so the worker thread exits.
        self.server.unblock();
    }
}

fn html_text_response(status: u16, body: &str) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    let bytes = body.as_bytes().to_vec();
    let len = bytes.len();
    tiny_http::Response::new(
        tiny_http::StatusCode(status),
        vec![
            tiny_http::Header::from_bytes(b"Content-Type".as_ref(), b"application/json".as_ref())
                .unwrap(),
        ],
        std::io::Cursor::new(bytes),
        Some(len),
        None,
    )
}

#[tokio::test]
async fn anthropic_oauth_flow_exchanges_code_for_tokens() {
    // The TS `anthropic-oauth.test.ts` suite verifies that the refresh request
    // sends the documented JSON body and that the new credentials reflect the
    // server's `access_token` / `refresh_token` / `expires_in`. We mirror that
    // here by mocking the token endpoint.
    let server = MockServer::start(|req| {
        let mut body = String::new();
        let mut reader = req;
        let _ = std::io::Read::read_to_string(reader.as_reader(), &mut body);
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["grant_type"], "refresh_token");
        assert_eq!(parsed["refresh_token"], "test-refresh");
        let response =
            r#"{"access_token":"new-access","refresh_token":"new-refresh","expires_in":3600}"#;
        let _ = reader.respond(html_text_response(200, response));
    });

    let provider = AnthropicOAuthProvider::with_token_url(server.base_url.clone());
    let dummy = OAuthCredentials {
        access_token: "old".into(),
        refresh_token: Some("test-refresh".into()),
        expires_at: Some(0),
        scope: None,
        extra: None,
    };
    let new_creds = provider.refresh(&dummy).await.expect("refresh ok");
    assert_eq!(new_creds.access_token, "new-access");
    assert_eq!(new_creds.refresh_token.as_deref(), Some("new-refresh"));
    let exp = new_creds.expires_at.expect("expires_at set");
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    // 3600s expiry minus the 5-minute safety window: should land in the
    // (now + ~55 min, now + 60 min) range.
    assert!(exp > now_ms + 50 * 60 * 1000, "expiry too early: {exp}");
    assert!(exp < now_ms + 65 * 60 * 1000, "expiry too late: {exp}");
}

#[tokio::test]
async fn openai_codex_oauth_refresh_decodes_account_id() {
    // Build an access-token JWT whose claim namespace contains
    // `chatgpt_account_id`, then verify the provider lifts it into `extra`.
    fn b64url(payload: &serde_json::Value) -> String {
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(payload).unwrap())
    }
    let header = b64url(&serde_json::json!({"alg":"none","typ":"JWT"}));
    let payload = b64url(&serde_json::json!({
        "https://api.openai.com/auth": { "chatgpt_account_id": "acc_123" }
    }));
    let signature = "sig"; // ignored by decoder
    let access_jwt = format!("{header}.{payload}.{signature}");
    let body = format!(
        r#"{{"access_token":"{access_jwt}","refresh_token":"new-refresh","expires_in":3600}}"#
    );

    let body_clone = body.clone();
    let server = MockServer::start(move |req| {
        let _ = req.respond(html_text_response(200, &body_clone));
    });

    let provider = OpenAiCodexOAuthProvider::with_token_url(server.base_url.clone());
    let dummy = OAuthCredentials {
        access_token: "old".into(),
        refresh_token: Some("test-refresh".into()),
        expires_at: Some(0),
        scope: None,
        extra: None,
    };
    let new_creds = provider.refresh(&dummy).await.expect("refresh ok");
    assert_eq!(new_creds.access_token, access_jwt);
    let extra = new_creds.extra.expect("extra populated from JWT");
    assert_eq!(extra["chatgpt_account_id"], "acc_123");
}

#[tokio::test]
async fn github_copilot_device_flow_polls_until_authorized() {
    // The TS suite scripts a sequence of (device_code, pending, success,
    // copilot_token) responses and asserts the provider polls until it gets
    // the access token. We reproduce that with a single mock server that
    // dispatches by request path and uses a counter to alternate the
    // /login/oauth/access_token responses.
    let poll_count = Arc::new(AtomicUsize::new(0));
    let poll_count_clone = Arc::clone(&poll_count);

    let server = MockServer::start(move |req| {
        let path = req.url().split('?').next().unwrap_or("").to_string();
        match path.as_str() {
            "/login/device/code" => {
                let body = r#"{
                    "device_code": "DEVICE_CODE",
                    "user_code": "USER-CODE",
                    "verification_uri": "https://example.test/device",
                    "interval": 0,
                    "expires_in": 30
                }"#;
                let _ = req.respond(html_text_response(200, body));
            }
            "/login/oauth/access_token" => {
                let n = poll_count_clone.fetch_add(1, Ordering::SeqCst);
                let body = if n == 0 {
                    // First poll: not yet authorized — provider should retry.
                    r#"{"error":"authorization_pending"}"#
                } else {
                    r#"{"access_token":"gh-access-token","token_type":"bearer","scope":"read:user"}"#
                };
                let _ = req.respond(html_text_response(200, body));
            }
            "/copilot_internal/v2/token" => {
                // expires_at is seconds; pick "far future" so the credential
                // is not seen as expired.
                let body = r#"{"token":"copilot-session-token","expires_at":4102444800}"#;
                let _ = req.respond(html_text_response(200, body));
            }
            other => {
                let _ = req.respond(html_text_response(
                    404,
                    &format!("{{\"error\":\"unknown route {other}\"}}"),
                ));
            }
        }
    });

    let endpoints = GithubEndpoints {
        device_code_url: format!("{}/login/device/code", server.base_url),
        access_token_url: format!("{}/login/oauth/access_token", server.base_url),
        copilot_token_url: format!("{}/copilot_internal/v2/token", server.base_url),
    };
    let provider = GithubCopilotOAuthProvider::with_endpoints(endpoints);

    let device_code_seen = Arc::new(std::sync::Mutex::new(None::<(String, String)>));
    let device_code_clone = Arc::clone(&device_code_seen);
    let callbacks = OAuthLoginCallbacks {
        on_open_url: Box::new(|_| {}),
        on_device_code: Box::new(move |code, url| {
            *device_code_clone.lock().unwrap() = Some((code.to_string(), url.to_string()));
        }),
    };

    let creds = provider.login(&callbacks).await.expect("login ok");
    assert_eq!(creds.access_token, "copilot-session-token");
    assert_eq!(creds.refresh_token.as_deref(), Some("gh-access-token"));
    assert!(
        poll_count.load(Ordering::SeqCst) >= 2,
        "should have polled at least twice (got {})",
        poll_count.load(Ordering::SeqCst)
    );
    let captured = device_code_seen.lock().unwrap().clone();
    assert_eq!(
        captured,
        Some((
            "USER-CODE".to_string(),
            "https://example.test/device".to_string()
        ))
    );
}

// ---------------------------------------------------------------------------
// Production-readiness regression tests (file perms, debug redaction,
// concurrent saves).
// ---------------------------------------------------------------------------

fn dummy_auth_info() -> OAuthAuthInfo {
    OAuthAuthInfo {
        provider_id: OAuthProviderId::Anthropic,
        credentials: OAuthCredentials {
            access_token: "tok".into(),
            refresh_token: Some("ref".into()),
            expires_at: Some(0),
            scope: None,
            extra: None,
        },
        created_at_ms: 0,
    }
}

#[cfg(unix)]
#[tokio::test]
async fn oauth_storage_file_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("oauth.json");
    let registry = OAuthRegistry::with_storage_path(path.clone());
    registry.save(&dummy_auth_info()).await.unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "oauth.json must be 0600");
}

#[cfg(unix)]
#[tokio::test]
async fn oauth_storage_parent_dir_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let parent = tmp.path().join("nested-oauth-dir");
    let path = parent.join("oauth.json");
    let registry = OAuthRegistry::with_storage_path(path);
    registry.save(&dummy_auth_info()).await.unwrap();
    let mode = std::fs::metadata(&parent).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700, "parent dir must be 0700");
}

#[test]
fn oauth_credentials_debug_redacts_tokens() {
    let creds = OAuthCredentials {
        access_token: "secret-access".into(),
        refresh_token: Some("secret-refresh".into()),
        expires_at: Some(1000),
        scope: Some("scope-x".into()),
        extra: None,
    };
    let dbg = format!("{:?}", creds);
    assert!(!dbg.contains("secret-access"), "access_token leaked: {dbg}");
    assert!(
        !dbg.contains("secret-refresh"),
        "refresh_token leaked: {dbg}"
    );
    assert!(dbg.contains("redacted"), "expected redaction marker: {dbg}");
    // Non-secret fields must remain visible for diagnostics.
    assert!(dbg.contains("scope-x"), "scope should be visible: {dbg}");
    assert!(dbg.contains("1000"), "expires_at should be visible: {dbg}");
}

#[test]
fn oauth_auth_info_debug_redacts_tokens() {
    // Make sure the redaction transitively applies through OAuthAuthInfo
    // (the type that ends up in tracing/logs).
    let info = OAuthAuthInfo {
        provider_id: OAuthProviderId::OpenAICodex,
        credentials: OAuthCredentials {
            access_token: "secret-access".into(),
            refresh_token: Some("secret-refresh".into()),
            expires_at: None,
            scope: None,
            extra: None,
        },
        created_at_ms: 42,
    };
    let dbg = format!("{:?}", info);
    assert!(!dbg.contains("secret-access"));
    assert!(!dbg.contains("secret-refresh"));
    assert!(dbg.contains("redacted"));
    assert!(dbg.contains("OpenAICodex"));
    assert!(dbg.contains("42"));
}

#[tokio::test]
async fn concurrent_saves_preserve_all_records() {
    let tmp = tempfile::tempdir().unwrap();
    let registry = Arc::new(OAuthRegistry::with_storage_path(
        tmp.path().join("oauth.json"),
    ));
    let mut handles = vec![];
    for (i, id) in [
        OAuthProviderId::Anthropic,
        OAuthProviderId::OpenAICodex,
        OAuthProviderId::GithubCopilot,
    ]
    .iter()
    .enumerate()
    {
        let r = Arc::clone(&registry);
        let id = *id;
        handles.push(tokio::spawn(async move {
            let info = OAuthAuthInfo {
                provider_id: id,
                credentials: OAuthCredentials {
                    access_token: format!("token-{i}"),
                    refresh_token: None,
                    expires_at: None,
                    scope: None,
                    extra: None,
                },
                created_at_ms: 1000 + i as u64,
            };
            r.save(&info).await.unwrap();
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    let loaded = registry.load().await.unwrap();
    assert_eq!(
        loaded.len(),
        3,
        "all three concurrent saves must be persisted"
    );
}
