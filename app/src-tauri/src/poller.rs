//! Background poll timer: a tokio interval that calls `poll_now` on a
//! blocking task, emits `new-events` to the webview, and raises OS
//! notifications per the contract's rules (notify_kinds, enabled flag,
//! quiet hours, watched-always-notifies, >10 collapse) and its
//! notification-text format (see `notify_one`).
//!
//! Nothing in here ever calls the engine from the async task itself: every
//! engine call goes through `commands::blocking`, so a slow GitHub request
//! cannot stall the runtime the webview's IPC also runs on.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use chrono::Timelike;
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tauri_plugin_notification::NotificationExt;

use crate::commands::blocking;
use crate::engine_api::EngineApi;
use gitmon::types::{Event, EventKind, PollResult, QuietHours, RepoError, Settings};
use gitmon::{EngineError, ErrorKind};

/// The webview event carrying a poll's outcome. Payload: [`PollResult`].
pub const NEW_EVENTS_EVENT: &str = "new-events";
/// The webview event that raises the rate-limit banner. Payload:
/// [`RateLimitNotice`].
pub const RATE_LIMITED_EVENT: &str = "poll-rate-limited";

/// What the UI needs to draw the rate-limit banner (CONTRACT.md's
/// `rate_limited` error kind). Deliberately *not* an OS notification: being
/// throttled is a state of the app, not an event worth interrupting for.
///
/// `reset_at` now comes through on both paths: `RepoError` carries the
/// `reset_at` of the `EngineError` that produced it, so a limit hit by the
/// background poller says when it lifts just as one hit by a command
/// (`add_repo`, `get_thread`, …) does. It stays `None` only when GitHub sent
/// no `x-ratelimit-reset` header, and the banner then falls back to its
/// timeless wording.
#[derive(Debug, Clone, Serialize)]
pub struct RateLimitNotice {
    pub message: String,
    pub reset_at: Option<i64>,
}

fn local_minute_of_day() -> u16 {
    let now = chrono::Local::now();
    (now.hour() * 60 + now.minute()) as u16
}

/// Whether `minute` (minutes after local midnight) is inside `window`.
/// Handles wraparound, e.g. 22:00–08:00.
fn quiet_hours_contain(window: &QuietHours, minute: u16) -> bool {
    if window.start_minute <= window.end_minute {
        minute >= window.start_minute && minute < window.end_minute
    } else {
        minute >= window.start_minute || minute < window.end_minute
    }
}

fn in_quiet_hours(qh: &Option<QuietHours>) -> bool {
    match qh {
        Some(window) => quiet_hours_contain(window, local_minute_of_day()),
        None => false,
    }
}

/// The verb CONTRACT.md's notification-text paragraph uses in the title for
/// every kind except `commit`, which gets its own "pushed to" phrasing.
fn verb_for_kind(kind: EventKind) -> &'static str {
    match kind {
        EventKind::Commit => "pushed",
        EventKind::PrOpened | EventKind::IssueOpened => "opened",
        EventKind::PrMerged => "merged",
        EventKind::PrClosed => "closed",
        EventKind::PrReviewed => "reviewed",
        EventKind::PrCommented | EventKind::IssueCommented => "commented on",
    }
}

/// The short (7-char) sha off the end of a commit event's `url`
/// (".../commit/<sha>"), for the notification body's "first message line
/// and short sha" — the full sha isn't carried on `Event` itself.
fn short_sha(url: &str) -> &str {
    let full = url.rsplit('/').next().unwrap_or("");
    &full[..full.len().min(7)]
}

/// Whether this event raises a notification at all, given the settings.
///
/// An event on a watched thread always does, whatever `notify_kinds` says —
/// CONTRACT.md "Watched threads", Notifications. The enabled flag and quiet
/// hours are checked once for the whole poll, not here.
fn should_notify(event: &Event, notify_kinds: &HashSet<EventKind>) -> bool {
    event.watched || notify_kinds.contains(&event.kind)
}

