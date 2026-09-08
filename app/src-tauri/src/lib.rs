//! App entry point: wires up the real `gitmon` engine, Tauri commands, the
//! tray icon, the background poller, and the "close hides, Cmd+Q quits"
//! window behavior described in the packet's UI section.

mod commands;
mod engine_api;
mod keychain;
#[cfg(feature = "mock")]
mod mock_engine;
mod poller;
mod tray;

use std::sync::Arc;

use tauri::{Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewWindow, WindowEvent};

use engine_api::EngineApi;

/// Shared handle to the engine implementation. An `Arc<dyn EngineApi>` (not
/// `Box`) because both Tauri commands and the background poller task need
/// their own owned reference to the same instance.
pub struct EngineState(pub Arc<dyn EngineApi>);

/// The GitHub OAuth App's client ID (device flow, no secret), baked in at
/// build time — see docs/CONTRACT.md "Sign-in". `REPLACE_ME` at runtime
/// means the build didn't get `VIGIE_GITHUB_CLIENT_ID`; `commands::
/// start_device_login` and `commands::github_client_id_ready` both check
/// for that literal rather than talking to GitHub with a bogus id. Never
/// hardcode a real client ID here — this is a committed file.
pub const GITHUB_CLIENT_ID: &str = match option_env!("VIGIE_GITHUB_CLIENT_ID") {
    Some(id) => id,
    None => "REPLACE_ME",
};

/// Set to `1` to run against the in-process mock instead of GitHub. Only a
/// build with `--features mock` has a mock to switch to; in a release build
/// this is read and ignored, so a stray environment variable can never make
/// the shipped app serve fabricated data.
const MOCK_ENV_VAR: &str = "GITMON_MOCK";

fn mock_requested() -> bool {
    std::env::var(MOCK_ENV_VAR).map(|v| v == "1").unwrap_or(false)
}

/// The engine this process runs on: the real one, opened on
/// `app_data_dir/gitmon.db`, unless a `mock`-feature build was asked for the
/// mock.
fn build_engine(data_dir: &std::path::Path) -> Arc<dyn EngineApi> {
    #[cfg(feature = "mock")]
    if mock_requested() {
        eprintln!("vigie: {MOCK_ENV_VAR}=1 — running on the in-process mock engine");
        return Arc::new(mock_engine::MockEngine::new());
    }
    #[cfg(not(feature = "mock"))]
    if mock_requested() {
        eprintln!("vigie: {MOCK_ENV_VAR}=1 ignored — this build has no mock engine compiled in");
    }

    let engine = gitmon::Engine::open(data_dir)
        .unwrap_or_else(|e| panic!("could not open the Vigie database in {data_dir:?}: {e}"));
    Arc::new(engine)
}

/// Emitted to the webview when the stored credential turns out to be dead,
/// so the app falls back to the sign-in screen instead of showing a feed it
/// can no longer refresh. Only ever fires out of the legacy migration below
/// now — a per-account token is deliberately never re-verified at startup,
/// so a dead one just shows up as that account's repos going "signed out"
/// (docs/CONTRACT.md "Accounts").
pub const SIGNED_OUT_EVENT: &str = "signed-out";

/// What migrating the legacy single-token keychain item did, so the caller
/// (which has the `AppHandle` this pure function deliberately doesn't take)
/// can decide whether to tell the webview.
enum LegacyMigration {
    /// No legacy item was there to migrate — the common case for every
    /// install past the first one.
    None,
    /// Migrated into an account; the legacy item is gone.
    Migrated,
    /// GitHub rejected the legacy token outright: there is nothing left to
    /// keep it for, so the item is removed too. With no accounts and no
    /// legacy item left, this *is* the signed-out state the old single-token
    /// startup gave a dead credential.
    DeadCredential,
    /// Something else went wrong (offline, an unreadable keychain, …). The
    /// legacy item is left in place so the next launch tries again.
    TransientFailure,
}

