//! Driver-side glue for the `/login`, `/logout`, and first-run login flows.
//!
//! The [overlay runtime](super::overlay) supplies the mount/dispatch/close
//! machinery; this module supplies the *session-aware* half: it reads the provider
//! catalog + credential state off the [`AgentSession`], mounts the right overlay
//! (provider picker → key dialog, or the OAuth flow), and applies the result
//! (persist an API key, run the OAuth login, or clear credentials).
//!
//! # Concurrency
//!
//! Every `open_*` runs on the **turn-runner task** (the one place that owns
//! `&mut AgentSession`), so it can `await` an overlay outcome or an OAuth login
//! future and then apply the result. While it awaits, the **input loop** routes keys
//! (and pastes — VAL-OVERLAY-027) into the mounted overlay.
//!
//! # OAuth-vs-key routing (VAL-OVERLAY-034)
//!
//! `/login <provider>` splits **case-insensitively**: a provider that has an OAuth
//! implementation (`anthropic`, `openai-codex`, `github-copilot`) runs the OAuth
//! flow; anything else opens the API-key dialog. Matching the lowercased id is the
//! regression fix — `/login Anthropic` used to fall through to the key dialog because
//! the match was case-sensitive.
//!
//! # Isolation
//!
//! Credentials persist through [`AuthStorage::new`], which resolves
//! `~/.hand/agent/auth.json` via `$HOME` — so a probe that redirects `$HOME` (and
//! `HAND_HOME`) to a temp dir keeps the write isolated (see
//! `docs/user-test-patterns.md`).

use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use hand_tui::rt::scheduler::FrameRequester;
use model::oauth::{OAuthLoginCallbacks, OAuthProviderId, OAuthRegistry};
use tokio::sync::mpsc;

use crate::core::agent_session::AgentSession;
use crate::core::auth_storage::{AuthRecord, AuthStorage};
use crate::core::model_registry::{AuthSource, ModelRegistry};

use super::chat;
use super::login_dialog::{KeyDialogOutcome, LoginKeyDialog};
use super::login_provider_picker::{
    AuthMethod, LoginProviderOutcome, LoginProviderPicker, LoginProviderRow, ProviderBadge,
};
use super::oauth_flow::{OAuthFlowOverlay, OAuthStatus};
use super::overlay::{self, DoneSignal, SelectorController, SharedOverlay};
use super::state::{DriverState, lock_state};

/// The one-line welcome banner shown on a fresh temp home with no credentials, just
/// before the login picker auto-opens (VAL-OVERLAY-022).
pub const WELCOME_NO_CREDENTIALS: &str =
    "Welcome to hand. No provider credentials were found — opening /login.";

/// Map a provider id to its OAuth implementation id, if one exists. The comparison
/// is **case-insensitive** so `/login Anthropic` routes to OAuth (VAL-OVERLAY-034).
#[must_use]
pub fn oauth_provider_id(provider: &str) -> Option<OAuthProviderId> {
    match provider.trim().to_lowercase().as_str() {
        "anthropic" => Some(OAuthProviderId::Anthropic),
        "openai-codex" => Some(OAuthProviderId::OpenAICodex),
        "github-copilot" => Some(OAuthProviderId::GithubCopilot),
        _ => None,
    }
}

/// Whether any provider in the session's catalog has a usable credential (stored or
/// via env var). Drives the first-run onboarding gate (VAL-OVERLAY-022).
#[must_use]
pub fn any_provider_has_credentials(session: &AgentSession) -> bool {
    registry_has_credentials(session.model_registry())
}

/// The `&ModelRegistry` core of [`any_provider_has_credentials`], testable against a
/// directly-built registry.
#[must_use]
pub fn registry_has_credentials(registry: &ModelRegistry) -> bool {
    let mut seen: HashSet<String> = HashSet::new();
    for model in registry.all() {
        let pid = model.provider.as_str();
        if !seen.insert(pid.to_string()) {
            continue;
        }
        if registry.has_provider_auth_configured(pid) {
            return true;
        }
    }
    false
}

/// Build the `/login` provider list from the session's model catalog: one row per
/// unique provider id, each with its credential badge (green `configured` / yellow
/// `env detected` / none) and the auth method it uses (OAuth vs API key). Sorted by
/// display name. Kept pure over the registry so the badge rule is unit-testable.
#[must_use]
pub fn build_provider_rows(session: &AgentSession) -> Vec<LoginProviderRow> {
    build_provider_rows_from(session.model_registry())
}