/// One event's OS notification, per CONTRACT.md's "Tauri commands" closing
/// paragraph: title `<actor> <verb> on <owner/name> #<number>` (commits:
/// `<actor> pushed to <owner/name>`), prefixed `Watched:` for an event on a
/// watched thread; body the PR/issue title (commits: the first message line
/// and short sha) — never a snippet of the event's body text, which reads as
/// truncated prose on a glance-length surface like a lock screen.
fn notify_one(app: &AppHandle, event: &Event, repo_name: &str) {
    let subject = if event.kind == EventKind::Commit {
        format!("{} pushed to {}", event.actor_login, repo_name)
    } else {
        format!(
            "{} {} on {} #{}",
            event.actor_login,
            verb_for_kind(event.kind),
            repo_name,
            event.number.unwrap_or_default()
        )
    };
    let title = if event.watched { format!("Watched: {subject}") } else { subject };
    let body = if event.kind == EventKind::Commit {
        format!("{} {}", event.title, short_sha(&event.url))
    } else {
        event.title.clone()
    };
    let _ = app.notification().builder().title(title).body(body).show();
}

fn notify_summary(app: &AppHandle, count: usize) {
    let _ = app.notification().builder().title("Vigie").body(format!("{count} new events")).show();
}

/// The `rate_limited` repo failures in a poll, if any.
///
/// `RepoError.message` is the engine error's `Display`, which is
/// `"<kind>: <message>"` — the kind is what identifies one, so that prefix is
/// what is matched on and then stripped back off for the user. `reset_at`
/// rides along on the same `RepoError` and is handed to the banner as-is.
fn rate_limit_notice(errors: &[RepoError]) -> Option<RateLimitNotice> {
    let prefix = format!("{}: ", ErrorKind::RateLimited.as_str());
    let hit = errors.iter().find(|e| e.message.starts_with(&prefix))?;
    Some(RateLimitNotice {
        message: hit.message[prefix.len()..].to_string(),
        reset_at: hit.reset_at,
    })
}

/// Emits a poll's outcome to the webview and raises whatever notifications
/// the settings allow. Pure app-side work — no engine calls except the
/// settings and repo-name lookups the caller has already made.
fn announce(app: &AppHandle, result: &PollResult, settings: &Settings, repos: &HashMap<u64, String>) {
    if !result.new_events.is_empty() || !result.errors.is_empty() {
        let _ = app.emit(NEW_EVENTS_EVENT, result);
    }
    // A banner, never a notification: see `RateLimitNotice`.
    if let Some(notice) = rate_limit_notice(&result.errors) {
        let _ = app.emit(RATE_LIMITED_EVENT, notice);
    }

    if result.new_events.is_empty() {
        return;
    }
    if !settings.notifications_enabled || in_quiet_hours(&settings.quiet_hours) {
        return;
    }

    let notify_kinds: HashSet<EventKind> = settings.notify_kinds.iter().copied().collect();
    let matching: Vec<&Event> =
        result.new_events.iter().filter(|e| should_notify(e, &notify_kinds)).collect();
    if matching.len() > 10 {
        notify_summary(app, matching.len());
    } else {
        for e in &matching {
            let repo_name = repos.get(&e.repo_id).map(String::as_str).unwrap_or("unknown/repo");
            notify_one(app, e, repo_name);
        }
    }
}

/// Runs one poll cycle off the async thread: fetch, emit, and (maybe)
/// notify. Shared by the timer below and `commands::poll_now`, so a
/// hand-triggered poll behaves exactly like a scheduled one.
///
/// The three engine calls ride on one blocking task rather than three: they
/// all contend for the same store mutex, so splitting them would only add
/// hand-offs between waits.
pub async fn poll_and_notify(
    app: AppHandle,
    engine: Arc<dyn EngineApi>,
) -> Result<PollResult, EngineError> {
    let (result, settings, repos) = blocking(move || {
        let result = engine.poll_now();
        let settings = engine.get_settings();
        let repos: HashMap<u64, String> = engine
            .list_repos()
            .into_iter()
            .map(|r| (r.id, format!("{}/{}", r.owner, r.name)))
            .collect();
        (result, settings, repos)
    })
    .await?;

    announce(&app, &result, &settings, &repos);
    Ok(result)
}

