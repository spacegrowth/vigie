//! Every Tauri command from docs/CONTRACT.md's "Tauri commands" section,
//! delegating to `Arc<dyn EngineApi>` (held in `EngineState` so the poller
//! task shares the one instance). Argument/return shapes mirror the
//! contract's JSON types exactly — they *are* the engine's own types now,
//! re-exported from `gitmon::types`, so the two sides cannot drift.
//!
//! Every command carries `rename_all = "snake_case"`: the wire format is
//! snake_case end to end (the engine types serialize snake_case and the
//! frontend sends snake_case keys), while Tauri's default is camelCase —
//! without the attribute, `device_code` would arrive as `deviceCode` and
//! every multi-word argument would fail at the IPC boundary.
//!
//! Every command body runs on a blocking task (`blocking` below). The
//! engine is synchronous and most of its methods make network calls behind
//! a mutex, so running one inline would park a webview IPC worker — and,
//! through the store mutex, the background poller with it. Commands are
//! therefore `async fn` returning `Result<_, EngineError>`; an `Ok` value
//! still serialises to the plain JSON the frontend expects.

use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State, Url, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_opener::OpenerExt;

use crate::engine_api::EngineApi;
use crate::poller;
use crate::tray::refresh_tray_badge;
use crate::EngineState;
use gitmon::types::{
    Account, BackfillResult, CommitDetail, CommitFile, DeviceLogin, DeviceLoginStatus, Digest,
    Event, FilterMode, OpenPull, PersonSuggestion, PollResult, Repo, RepoSuggestion, Settings,
    Team, Thread, Watch,
};
use gitmon::{EngineError, ErrorKind};

/// Label of the Vigie-owned window the device flow opens on
/// `verification_uri_complete` — see CONTRACT.md "Sign-in": the app never
/// opens the system browser for sign-in.
const SIGNIN_WINDOW_LABEL: &str = "github-signin";

/// Injected into the GitHub sign-in webview: hides scrollbars there the same
/// way app.css does for the app's own pages (which this file cannot reach —
/// github.com is an external origin). Scrolling itself is untouched.
const SIGNIN_SCROLLBAR_HIDE_SCRIPT: &str = r#"
(function () {
  var css = 'html, body, * { scrollbar-width: none !important; } ' +
    '::-webkit-scrollbar { display: none !important; width: 0 !important; height: 0 !important; }';
  function inject() {
    var s = document.createElement('style');
    s.textContent = css;
    (document.head || document.documentElement).appendChild(s);
  }
  if (document.head) inject();
  else document.addEventListener('DOMContentLoaded', inject);
})();
"#;

/// The webview event emitted when a successful sign-in's token could not be
/// saved to the keychain. Payload: [`TokenSaveFailure`]. The sign-in itself
/// still succeeds — the token is live on the engine for this session — so
/// this is a warning the frontend shows, not an error that undoes it.
pub const TOKEN_SAVE_FAILED_EVENT: &str = "token-save-failed";

/// What the UI needs to explain the degraded sign-in: the keychain's own
/// failure message (OSStatus text included), so the cause is visible.
#[derive(Debug, Clone, Serialize)]
pub struct TokenSaveFailure {
    pub message: String,
}

/// Emitted when the in-app sign-in webview navigates somewhere that needs a
/// passkey. No payload: the URL has already been handed to the default
/// browser by then, so all the frontend has to do is say so.
pub const PASSKEY_HANDOFF_EVENT: &str = "passkey-handoff";

/// URL fragments that mark a GitHub page as a WebAuthn/passkey step —
/// `/sessions/passkey`, `/sessions/two-factor/webauthn` and friends.
///
/// A `WKWebView` outside a browser-entitled app exposes the WebAuthn API but
/// has no authenticator behind it, so such a page cannot complete in the
/// in-app window: measured against `https://webauthn.io` in a plain
/// `WKWebView`, `PublicKeyCredential` is a function and
/// `isUserVerifyingPlatformAuthenticatorAvailable()` is `false`, and
/// `navigator.credentials.create` rejects with `NotAllowedError: The request
/// is not allowed by the user agent or the platform in the current context`.
/// Hence the hand-off to the default browser (see `open_github_signin_window`).
const PASSKEY_URL_MARKERS: [&str; 2] = ["webauthn", "passkey"];