/// One-time migration: the pre-v5 keychain item, if present, becomes the
/// first account (docs/CONTRACT.md "Accounts" — "Legacy"). Pure engine +
/// keychain logic with no `AppHandle`, so it is directly testable — see the
/// `mock`-feature tests below.
fn migrate_legacy_token(engine: &Arc<dyn EngineApi>) -> LegacyMigration {
    let token = match keychain::load_token() {
        Ok(Some(token)) => token,
        Ok(None) => return LegacyMigration::None,
        Err(e) => {
            eprintln!("vigie: could not read the legacy keychain item: {e}");
            return LegacyMigration::TransientFailure;
        }
    };
    match engine.add_account(&token) {
        Ok(account) => match keychain::store_token_for(&account.login, &token) {
            Ok(()) => {
                // Clear the legacy item only now that the token safely lives
                // under the new account's own item — clearing it first would
                // lose the only stored copy if this write had failed (a
                // transient keychain error, say).
                if let Err(e) = keychain::clear_token() {
                    eprintln!("vigie: could not remove the legacy keychain item: {e}");
                }
                LegacyMigration::Migrated
            }
            Err(e) => {
                // The legacy item is left in place (its token still matches
                // what `add_account` just verified) so the next launch
                // retries the write; `add_account` is an upsert, so retrying
                // it then is safe and creates no duplicate account.
                eprintln!(
                    "vigie: could not save the migrated token under {}: {e}; leaving the legacy item in place to retry next launch",
                    account.login
                );
                LegacyMigration::TransientFailure
            }
        },
        Err(e) if e.kind == gitmon::ErrorKind::Auth => {
            eprintln!("vigie: the legacy GitHub credential was rejected; dropping it");
            if let Err(e) = keychain::clear_token() {
                eprintln!("vigie: could not remove the dead legacy keychain item: {e}");
            }
            LegacyMigration::DeadCredential
        }
        Err(e) => {
            eprintln!(
                "vigie: could not migrate the legacy GitHub credential ({}); will retry next launch",
                e.kind.as_str()
            );
            LegacyMigration::TransientFailure
        }
    }
}

/// Loads every account's own keychain item and hands it to the engine.
///
/// Deliberately unverified — see `EngineApi::set_account_token` — so startup
/// never spends a `/user` request per account; a missing or dead item only
/// ever shows up later as that account's repos going "signed out", never as
/// a startup failure. A missing item is logged, not panicked on.
fn install_account_tokens(engine: &Arc<dyn EngineApi>) {
    for account in engine.list_accounts() {
        match keychain::load_token_for(&account.login) {
            Ok(Some(token)) => engine.set_account_token(&account.login, &token),
            Ok(None) => eprintln!(
                "vigie: no keychain item for {}; its repos will show \"signed out\" until it signs in again",
                account.login
            ),
            Err(e) => {
                eprintln!("vigie: could not read the keychain item for {}: {e}", account.login)
            }
        }
    }
}

/// Hands the keychain's tokens to the engine at startup (docs/CONTRACT.md
/// "Accounts"): migrates the legacy single-token item into an account if
/// one is still there, then installs every account's own token.
///
/// Runs on a blocking task: nothing waits on it, and the poller idles
/// harmlessly until it lands.
fn install_stored_token(app: tauri::AppHandle, engine: Arc<dyn EngineApi>) {
    tauri::async_runtime::spawn_blocking(move || {
        if let LegacyMigration::DeadCredential = migrate_legacy_token(&engine) {
            let _ = app.emit(SIGNED_OUT_EVENT, ());
        }
        install_account_tokens(&engine);
    });
}

/// Shrinks and repositions `window` so it's fully within the work area of
/// whichever monitor it's about to appear on — called from `run()`'s
/// `setup()`, right before the window is shown, on both a first launch
/// (tauri.conf.json's plain 1440x900 default) and a relaunch (whatever
/// tauri-plugin-window-state's `on_window_ready` already restored). Neither
/// of those is monitor-aware on its own: a config default is just a fixed
/// request with no idea how big any given screen is, and the plugin's own
/// restore only skips *position* when no monitor intersects the saved
/// point — it still applies a saved *size* unconditionally, so a window
/// remembered at a large size on a display that's since been swapped for a
/// smaller one (or unplugged) would otherwise reopen oversized or straddling
/// the new screen's edge. A missing monitor (`current_monitor`/
/// `primary_monitor` both empty — no display info available at all) leaves
/// the window untouched rather than guessing.
fn clamp_to_monitor<R: tauri::Runtime>(window: &WebviewWindow<R>) {
    let Some(monitor) = window.current_monitor().ok().flatten().or_else(|| window.primary_monitor().ok().flatten()) else {
        return;
    };
    let work_area = monitor.work_area();
    let (area_x, area_y) = (work_area.position.x, work_area.position.y);
    let (area_w, area_h) = (work_area.size.width as i32, work_area.size.height as i32);

    let Ok(size) = window.outer_size() else { return };
    let width = (size.width as i32).min(area_w).max(1);
    let height = (size.height as i32).min(area_h).max(1);
    if width != size.width as i32 || height != size.height as i32 {
        let _ = window.set_size(PhysicalSize::new(width as u32, height as u32));
    }

    let Ok(pos) = window.outer_position() else { return };
    // Clamped against the resized (not original) width/height, so the
    // window's far edge lands on the work area's edge rather than past it.
    let x = pos.x.max(area_x).min(area_x + area_w - width);
    let y = pos.y.max(area_y).min(area_y + area_h - height);
    if x != pos.x || y != pos.y {
        let _ = window.set_position(PhysicalPosition::new(x, y));
    }
}