/// The `&ModelRegistry` core of [`build_provider_rows`], testable against a
/// directly-built registry.
#[must_use]
pub fn build_provider_rows_from(registry: &ModelRegistry) -> Vec<LoginProviderRow> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut rows: Vec<LoginProviderRow> = Vec::new();
    for model in registry.all() {
        let id = model.provider.as_str().to_string();
        if !seen.insert(id.clone()) {
            continue;
        }
        let name = registry.provider_display_name(&id);
        let status = registry.provider_auth_status(&id);
        let badge = match (status.configured, status.source) {
            (true, _) => ProviderBadge::Configured,
            (false, Some(AuthSource::Environment)) => ProviderBadge::EnvDetected,
            _ => ProviderBadge::None,
        };
        let method = if oauth_provider_id(&id).is_some() {
            AuthMethod::Oauth
        } else {
            AuthMethod::ApiKey
        };
        rows.push(LoginProviderRow {
            id,
            name,
            badge,
            method,
        });
    }
    rows.sort_by_key(|r| r.name.to_lowercase());
    rows
}

/// Run the `/login` flow: with an explicit provider go straight to its flow;
/// otherwise open the provider picker first (VAL-OVERLAY-015 / -027 / -028 / -034).
pub async fn open_login(
    session: &mut AgentSession,
    provider: Option<&str>,
    overlay: &SharedOverlay,
    done: &DoneSignal,
    state: &Arc<Mutex<DriverState>>,
    requester: &FrameRequester,
) {
    // `/login <provider>` skips the picker (power-user path + the direct-arg
    // routing test); a bare `/login` opens the picker to choose one.
    let chosen = match provider.map(str::trim).filter(|s| !s.is_empty()) {
        Some(p) => Some(p.to_string()),
        None => {
            open_provider_picker(
                session,
                "Select a provider to log in",
                overlay,
                done,
                state,
                requester,
            )
            .await
        }
    };
    let Some(provider_id) = chosen else {
        return;
    };

    // OAuth-vs-key split, case-insensitive (VAL-OVERLAY-034).
    if oauth_provider_id(&provider_id).is_some() {
        run_oauth_login(session, &provider_id, overlay, done, state, requester).await;
    } else {
        open_key_dialog(session, &provider_id, overlay, done, state, requester).await;
    }
}

/// Mount the provider picker and return the chosen provider id (or `None` on cancel
/// / empty catalog). A catalog with no providers takes the no-data degradation: no
/// overlay opens and the `[/login: no providers available]` status line lands
/// (VAL-OVERLAY-019, login part).
async fn open_provider_picker(
    session: &AgentSession,
    title: &str,
    overlay: &SharedOverlay,
    done: &DoneSignal,
    state: &Arc<Mutex<DriverState>>,
    requester: &FrameRequester,
) -> Option<String> {
    let rows = build_provider_rows(session);
    if rows.is_empty() {
        commit_status(state, requester, "[/login: no providers available]");
        return None;
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<LoginProviderOutcome>();
    done.store(false, Ordering::SeqCst);
    let picker = LoginProviderPicker::new(title.to_string(), rows, tx, done.clone());
    let controller: Arc<Mutex<dyn SelectorController>> = Arc::new(Mutex::new(picker));
    overlay::mount(overlay, requester, controller, done.clone());

    match rx.recv().await {
        Some(LoginProviderOutcome::Selected(id)) => Some(id),
        Some(LoginProviderOutcome::Cancelled) => {
            commit_status(state, requester, "[/login cancelled]");
            None
        }
        None => {
            overlay::close(overlay, requester);
            None
        }
    }
}

/// Open the API-key dialog for `provider_id`, and on submit persist the key to
/// `auth.json` (VAL-OVERLAY-015). The confirmation names the *provider*, never the
/// key, so the secret never lands in scrollback (VAL-OVERLAY-016).
async fn open_key_dialog(
    session: &AgentSession,
    provider_id: &str,
    overlay: &SharedOverlay,
    done: &DoneSignal,
    state: &Arc<Mutex<DriverState>>,
    requester: &FrameRequester,
) {
    // Canonicalise a known provider id (so `/login OpenAI` stores under `openai`);
    // unknown ids are accepted verbatim so a user can log in to a provider we don't
    // statically know about.
    let canonical = model::types::Provider::from_str(provider_id)
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| provider_id.to_string());
    let display = session.model_registry().provider_display_name(&canonical);

    let (tx, mut rx) = mpsc::unbounded_channel::<KeyDialogOutcome>();
    done.store(false, Ordering::SeqCst);
    let dialog = LoginKeyDialog::new(display, tx, done.clone());
    let controller: Arc<Mutex<dyn SelectorController>> = Arc::new(Mutex::new(dialog));
    overlay::mount(overlay, requester, controller, done.clone());

    match rx.recv().await {
        Some(KeyDialogOutcome::Submitted(key)) => {
            persist_api_key(&canonical, key, state, requester)
        }
        Some(KeyDialogOutcome::Cancelled) => {
            commit_status(state, requester, "[/login cancelled]");
        }
        None => overlay::close(overlay, requester),
    }
}