fn is_passkey_url(url: &Url) -> bool {
    let host_is_github =
        url.host_str().is_some_and(|h| h == "github.com" || h.ends_with(".github.com"));
    if !host_is_github {
        return false;
    }
    let path_and_query = format!("{}?{}", url.path(), url.query().unwrap_or_default())
        .to_ascii_lowercase();
    PASSKEY_URL_MARKERS.iter().any(|m| path_and_query.contains(m))
}

/// Runs one engine call off the IPC thread.
///
/// A failure here is the task itself dying (a panic inside the engine, or
/// shutdown racing the call), never a GitHub or storage failure — those
/// come back inside `T`. It is reported as `storage` because that is the
/// contract's kind for "something local went wrong".
pub async fn blocking<T, F>(f: F) -> Result<T, EngineError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| EngineError::storage(format!("engine call failed: {e}")))
}

/// The engine handle, cloned out of Tauri's state so it can move into a
/// blocking closure (`State` itself borrows the app and cannot).
fn engine(state: &State<'_, EngineState>) -> Arc<dyn EngineApi> {
    state.0.clone()
}

/// Whether the build got a real GitHub OAuth App client ID (see
/// `crate::GITHUB_CLIENT_ID`). The frontend calls this to disable the Sign
/// in button — and show why — rather than let a doomed request go out.
#[tauri::command(rename_all = "snake_case")]
pub fn github_client_id_ready() -> bool {
    crate::GITHUB_CLIENT_ID != "REPLACE_ME"
}

/// The OAuth app's own page in GitHub settings — where a member requests
/// organisation approval and an owner grants it. GitHub offers no API for
/// making that request, so landing the user on the button is the most an app
/// can do about an org that has not approved it.
#[tauri::command(rename_all = "snake_case")]
pub fn github_app_connection_url() -> String {
    format!(
        "https://github.com/settings/connections/applications/{}",
        crate::GITHUB_CLIENT_ID
    )
}

#[tauri::command(rename_all = "snake_case")]
pub async fn start_device_login(state: State<'_, EngineState>) -> Result<DeviceLogin, EngineError> {
    if crate::GITHUB_CLIENT_ID == "REPLACE_ME" {
        return Err(EngineError::invalid("Build is missing a GitHub client ID."));
    }
    let engine = engine(&state);
    blocking(move || engine.start_device_login(crate::GITHUB_CLIENT_ID)).await?
}

/// Opens (replacing any prior instance) the in-app "Sign in to GitHub"
/// window on the device flow's `verification_uri_complete` — the default
/// path, per CONTRACT.md "Sign-in". Sized and titled per the packet;
/// parented to the main window so it reads as modal to it.
///
/// The one thing this window cannot do is a passkey, so its navigation
/// handler watches for GitHub's WebAuthn pages and hands those — and only
/// those — to the default browser (see [`is_passkey_url`] and
/// [`open_signin_in_browser`]). The navigation is still allowed: the user
/// may come back and pick a password or a code instead, and the device flow
/// does not care which window finishes it — the poll loop is watching
/// GitHub, not this webview.
#[tauri::command(rename_all = "snake_case")]
pub fn open_github_signin_window(app: AppHandle, url: String) -> Result<(), EngineError> {
    if let Some(existing) = app.get_webview_window(SIGNIN_WINDOW_LABEL) {
        let _ = existing.close();
    }
    let target: Url =
        url.parse().map_err(|_| EngineError::invalid("That sign-in URL isn't valid."))?;
    let nav_app = app.clone();
    let mut builder =
        WebviewWindowBuilder::new(&app, SIGNIN_WINDOW_LABEL, WebviewUrl::External(target))
            .title("Sign in to GitHub")
            .inner_size(720.0, 760.0)
            .resizable(true)
            .min_inner_size(640.0, 600.0)
            .initialization_script(SIGNIN_SCROLLBAR_HIDE_SCRIPT)
            .on_navigation(move |url| {
                if is_passkey_url(url) {
                    hand_off_to_browser(&nav_app, url.as_str());
                }
                true
            });
    if let Some(main) = app.get_webview_window("main") {
        builder = builder.parent(&main).map_err(|e| EngineError::storage(e.to_string()))?;
    }
    builder.build().map_err(|e| EngineError::storage(e.to_string()))?;
    Ok(())
}