/// `migrate_legacy_token`/`install_account_tokens` take no `AppHandle`
/// precisely so they can be driven directly here, against the in-process
/// mock engine and the keychain's own mock backend (`--features mock`) —
/// no real window, no real keychain, per the packet's boundary on both.
#[cfg(all(test, feature = "mock"))]
mod tests {
    use super::*;
    use crate::mock_engine::MockEngine;

    /// The pre-v5 keychain item's account name (docs/CONTRACT.md
    /// "Accounts" — "Legacy"). Kept as a literal here rather than exposed
    /// from `keychain` just for tests: it is a stable, documented contract
    /// value, not an implementation detail.
    const LEGACY_ACCOUNT: &str = "github-token";

    /// Migration: a legacy item present at startup becomes an account, and
    /// the legacy item is gone afterward — the packet's required coverage.
    #[test]
    fn migration_path_adds_the_account_and_removes_the_legacy_item() {
        let _guard = keychain::lock_for_test();
        keychain::store_token_for(LEGACY_ACCOUNT, "legacy-tok").unwrap();

        let engine: Arc<dyn EngineApi> = Arc::new(MockEngine::new());
        let outcome = migrate_legacy_token(&engine);
        assert!(matches!(outcome, LegacyMigration::Migrated));

        let accounts = engine.list_accounts();
        assert_eq!(accounts.len(), 1, "accounts: {accounts:?}");
        let login = accounts[0].login.clone();

        assert_eq!(keychain::load_token().unwrap(), None, "the legacy item must be gone");
        assert_eq!(
            keychain::load_token_for(&login).unwrap(),
            Some("legacy-tok".to_string()),
            "the token moves to the new account's own item"
        );

        keychain::delete_token_for(&login).unwrap();
    }

    /// A migrated token that fails to save under the new account's own item
    /// (a transient keychain error) must not lose the legacy item — it is
    /// the only stored copy of the token until the new item exists. The
    /// account is still registered on the engine (in memory) either way:
    /// retrying next launch is safe because `add_account` upserts by login.
    #[test]
    fn a_failed_migration_write_leaves_the_legacy_item_in_place() {
        let _guard = keychain::lock_for_test();
        keychain::store_token_for(LEGACY_ACCOUNT, "legacy-tok").unwrap();
        keychain::delete_token_for("legacy-tok").unwrap();
        // The mock derives the new account's login from the token itself
        // (`mock_login_for_token`): for a token with no "gho_mock" prefix,
        // that's just the token lowercased — "legacy-tok" here — so this is
        // the account whose write needs to fail.
        keychain::fail_next_write_for("legacy-tok");

        let engine: Arc<dyn EngineApi> = Arc::new(MockEngine::new());
        let outcome = migrate_legacy_token(&engine);
        assert!(matches!(outcome, LegacyMigration::TransientFailure));

        assert_eq!(
            keychain::load_token().unwrap(),
            Some("legacy-tok".to_string()),
            "the legacy item must survive a failed write to the new item"
        );
        assert_eq!(keychain::load_token_for("legacy-tok").unwrap(), None, "the failed write must not have landed");
        let accounts = engine.list_accounts();
        assert_eq!(accounts.len(), 1, "accounts: {accounts:?}");

        keychain::delete_token_for("legacy-tok").unwrap();
        keychain::delete_token_for(LEGACY_ACCOUNT).unwrap();
    }