/// Persist a submitted API key for `provider` and land the confirmation line.
///
/// The confirmation is `[login: api key saved for <provider>]` — it names the
/// provider only, so the key never appears in scrollback (VAL-OVERLAY-016). The key
/// itself is dropped after the write.
fn persist_api_key(
    provider: &str,
    key: String,
    state: &Arc<Mutex<DriverState>>,
    requester: &FrameRequester,
) {
    // `AuthStorage::new()` resolves `~/.hand/agent/auth.json` via `$HOME`, so a probe
    // that redirects `$HOME` keeps the write isolated.
    let result = AuthStorage::new().and_then(|s| s.set(provider, AuthRecord::ApiKey { key }));
    match result {
        Ok(()) => commit_status(
            state,
            requester,
            &format!("[login: api key saved for {provider}]"),
        ),
        Err(e) => commit_error(
            state,
            requester,
            &format!("[/login failed to save key: {e}]"),
        ),
    }
}

/// Run the OAuth login for `provider_id`: mount the progress overlay, run the
/// provider's `login()` with **browser launch suppressed** (the authorize URL is
/// only *shown*), and on completion persist or report (VAL-OVERLAY-028).
///
/// A failure (network blocked, callback timeout, user cancel) unmounts the overlay
/// and lands the red `[oauth: login failed: …]` line; the editor is reachable again
/// on the next key.
async fn run_oauth_login(
    session: &AgentSession,
    provider_id: &str,
    overlay: &SharedOverlay,
    done: &DoneSignal,
    state: &Arc<Mutex<DriverState>>,
    requester: &FrameRequester,
) {
    let Some(oauth_id) = oauth_provider_id(provider_id) else {
        // Unreachable in practice (the caller checked), but keep total.
        commit_error(state, requester, "[oauth: no OAuth flow for this provider]");
        return;
    };
    let display = session.model_registry().provider_display_name(provider_id);

    // The shared status buffer: the login callbacks push progress here, the overlay
    // renders it, and Esc flags a cancel the overlay observes.
    let status = OAuthStatus::new(vec![format!("Starting OAuth login for {display}…")]);
    done.store(false, Ordering::SeqCst);
    let ov = OAuthFlowOverlay::new(display.clone(), status.clone(), done.clone());
    let controller: Arc<Mutex<dyn SelectorController>> = Arc::new(Mutex::new(ov));
    overlay::mount(overlay, requester, controller, done.clone());

    // Build callbacks that only *show* the URL / device code — never launch a
    // browser (the probe-suppression requirement). Each push repaints via the
    // requester so the URL appears live.
    let callbacks = build_status_callbacks(&status, requester);

    let registry = OAuthRegistry::new();
    let Some(provider) = registry.get(oauth_id) else {
        overlay::close(overlay, requester);
        commit_error(state, requester, "[oauth: provider not registered]");
        return;
    };

    // Race the provider login against the overlay's Esc-cancel flag: the login
    // future blocks on a loopback callback the browser never hits under a
    // network-blocked probe (it would otherwise wait out the provider's internal
    // timeout), so a poll on the shared cancel flag lets Esc abort the wait promptly
    // and land the failure line — the editor (on its own task) stays usable
    // throughout (VAL-OVERLAY-028). Dropping the login future cancels its in-flight
    // I/O.
    let outcome = tokio::select! {
        result = provider.login(&callbacks) => Some(result),
        () = poll_cancel(&status) => None,
    };

    // The flow is done (or cancelled): unmount the overlay regardless of the result.
    overlay::close(overlay, requester);

    match outcome {
        Some(Ok(creds)) => {
            // Persist through the OAuth registry (its own store), then confirm.
            let info = model::oauth::OAuthAuthInfo {
                provider_id: oauth_id,
                credentials: creds,
                created_at_ms: now_ms(),
            };
            match registry.save(&info).await {
                Ok(()) => commit_status(
                    state,
                    requester,
                    &format!("[login: oauth session saved for {provider_id}]"),
                ),
                Err(e) => commit_error(
                    state,
                    requester,
                    &format!("[oauth: login succeeded but save failed: {e}]"),
                ),
            }
        }
        Some(Err(e)) => {
            // Network-blocked / callback timeout land here; report red and leave the
            // session usable (VAL-OVERLAY-028).
            commit_error(state, requester, &format!("[oauth: login failed: {e}]"));
        }
        None => {
            // Esc cancelled the wait — report the cancel, drop the login future.
            commit_status(state, requester, "[oauth: login cancelled]");
        }
    }
}