/// Opens `url` in the default browser and tells the frontend a passkey sent
/// us there. Best-effort by design: it runs from a navigation callback,
/// where there is no one to return an error to, and failing to open Safari
/// must not stop the in-app window from carrying on.
fn hand_off_to_browser(app: &AppHandle, url: &str) {
    match app.opener().open_url(url, None::<&str>) {
        Ok(()) => {
            let _ = app.emit(PASSKEY_HANDOFF_EVENT, ());
        }
        Err(e) => eprintln!("vigie: could not open {url} in the default browser: {e}"),
    }
}

/// Opens the device flow's verification URL in the user's default browser —
/// the "Use Safari instead" escape hatch for anything the in-app window
/// can't do (passkeys, above all). The poll loop is untouched: it is
/// watching GitHub for the approval, so whichever window the user finishes
/// in, the same `poll_device_login` call sees it.
#[tauri::command(rename_all = "snake_case")]
pub fn open_signin_in_browser(app: AppHandle, url: String) -> Result<(), EngineError> {
    let _: Url = url.parse().map_err(|_| EngineError::invalid("That sign-in URL isn't valid."))?;
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| EngineError::storage(format!("couldn't open your browser: {e}")))
}

fn close_signin_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(SIGNIN_WINDOW_LABEL) {
        let _ = w.close();
    }
}