pub fn spawn(app: AppHandle, engine: Arc<dyn EngineApi>) {
    tauri::async_runtime::spawn(async move {
        loop {
            // Re-read the interval each cycle so a settings change (poll
            // interval is user-editable) takes effect on the next tick
            // without restarting the timer.
            let settings_engine = engine.clone();
            let interval_secs = blocking(move || settings_engine.get_settings().poll_interval_secs)
                .await
                .unwrap_or(120)
                .max(gitmon::MIN_POLL_INTERVAL_SECS) as u64;
            tokio::time::sleep(Duration::from_secs(interval_secs)).await;

            // A poll that fails is one lost cycle, not a dead timer: an
            // engine with no credential yet just returns empty results, and
            // the loop keeps ticking until a sign-in gives it one.
            if let Err(e) = poll_and_notify(app.clone(), engine.clone()).await {
                eprintln!("vigie: scheduled poll failed: {e}");
            }
            crate::tray::refresh_tray_badge(&app, &engine);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: EventKind, watched: bool) -> Event {
        Event {
            id: 1,
            repo_id: 1,
            kind,
            actor_login: "priya".to_string(),
            actor_avatar_url: None,
            title: "Add retry budget".to_string(),
            body_preview: None,
            body: None,
            url: "https://github.com/acme/platform/pull/412".to_string(),
            number: Some(412),
            occurred_at: 0,
            seen: false,
            team_ids: vec![],
            watched,
        }
    }

    /// The contract's "a new event with `watched: true` always raises a
    /// notification, whatever `notify_kinds` says".
    #[test]
    fn a_watched_event_notifies_even_when_its_kind_is_switched_off() {
        let only_commits: HashSet<EventKind> = [EventKind::Commit].into_iter().collect();
        assert!(!should_notify(&event(EventKind::PrOpened, false), &only_commits));
        assert!(should_notify(&event(EventKind::PrOpened, true), &only_commits));
        assert!(should_notify(&event(EventKind::Commit, false), &only_commits));
    }

    /// Only a `rate_limited` repo failure raises the banner, the kind prefix
    /// is stripped back off the message the user reads, and the repo error's
    /// `reset_at` reaches the banner so it can name the time polling resumes.
    #[test]
    fn only_rate_limit_errors_raise_the_banner() {
        let unrelated = [RepoError {
            repo_id: 1,
            message: "network: could not reach github".to_string(),
            reset_at: None,
        }];
        assert!(rate_limit_notice(&unrelated).is_none());

        let limited = [RepoError {
            repo_id: 1,
            message: "rate_limited: API rate limit exceeded".to_string(),
            reset_at: Some(1_760_003_600),
        }];
        let notice = rate_limit_notice(&limited).expect("banner");
        assert_eq!(notice.message, "API rate limit exceeded");
        assert_eq!(notice.reset_at, Some(1_760_003_600));

        // No `x-ratelimit-reset` header: the banner falls back to its
        // timeless wording rather than inventing a time.
        let headerless = [RepoError {
            repo_id: 1,
            message: "rate_limited: API rate limit exceeded".to_string(),
            reset_at: None,
        }];
        assert_eq!(rate_limit_notice(&headerless).expect("banner").reset_at, None);
    }

    #[test]
    fn quiet_hours_wrap_past_midnight() {
        let overnight = QuietHours { start_minute: 22 * 60, end_minute: 8 * 60 };
        assert!(quiet_hours_contain(&overnight, 23 * 60));
        assert!(quiet_hours_contain(&overnight, 2 * 60));
        assert!(!quiet_hours_contain(&overnight, 12 * 60));

        let daytime = QuietHours { start_minute: 9 * 60, end_minute: 17 * 60 };
        assert!(quiet_hours_contain(&daytime, 12 * 60));
        assert!(!quiet_hours_contain(&daytime, 8 * 60));
        // Half-open, like the engine's other windows: the end minute is out.
        assert!(!quiet_hours_contain(&daytime, 17 * 60));
    }
}