/// Resolve once the shared OAuth status is flagged cancelled (the user pressed Esc).
///
/// Polls the flag on a short tick rather than holding a notifier — the overlay only
/// sets an [`AtomicBool`](std::sync::atomic::AtomicBool), so a light poll keeps the
/// runtime coupling minimal while still aborting the login wait within a frame or two.
async fn poll_cancel(status: &OAuthStatus) {
    loop {
        if status.is_cancelled() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// Build OAuth callbacks that push progress into `status` and repaint — never
/// launching a browser (VAL-OVERLAY-028 probe suppression).
fn build_status_callbacks(status: &OAuthStatus, requester: &FrameRequester) -> OAuthLoginCallbacks {
    let url_status = status.clone();
    let url_requester = requester.clone();
    let code_status = status.clone();
    let code_requester = requester.clone();
    OAuthLoginCallbacks {
        on_open_url: Box::new(move |url| {
            url_status.push(format!("Open this URL to continue: {url}"));
            url_status.push("(browser launch suppressed — copy the URL manually)");
            url_requester.request_frame();
        }),
        on_device_code: Box::new(move |user_code, verification_url| {
            code_status.push(format!(
                "Visit {verification_url} and enter code: {user_code}"
            ));
            code_requester.request_frame();
        }),
    }
}

/// Run the `/logout` flow: with an explicit provider clear just that one; otherwise
/// clear every stored credential (VAL-OVERLAY-029). Clearing removes the provider's
/// `configured` badge on the next `/login`.
pub async fn open_logout(
    session: &mut AgentSession,
    provider: Option<&str>,
    state: &Arc<Mutex<DriverState>>,
    requester: &FrameRequester,
) {
    let _ = session; // logout reads no session state; kept for signature parity.
    let storage = match AuthStorage::new() {
        Ok(s) => s,
        Err(e) => {
            commit_error(state, requester, &format!("[/logout failed: {e}]"));
            return;
        }
    };

    let result = match provider.map(str::trim).filter(|s| !s.is_empty()) {
        Some(p) => {
            let canonical = model::types::Provider::from_str(p)
                .map(|prov| prov.as_str().to_string())
                .unwrap_or_else(|| p.to_string());
            storage.remove(&canonical)
        }
        None => clear_all(&storage),
    };

    match result {
        Ok(()) => commit_status(state, requester, "[logged out]"),
        Err(e) => commit_error(state, requester, &format!("[/logout failed: {e}]")),
    }
}

/// Remove every stored credential. Loading an empty store is a no-op success, so a
/// `/logout` with nothing on file still reports `[logged out]`.
fn clear_all(storage: &AuthStorage) -> Result<(), crate::core::auth_storage::AuthStorageError> {
    let records = storage.load()?;
    for provider in records.keys() {
        storage.remove(provider)?;
    }
    Ok(())
}

/// Current epoch millis (for the OAuth record's `created_at_ms`).
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Commit a yellow status block to scrollback and request a repaint.
fn commit_status(state: &Arc<Mutex<DriverState>>, requester: &FrameRequester, text: &str) {
    let lines = chat::status_lines_for(text);
    if lines.is_empty() {
        return;
    }
    lock_state(state).queue_commit(lines);
    requester.request_frame();
}

/// Commit a red error block to scrollback and request a repaint.
fn commit_error(state: &Arc<Mutex<DriverState>>, requester: &FrameRequester, text: &str) {
    let lines = chat::error_lines(text);
    if lines.is_empty() {
        return;
    }
    lock_state(state).queue_commit(lines);
    requester.request_frame();
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::core::model_registry::ModelRegistry;
    use tempfile::TempDir;

    /// A registry over the built-in catalog, bound to an isolated (empty) auth store
    /// in `dir`. Every built-in provider is present in `all()`; none is configured
    /// unless a credential is seeded into the store first (or an ambient env var
    /// matches — badge assertions therefore use `amazon-bedrock`, which has no
    /// env-var mapping).
    fn registry_in(dir: &TempDir) -> ModelRegistry {
        ModelRegistry::in_memory(AuthStorage::at(dir.path().join("auth.json")))
    }

    /// A registry with a stored api-key credential for `provider` (marks it
    /// configured — green badge).
    fn registry_with_stored_key(dir: &TempDir, provider: &str) -> ModelRegistry {
        let storage = AuthStorage::at(dir.path().join("auth.json"));
        storage
            .set(provider, AuthRecord::api_key("sk-stored-test-key"))
            .expect("seed stored credential");
        ModelRegistry::in_memory(storage)
    }

    // --- OAuth-vs-key routing is case-insensitive (VAL-OVERLAY-034) -------

    #[test]
    fn oauth_provider_id_is_case_insensitive() {
        assert_eq!(
            oauth_provider_id("anthropic"),
            Some(OAuthProviderId::Anthropic)
        );
        assert_eq!(
            oauth_provider_id("Anthropic"),
            Some(OAuthProviderId::Anthropic)
        );
        assert_eq!(
            oauth_provider_id("ANTHROPIC"),
            Some(OAuthProviderId::Anthropic)
        );
        assert_eq!(
            oauth_provider_id("openai-codex"),
            Some(OAuthProviderId::OpenAICodex)
        );
        assert_eq!(
            oauth_provider_id("github-copilot"),
            Some(OAuthProviderId::GithubCopilot)
        );
        // A plain api-key provider has no OAuth flow → routes to the key dialog.
        assert_eq!(oauth_provider_id("openai"), None);
        assert_eq!(oauth_provider_id("google"), None);
        assert_eq!(oauth_provider_id("openrouter"), None);
    }

    // --- provider list + badges (VAL-OVERLAY-015 / -029) ------------------
    //
    // Badge assertions use `amazon-bedrock` — a known provider with **no** env-var
    // mapping — so a stray key in the ambient shell can never flip the badge and the
    // unit test is deterministic. The env-detected badge and the OAuth-configured
    // badge are exercised end-to-end by the tmux probe under HOME isolation.

    #[test]
    fn build_provider_rows_configured_badge_from_stored_key() {
        let dir = TempDir::new().unwrap();
        // A stored api key marks the provider configured (green badge).
        let registry = registry_with_stored_key(&dir, "amazon-bedrock");
        let rows = build_provider_rows_from(&registry);
        let row = rows.iter().find(|r| r.id == "amazon-bedrock").unwrap();
        assert_eq!(row.badge, ProviderBadge::Configured);
        // A non-OAuth provider routes to the api-key dialog.
        assert_eq!(row.method, AuthMethod::ApiKey);
    }

    #[test]
    fn build_provider_rows_no_badge_when_unconfigured() {
        let dir = TempDir::new().unwrap();
        let registry = registry_in(&dir);
        let rows = build_provider_rows_from(&registry);
        // amazon-bedrock has no env-var mapping, so an empty store leaves it
        // badgeless regardless of the ambient shell.
        let row = rows.iter().find(|r| r.id == "amazon-bedrock").unwrap();
        assert_eq!(row.badge, ProviderBadge::None);
    }

    #[test]
    fn build_provider_rows_tags_oauth_method() {
        let dir = TempDir::new().unwrap();
        let registry = registry_in(&dir);
        let rows = build_provider_rows_from(&registry);
        let anthropic = rows.iter().find(|r| r.id == "anthropic").unwrap();
        // anthropic has an OAuth flow → the picker opens OAuth, not the key dialog.
        assert_eq!(anthropic.method, AuthMethod::Oauth);
        assert_eq!(anthropic.name, "Anthropic");
    }

    // --- first-run gate (VAL-OVERLAY-022) ---------------------------------

    #[test]
    fn registry_has_credentials_true_when_a_provider_is_configured() {
        let dir = TempDir::new().unwrap();
        // A stored credential → no first-run onboarding.
        let configured = registry_with_stored_key(&dir, "amazon-bedrock");
        assert!(registry_has_credentials(&configured));
    }
}