/// Registers the account a just-obtained token belongs to and stores its
/// token under that account's own keychain item (docs/CONTRACT.md
/// "Accounts") — the shared last step of both a fresh sign-in
/// (`poll_device_login`, below) and "Add account" (`add_account_token`), so
/// the two are the same flow underneath.
///
/// A keychain-save failure does not undo the sign-in: the account is live on
/// the engine either way, and the caller is told via `TOKEN_SAVE_FAILED_EVENT`
/// so the UI can warn that it won't survive a relaunch.
fn add_account_and_store(
    app: &AppHandle,
    engine: &Arc<dyn EngineApi>,
    token: &str,
) -> Result<Account, EngineError> {
    let account = engine.add_account(token)?;
    if let Err(e) = crate::keychain::store_token_for(&account.login, token) {
        eprintln!("vigie: could not save the token for {}: {e}", account.login);
        let _ = app.emit(TOKEN_SAVE_FAILED_EVENT, TokenSaveFailure { message: e });
    }
    Ok(account)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn poll_device_login(
    app: AppHandle,
    state: State<'_, EngineState>,
    device_code: String,
) -> Result<DeviceLoginStatus, EngineError> {
    let engine = engine(&state);
    // `poll_device_login` already sets the token on the engine and resolves
    // the login on success, so nothing here has to re-verify it.
    let poll_engine = engine.clone();
    let blocking_app = app.clone();
    let result: Result<DeviceLoginStatus, EngineError> = blocking(move || {
        let status = poll_engine.poll_device_login(crate::GITHUB_CLIENT_ID, &device_code)?;
        if let DeviceLoginStatus::Ok { token, .. } = &status {
            // Registers the account (and stores its token) the same way
            // "Add account" does — synchronously, inside this same blocking
            // call, before `ok` is returned — so the frontend's
            // `onSignedIn` (which immediately calls
            // `list_accounts`/`poll_now`) can never observe `ok` before the
            // account row and keychain item exist. A registration failure
            // is surfaced as this call's own error instead of a false `ok`.
            add_account_and_store(&blocking_app, &poll_engine, token)?;
        }
        Ok(status)
    })
    .await?;
    match &result {
        Ok(DeviceLoginStatus::Ok { .. }) => close_signin_window(&app),
        // expired_token / access_denied: the flow is over either way, so
        // the in-app GitHub window has nothing left to do — close it.
        Err(e) if e.kind == ErrorKind::Auth => close_signin_window(&app),
        _ => {}
    }
    result
}

#[tauri::command(rename_all = "snake_case")]
pub async fn list_accounts(state: State<'_, EngineState>) -> Result<Vec<Account>, EngineError> {
    let engine = engine(&state);
    blocking(move || engine.list_accounts()).await
}

/// Verifies `token`, registers (or re-authenticates) the account it belongs
/// to, and stores it in that account's own keychain item — see
/// `add_account_and_store`. Also what `poll_device_login`'s success path
/// calls internally, so "Add account" and sign-in share one implementation.
#[tauri::command(rename_all = "snake_case")]
pub async fn add_account_token(
    app: AppHandle,
    state: State<'_, EngineState>,
    token: String,
) -> Result<Account, EngineError> {
    let engine = engine(&state);
    blocking(move || add_account_and_store(&app, &engine, &token)).await?
}

/// Removes the account and its own keychain item. Its repos are dropped by
/// the engine along with it (docs/CONTRACT.md "Accounts": `remove_account`
/// forgets the account, its repos, and their events and watches).
#[tauri::command(rename_all = "snake_case")]
pub async fn remove_account(state: State<'_, EngineState>, login: String) -> Result<(), EngineError> {
    let engine = engine(&state);
    blocking(move || {
        engine.remove_account(&login)?;
        if let Err(e) = crate::keychain::delete_token_for(&login) {
            eprintln!("vigie: could not remove the keychain item for {login}: {e}");
        }
        Ok(())
    })
    .await?
}

/// "Sign out" (docs/CONTRACT.md "Accounts"): the row-level action that keeps
/// the account and its repos — unlike `remove_account`, above, which deletes
/// both. Drops that account's keychain item and its in-memory token, so it
/// stops polling (repos show `last_error: "signed out"`) until it signs back
/// in via the ordinary device flow, which re-`add_account`s the same login.
#[tauri::command(rename_all = "snake_case")]
pub async fn sign_out_account(
    state: State<'_, EngineState>,
    login: String,
) -> Result<(), EngineError> {
    let engine = engine(&state);
    blocking(move || {
        engine.forget_account_token(&login);
        if let Err(e) = crate::keychain::delete_token_for(&login) {
            eprintln!("vigie: could not remove the keychain item for {login}: {e}");
        }
        Ok(())
    })
    .await?
}

/// Shared by `sign_out` and `clear_token`, which are the same underlying
/// operation under two contract-listed names (see `clear_token`'s doc).
fn clear_credential(engine: &Arc<dyn EngineApi>) -> Result<(), EngineError> {
    crate::keychain::clear_token().map_err(EngineError::storage)?;
    // Also forgets the signed-in login, which is what stops auto-watch.
    engine.set_token(None);
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub fn sign_out(state: State<EngineState>) -> Result<(), EngineError> {
    clear_credential(&state.0)
}

/// Kept per CONTRACT.md's Tauri commands list alongside `sign_out` — no
/// button is wired to it anymore now that there's no token field in the
/// app to attach a "Clear" action to.
#[tauri::command(rename_all = "snake_case")]
pub fn clear_token(state: State<EngineState>) -> Result<(), EngineError> {
    clear_credential(&state.0)
}

/// Hands the keychain's token to the engine and returns it, or `null` when
/// none is stored.
///
/// It does *not* verify the token: startup does that once, off the UI
/// thread, in `lib.rs` (see `install_stored_token`), so a cold launch does
/// not wait on a `/user` round trip before painting.
#[tauri::command(rename_all = "snake_case")]
pub async fn load_token(state: State<'_, EngineState>) -> Result<Option<String>, EngineError> {
    let engine = engine(&state);
    blocking(move || {
        let token = crate::keychain::load_token().map_err(EngineError::storage)?;
        if let Some(t) = &token {
            engine.set_token(Some(t.clone()));
        }
        Ok(token)
    })
    .await?
}

/// Who the engine thinks is signed in — the contract's `current_login`.
/// `null` before a token has been verified, and after signing out. Purely
/// in-memory: this never costs a request.
#[tauri::command(rename_all = "snake_case")]
pub async fn current_login(state: State<'_, EngineState>) -> Result<Option<String>, EngineError> {
    let engine = engine(&state);
    blocking(move || engine.current_login()).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn list_teams(state: State<'_, EngineState>) -> Result<Vec<Team>, EngineError> {
    let engine = engine(&state);
    blocking(move || engine.list_teams()).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn create_team(
    state: State<'_, EngineState>,
    name: String,
    logins: Vec<String>,
) -> Result<Team, EngineError> {
    let engine = engine(&state);
    blocking(move || engine.create_team(&name, &logins)).await?
}

#[tauri::command(rename_all = "snake_case")]
pub async fn update_team(
    state: State<'_, EngineState>,
    team: Team,
) -> Result<(), EngineError> {
    let engine = engine(&state);
    blocking(move || engine.update_team(team)).await?
}

#[tauri::command(rename_all = "snake_case")]
pub async fn delete_team(state: State<'_, EngineState>, team_id: u64) -> Result<(), EngineError> {
    let engine = engine(&state);
    blocking(move || engine.delete_team(team_id)).await?
}

#[tauri::command(rename_all = "snake_case")]
pub async fn reorder_teams(state: State<'_, EngineState>, ids: Vec<u64>) -> Result<(), EngineError> {
    let engine = engine(&state);
    blocking(move || engine.reorder_teams(&ids)).await?
}

#[tauri::command(rename_all = "snake_case")]
pub async fn import_org_team(
    state: State<'_, EngineState>,
    org: String,
    team_slug: String,
) -> Result<Team, EngineError> {
    let engine = engine(&state);
    blocking(move || engine.import_org_team(&org, &team_slug)).await?
}

#[tauri::command(rename_all = "snake_case")]
pub async fn suggest_people(
    state: State<'_, EngineState>,
    query: String,
    team_id: Option<u64>,
    limit: u32,
) -> Result<Vec<PersonSuggestion>, EngineError> {
    let engine = engine(&state);
    blocking(move || engine.suggest_people(&query, team_id, limit)).await?
}

/// `suggest_people`'s mirror image: `team_id`, when given, keeps only that
/// team's members instead of excluding them — for narrowing a search *to* a
/// team scope (Summary's person filter) rather than filling one in (Teams).
/// See docs/CONTRACT.md "People suggestions".
#[tauri::command(rename_all = "snake_case")]
pub async fn suggest_active_people(
    state: State<'_, EngineState>,
    query: String,
    team_id: Option<u64>,
    limit: u32,
) -> Result<Vec<PersonSuggestion>, EngineError> {
    let engine = engine(&state);
    blocking(move || engine.suggest_active_people(&query, team_id, limit)).await?
}

#[tauri::command(rename_all = "snake_case")]
pub async fn suggest_repos(
    state: State<'_, EngineState>,
) -> Result<Vec<RepoSuggestion>, EngineError> {
    let engine = engine(&state);
    blocking(move || engine.suggest_repos()).await?
}

#[tauri::command(rename_all = "snake_case")]
pub async fn add_repo(
    state: State<'_, EngineState>,
    spec: String,
    account_login: Option<String>,
) -> Result<Repo, EngineError> {
    let engine = engine(&state);
    blocking(move || engine.add_repo(&spec, account_login.as_deref())).await?
}

#[tauri::command(rename_all = "snake_case")]
pub async fn remove_repo(state: State<'_, EngineState>, repo_id: u64) -> Result<(), EngineError> {
    let engine = engine(&state);
    blocking(move || engine.remove_repo(repo_id)).await?
}

#[tauri::command(rename_all = "snake_case")]
pub async fn list_repos(state: State<'_, EngineState>) -> Result<Vec<Repo>, EngineError> {
    let engine = engine(&state);
    blocking(move || engine.list_repos()).await
}

/// The repos the Repos view keeps in its collapsed "Hidden" section — the one
/// place a hidden repo is still shown, because it is the page that unhides it.
#[tauri::command(rename_all = "snake_case")]
pub async fn list_hidden_repos(state: State<'_, EngineState>) -> Result<Vec<Repo>, EngineError> {
    let engine = engine(&state);
    blocking(move || engine.list_hidden_repos()).await
}

/// Hides or unhides one repo. Not a delete: `remove_repo` above is the
/// destructive one, and everything this repo has collected stays exactly where
/// it is while it is hidden.
#[tauri::command(rename_all = "snake_case")]
pub async fn set_repo_hidden(
    state: State<'_, EngineState>,
    repo_id: u64,
    hidden: bool,
) -> Result<(), EngineError> {
    let engine = engine(&state);
    blocking(move || engine.set_repo_hidden(repo_id, hidden)).await?
}

#[tauri::command(rename_all = "snake_case")]
pub async fn poll_now(
    app: AppHandle,
    state: State<'_, EngineState>,
) -> Result<PollResult, EngineError> {
    let engine = engine(&state);
    let result = poller::poll_and_notify(app.clone(), engine.clone()).await?;
    refresh_tray_badge(&app, &engine);
    Ok(result)
}

/// Loads the next older window of history — what the feed calls when the user
/// reaches the end of it. `span_secs` defaults to a week and is clamped by the
/// engine; omitting `repo_id` steps every repo back.
///
/// Deliberately not routed through `poller::poll_and_notify` the way `poll_now`
/// is, and deliberately not followed by a tray-badge refresh: backfilled events
/// arrive already seen, so there is no notification to raise and no badge to
/// change.
#[tauri::command(rename_all = "snake_case")]
pub async fn backfill(
    state: State<'_, EngineState>,
    repo_id: Option<u64>,
    span_secs: Option<i64>,
) -> Result<BackfillResult, EngineError> {
    let engine = engine(&state);
    let span = span_secs.unwrap_or(gitmon::BACKFILL_DEFAULT_SPAN_SECS);
    blocking(move || engine.backfill(repo_id, span)).await?
}

#[tauri::command(rename_all = "snake_case")]
#[allow(clippy::too_many_arguments)] // mirrors gitmon::Engine::list_events's five composable filters
pub async fn list_events(
    state: State<'_, EngineState>,
    repo_id: Option<u64>,
    actor: Option<String>,
    team_id: Option<u64>,
    mode: FilterMode,
    watched_only: bool,
    before_id: Option<u64>,
    limit: u32,
) -> Result<Vec<Event>, EngineError> {
    let engine = engine(&state);
    blocking(move || {
        engine.list_events(repo_id, actor.as_deref(), team_id, mode, watched_only, before_id, limit)
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn mark_seen(
    app: AppHandle,
    state: State<'_, EngineState>,
    ids: Vec<u64>,
) -> Result<(), EngineError> {
    let engine = engine(&state);
    let marker = engine.clone();
    blocking(move || marker.mark_seen(&ids)).await??;
    refresh_tray_badge(&app, &engine);
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn unseen_count(state: State<'_, EngineState>) -> Result<u64, EngineError> {
    let engine = engine(&state);
    blocking(move || engine.unseen_count()).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_thread(
    state: State<'_, EngineState>,
    repo_id: u64,
    number: u64,
) -> Result<Thread, EngineError> {
    let engine = engine(&state);
    blocking(move || engine.get_thread(repo_id, number)).await?
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_commit(
    state: State<'_, EngineState>,
    repo_id: u64,
    sha: String,
) -> Result<CommitDetail, EngineError> {
    let engine = engine(&state);
    blocking(move || engine.get_commit(repo_id, &sha)).await?
}

/// The detail pane's Files tab (CONTRACT.md "Reading in-app").
#[tauri::command(rename_all = "snake_case")]
pub async fn get_pull_files(
    state: State<'_, EngineState>,
    repo_id: u64,
    number: u64,
) -> Result<Vec<CommitFile>, EngineError> {
    let engine = engine(&state);
    blocking(move || engine.get_pull_files(repo_id, number)).await?
}

#[tauri::command(rename_all = "snake_case")]
pub async fn list_watches(state: State<'_, EngineState>) -> Result<Vec<Watch>, EngineError> {
    let engine = engine(&state);
    blocking(move || engine.list_watches()).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn watch_thread(
    state: State<'_, EngineState>,
    repo_id: u64,
    number: u64,
) -> Result<Watch, EngineError> {
    let engine = engine(&state);
    blocking(move || engine.watch_thread(repo_id, number)).await?
}

#[tauri::command(rename_all = "snake_case")]
pub async fn unwatch_thread(
    state: State<'_, EngineState>,
    repo_id: u64,
    number: u64,
) -> Result<(), EngineError> {
    let engine = engine(&state);
    blocking(move || engine.unwatch_thread(repo_id, number)).await?
}

#[tauri::command(rename_all = "snake_case")]
pub async fn clear_closed_watches(state: State<'_, EngineState>) -> Result<u64, EngineError> {
    let engine = engine(&state);
    blocking(move || engine.clear_closed_watches()).await
}

/// A period summary of stored events (CONTRACT.md "Digests"). The Summary
/// view resolves its period control (Today/This week/This month/Custom) to
/// unix seconds in local time before calling this; the engine only
/// aggregates what's already stored, never calendar math.
///
/// `tz_offset_secs` is the webview's own UTC offset — the app already knows
/// it, having just used it to resolve the window — and the engine uses it for
/// nothing but deciding where a local day starts in `series` and `hours`.
#[tauri::command(rename_all = "snake_case")]
pub async fn digest(
    state: State<'_, EngineState>,
    start: i64,
    end: i64,
    team_id: Option<u64>,
    actor: Option<String>,
    tz_offset_secs: i32,
) -> Result<Digest, EngineError> {
    let engine = engine(&state);
    blocking(move || engine.digest(start, end, team_id, actor.as_deref(), tz_offset_secs))
        .await?
}

/// Every pull request open right now, oldest first (CONTRACT.md "Pulls").
/// Filled in by the poller from the PR listing it already fetches, so this
/// reads the store and never GitHub.
#[tauri::command(rename_all = "snake_case")]
pub async fn open_pulls(
    state: State<'_, EngineState>,
    team_id: Option<u64>,
    actor: Option<String>,
    reviewer: Option<String>,
) -> Result<Vec<OpenPull>, EngineError> {
    let engine = engine(&state);
    blocking(move || engine.open_pulls(team_id, actor.as_deref(), reviewer.as_deref())).await?
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_settings(state: State<'_, EngineState>) -> Result<Settings, EngineError> {
    let engine = engine(&state);
    blocking(move || engine.get_settings()).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn set_settings(
    state: State<'_, EngineState>,
    settings: Settings,
) -> Result<(), EngineError> {
    let engine = engine(&state);
    blocking(move || engine.set_settings(settings)).await?
}

#[cfg(test)]
mod tests {
    use super::{is_passkey_url, Url};

    fn u(s: &str) -> Url {
        s.parse().expect("test url")
    }

    #[test]
    fn passkey_and_webauthn_pages_are_handed_off() {
        assert!(is_passkey_url(&u("https://github.com/sessions/passkey")));
        assert!(is_passkey_url(&u("https://github.com/sessions/two-factor/webauthn")));
        // Case and query position must not matter.
        assert!(is_passkey_url(&u("https://github.com/login?webauthn=1")));
        assert!(is_passkey_url(&u("https://github.com/sessions/PassKey")));
    }

    #[test]
    fn the_ordinary_device_flow_pages_are_not() {
        assert!(!is_passkey_url(&u("https://github.com/login/device?user_code=ABCD-1234")));
        assert!(!is_passkey_url(&u("https://github.com/login")));
        assert!(!is_passkey_url(&u("https://github.com/login/oauth/authorize")));
    }

    /// The hand-off opens a URL in the user's browser, so it must never fire
    /// for a host that only looks like GitHub.
    #[test]
    fn only_github_hosts_count() {
        assert!(!is_passkey_url(&u("https://evil.example/github.com/sessions/passkey")));
        assert!(!is_passkey_url(&u("https://github.com.evil.example/sessions/passkey")));
        assert!(is_passkey_url(&u("https://gist.github.com/sessions/webauthn")));
    }
}