    /// No legacy item at all — every install past the first one — is a
    /// no-op, not an error, and adds no account.
    #[test]
    fn migration_is_a_no_op_with_no_legacy_item() {
        let _guard = keychain::lock_for_test();
        keychain::delete_token_for(LEGACY_ACCOUNT).unwrap();

        let engine: Arc<dyn EngineApi> = Arc::new(MockEngine::new());
        assert!(matches!(migrate_legacy_token(&engine), LegacyMigration::None));
        assert!(engine.list_accounts().is_empty());
    }

    /// A legacy token GitHub rejects is dropped, not migrated: no account is
    /// created, and the dead item is removed rather than kept around.
    #[test]
    fn a_dead_legacy_token_is_dropped_not_migrated() {
        let _guard = keychain::lock_for_test();
        // The mock's sentinel for "GitHub rejected this token" — see
        // `mock_engine::MOCK_REJECTED_TOKEN`.
        keychain::store_token_for(LEGACY_ACCOUNT, "revoked").unwrap();

        let engine: Arc<dyn EngineApi> = Arc::new(MockEngine::new());
        let outcome = migrate_legacy_token(&engine);
        assert!(matches!(outcome, LegacyMigration::DeadCredential));

        assert!(engine.list_accounts().is_empty());
        assert_eq!(keychain::load_token().unwrap(), None, "the dead item must be gone too");
    }

    /// Startup with two accounts installs both tokens — the packet's
    /// required coverage. Each account's own keychain item is read and
    /// handed to the engine via `set_account_token`.
    #[test]
    fn startup_installs_every_accounts_token() {
        let _guard = keychain::lock_for_test();
        let mock = Arc::new(MockEngine::new());
        let alice = mock.add_account("alice").unwrap();
        let bob = mock.add_account("bob").unwrap();
        keychain::store_token_for(&alice.login, "tok-alice").unwrap();
        keychain::store_token_for(&bob.login, "tok-bob").unwrap();

        // Coerced to the trait object `lib.rs` actually runs against; the
        // `Arc` clone shares the same underlying mock, so `mock`'s own
        // (test-only) accessor still sees what this call wrote.
        let engine: Arc<dyn EngineApi> = mock.clone();
        install_account_tokens(&engine);

        assert_eq!(mock.installed_account_token(&alice.login), Some("tok-alice".to_string()));
        assert_eq!(mock.installed_account_token(&bob.login), Some("tok-bob".to_string()));

        keychain::delete_token_for(&alice.login).unwrap();
        keychain::delete_token_for(&bob.login).unwrap();
    }

    /// An account with no keychain item is logged and skipped, never a
    /// panic — its repos simply show "signed out" later.
    #[test]
    fn startup_tolerates_an_account_with_no_keychain_item() {
        let _guard = keychain::lock_for_test();
        let mock = Arc::new(MockEngine::new());
        let alice = mock.add_account("alice").unwrap();
        keychain::delete_token_for(&alice.login).unwrap();

        let engine: Arc<dyn EngineApi> = mock.clone();
        install_account_tokens(&engine); // must not panic
        assert_eq!(mock.installed_account_token(&alice.login), None);
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        // Checks site/latest.json (GitHub Pages, endpoint in tauri.conf.json)
        // for a newer signed build. Registering the plugin is all the Rust
        // side needs: `check()`/`downloadAndInstall()` are its own IPC
        // commands, called straight from the frontend (Settings.svelte /
        // UpdatePrompt.svelte) — nothing here decides when that happens.
        // `tauri-plugin-process` is the separate official plugin
        // `relaunch()` comes from, for installing on the user's say-so.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        // Remembers the main window's size and position across launches
        // (packet gm-window-r1) — restored in that window's own
        // `on_window_ready`, which runs during window creation, before
        // `setup()` below gets a chance to run `clamp_to_monitor`.
        // "tray-popover" is denylisted deliberately: it's repositioned
        // under the menu bar icon every time it opens (tray.rs), so
        // there's never a saved geometry worth restoring for it, and
        // tracking it would just mean writing to disk on every popover
        // open/close for no benefit.
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_denylist(&["tray-popover"])
                .build(),
        )
        .invoke_handler(tauri::generate_handler![
            commands::github_client_id_ready,
            commands::start_device_login,
            commands::poll_device_login,
            commands::open_github_signin_window,
            commands::open_signin_in_browser,
            commands::sign_out,
            commands::load_token,
            commands::clear_token,
            commands::current_login,
            commands::list_accounts,
            commands::add_account_token,
            commands::remove_account,
            commands::sign_out_account,
            commands::list_teams,
            commands::create_team,
            commands::update_team,
            commands::delete_team,
            commands::reorder_teams,
            commands::import_org_team,
            commands::suggest_people,
            commands::suggest_active_people,
            commands::suggest_repos,
            commands::add_repo,
            commands::remove_repo,
            commands::list_repos,
            commands::list_hidden_repos,
            commands::set_repo_hidden,
            commands::poll_now,
            commands::backfill,
            commands::list_events,
            commands::mark_seen,
            commands::unseen_count,
            commands::get_thread,
            commands::get_commit,
            commands::get_pull_files,
            commands::list_watches,
            commands::watch_thread,
            commands::unwatch_thread,
            commands::clear_closed_watches,
            commands::digest,
            commands::open_pulls,
            commands::get_settings,
            commands::set_settings,
        ])
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(move |app| {
            // The engine's database lives beside the app's own data, which
            // only exists once there is an app to ask — hence here rather
            // than before the builder.
            let data_dir = app.path().app_data_dir()?;
            let engine = build_engine(&data_dir);
            app.manage(EngineState(engine.clone()));

            tray::setup(app)?;
            // The window otherwise opens behind whatever app was active at
            // launch; bring it to the front the way a normal app launch would.
            if let Some(main) = app.get_webview_window("main") {
                // By the time `setup()` runs, `main` already has whatever
                // size/position tauri-plugin-window-state's `on_window_ready`
                // restored (that hook runs during window creation, earlier
                // in `Builder::build()` than this closure) — or, on a first
                // launch, tauri.conf.json's plain 1440x900 default. Neither
                // of those accounts for the monitor actually in front of the
                // user right now: the config default doesn't know how big
                // any given screen is, and the plugin restores a saved size
                // unconditionally (only its position restore skips itself
                // when no monitor intersects the saved point). A user on a
                // smaller display than last time, or than 1440x900 on a
                // first run, would otherwise get a window that opens too
                // big for the screen or straddling its edge.
                clamp_to_monitor(&main);
                let _ = main.show();
                let _ = main.set_focus();
            }
            // Token first, then the timer: the poller's first tick is a
            // whole interval away, so the credential is in place long
            // before it fires, and with no credential it simply polls
            // nothing until a sign-in provides one.
            install_stored_token(app.handle().clone(), engine.clone());
            poller::spawn(app.handle().clone(), engine);
            Ok(())
        })
        .on_window_event(|window, event| {
            match event {
                // Closing the main window (red traffic light / Cmd+W) hides
                // it instead of quitting — the app stays running in the
                // tray. Cmd+Q / the tray's Quit item calls `app.exit()`
                // directly (see tray.rs), bypassing this handler, so it
                // quits for real.
                WindowEvent::CloseRequested { api, .. } if window.label() == "main" => {
                    api.prevent_close();
                    let _ = window.hide();
                }
                // The tray popover behaves like a menu: it disappears the
                // moment it loses focus to one of our own other windows
                // (the main window coming forward) rather than sitting
                // around as a window. A click landing outside this app
                // entirely (another app, the desktop) is covered instead by
                // tray.rs's `CLICK_OUTSIDE_MONITOR` — see
                // `tray::install_click_outside_monitor`'s doc comment for
                // why this event alone isn't reliable enough to cover that
                // case too. Escape is handled separately, in JS
                // (TrayPopover.svelte) — pressing it doesn't move
                // key-window focus on its own, so this handler alone
                // wouldn't catch it.
                //
                // `note_external_hide` records *when* this happened so
                // tray.rs's own click handler can tell "the click that
                // opened the tray icon's context also just took focus
                // away from the popover, and this handler's hide() raced
                // ahead of that click handler" apart from "the user
                // clicked the tray icon well after the popover had
                // already closed" — see `tray::PopoverFocusState`.
                WindowEvent::Focused(false) if window.label() == "tray-popover" => {
                    let _ = window.hide();
                    tray::remove_click_outside_monitor();
                    window.state::<tray::PopoverFocusState>().note_external_hide();
                }
                _ => {}
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
