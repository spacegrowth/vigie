//! `backfill`: the cursor, the window it fetches, and what it refuses to do.
//!
//! Every test here drives the same shape: poll a quiet repo so the engine
//! records how far back it reached, then swap the mock for one serving commits
//! at timestamps chosen *relative to that floor*, so the window under test is
//! exact rather than approximate.

mod common;

use common::{
    engine_for, iso8601, mock_get, mount, mount_logging_since, recorded, server, since_log,
    unmount, RepoMock, Resp,
};
use gitmon::{
    Engine, ErrorKind, FilterMode, Settings, BACKFILL_MAX_SPAN_SECS, BACKFILL_MIN_SPAN_SECS,
    FIRST_POLL_LOOKBACK_SECS,
};

const HOUR: i64 = 3600;
const DAY: i64 = 24 * HOUR;

fn filter_all(engine: &Engine) {
    engine
        .set_settings(Settings { filter_mode: FilterMode::All, ..Settings::default() })
        .expect("settings accepted");
}

/// A `/commits` payload: one entry per `(sha, unix time)`, authored by alice.
fn commits_json(entries: &[(&str, i64)]) -> String {
    let body: Vec<String> = entries
        .iter()
        .map(|(sha, ts)| {
            let date = iso8601(*ts);
            format!(
                r#"{{"sha":"{sha}",
                     "html_url":"https://github.com/acme/platform/commit/{sha}",
                     "commit":{{"message":"Commit {sha}",
                                "author":{{"name":"Alice Ng","date":"{date}"}},
                                "committer":{{"name":"Alice Ng","date":"{date}"}}}},
                     "author":{{"login":"alice","id":1,"avatar_url":null}},
                     "committer":null}}"#
            )
        })
        .collect();
    format!("[{}]", body.join(","))
}

/// A repo whose only activity is the given commits.
fn repo_with_commits(entries: &[(&str, i64)]) -> RepoMock {
    RepoMock { commits: Resp::ok(commits_json(entries)), ..RepoMock::quiet("acme", "platform") }
}

/// The engine's own record of how far back this repo is covered.
fn floor(engine: &Engine) -> Option<i64> {
    engine.list_repos()[0].backfilled_to
}

/// The first poll is what establishes the floor a backfill steps down from: it
/// reached back 24 hours, so that is the oldest instant the repo covers. Later
/// polls fetch a *narrower*, newer window, so they must leave the floor alone —
/// otherwise every poll would erase whatever backfill had reached and the feed
/// could never get past its first day.
#[test]
fn the_first_poll_records_how_far_back_it_reached_and_later_polls_do_not_move_it() {
    let mut srv = server();
    let _mocks = mount(&mut srv, &RepoMock::quiet("acme", "platform"));
    let (engine, _dir) = engine_for(&srv);
    filter_all(&engine);
    engine.add_repo("acme/platform", None).unwrap();
    assert_eq!(floor(&engine), None, "a repo with no poll has no floor");

    let before = chrono::Utc::now().timestamp();
    engine.poll_now();
    let first = floor(&engine).expect("the first poll seeds the floor");

    assert!(
        (first - (before - FIRST_POLL_LOOKBACK_SECS)).abs() <= 5,
        "the floor should be the first poll's window start: {first} vs {before}"
    );

    engine.poll_now();
    assert_eq!(floor(&engine), Some(first), "a second poll moved the floor");
}

/// One step: the window is exactly `[floor - span, floor)`, only events inside
/// it are stored, the floor lands on the window's bottom, and the *poll*
/// watermark is left exactly where it was — a backfill reads the past and says
/// nothing about how current the repo is.
#[test]
fn one_step_stores_the_window_advances_the_floor_and_leaves_the_watermark_alone() {
    let mut srv = server();
    let quiet = mount(&mut srv, &RepoMock::quiet("acme", "platform"));
    let (engine, _dir) = engine_for(&srv);
    filter_all(&engine);
    let repo = engine.add_repo("acme/platform", None).unwrap();
    engine.poll_now();
    let start = floor(&engine).expect("the first poll seeds the floor");
    let watermark = engine.list_repos()[0].last_polled_at.expect("the poll set a watermark");

    // Three commits: one inside the window, one above it (ground the first poll
    // already covered) and one below it (the *next* step's ground).
    unmount(&quiet);
    let windows = since_log();
    let _mocks = mount_logging_since(
        &mut srv,
        &repo_with_commits(&[
            ("aaaa000000000000000000000000000000000001", start + 10),
            ("bbbb000000000000000000000000000000000002", start - 10),
            ("cccc000000000000000000000000000000000003", start - DAY - 10),
        ]),
        &windows,
    );

    let result = engine.backfill(None, DAY).unwrap();

    assert_eq!(result.repos.len(), 1, "one repo, one row: {result:?}");
    let row = &result.repos[0];
    assert_eq!(row.repo_id, repo.id);
    assert_eq!(row.to, start, "the step starts at the floor");
    assert_eq!(row.from, start - DAY, "and reaches back exactly one span");
    assert_eq!(row.inserted, 1, "only the commit inside the window: {row:?}");
    assert_eq!(row.error, None);

    let stored = engine.list_events(None, None, None, FilterMode::All, false, None, 500);
    assert_eq!(stored.len(), 1, "events: {stored:#?}");
    assert_eq!(stored[0].title, "Commit bbbb000000000000000000000000000000000002");
    assert_eq!(stored[0].occurred_at, start - 10);

    assert_eq!(floor(&engine), Some(start - DAY), "the floor did not step down");
    assert_eq!(
        engine.list_repos()[0].last_polled_at,
        Some(watermark),
        "a backfill must not move the poll watermark"
    );

    // The window's lower bound really went out on the wire as `since`.
    let seen = recorded(&windows);
    assert_eq!(seen, vec![start - DAY], "the backfill asked for the wrong window: {seen:?}");
}

/// Backfilled events are history the user scrolled back for, never news: they
/// land already seen, so they cannot inflate the unread badge or reach the
/// notification path a poll's new events do.
#[test]
fn backfilled_events_arrive_already_seen() {
    let mut srv = server();
    let quiet = mount(&mut srv, &RepoMock::quiet("acme", "platform"));
    let (engine, _dir) = engine_for(&srv);
    filter_all(&engine);
    engine.add_repo("acme/platform", None).unwrap();
    engine.poll_now();
    let start = floor(&engine).unwrap();
    assert_eq!(engine.unseen_count(), 0, "the quiet poll found nothing");

    unmount(&quiet);
    let _mocks = mount(
        &mut srv,
        &repo_with_commits(&[
            ("aaaa000000000000000000000000000000000001", start - 10),
            ("bbbb000000000000000000000000000000000002", start - 20),
        ]),
    );

    assert_eq!(engine.backfill(None, DAY).unwrap().repos[0].inserted, 2);

    let stored = engine.list_events(None, None, None, FilterMode::All, false, None, 500);
    assert_eq!(stored.len(), 2);
    assert!(stored.iter().all(|e| e.seen), "backfilled events must be seen: {stored:#?}");
    assert_eq!(engine.unseen_count(), 0, "the badge counted backfilled history as new");
}

/// The dedupe index is what makes a window safe to fetch twice. A rescan
/// reopens the floor at today, so backfilling further than before genuinely
/// re-covers ground already stored — and stores nothing again.
#[test]
fn a_backfill_over_an_overlapping_window_inserts_nothing_new() {
    let mut srv = server();
    let quiet = mount(&mut srv, &RepoMock::quiet("acme", "platform"));
    let (engine, _dir) = engine_for(&srv);
    filter_all(&engine);
    let repo = engine.add_repo("acme/platform", None).unwrap();
    engine.poll_now();
    let start = floor(&engine).unwrap();

    unmount(&quiet);
    let _mocks = mount(
        &mut srv,
        &repo_with_commits(&[
            ("aaaa000000000000000000000000000000000001", start - 10),
            ("bbbb000000000000000000000000000000000002", start - 3000),
        ]),
    );

    assert_eq!(engine.backfill(Some(repo.id), HOUR).unwrap().repos[0].inserted, 2);
    assert_eq!(engine.list_events(None, None, None, FilterMode::All, false, None, 500).len(), 2);

    // A rescan puts the floor back at today's window start, so the next step
    // reaches back across everything the first one already stored.
    engine.rescan(Some(repo.id)).unwrap();
    engine.poll_now();
    let reopened = floor(&engine).expect("the re-poll seeded the floor again");
    assert!(reopened >= start, "the re-poll's window should start no earlier than the first's");

    let second = engine.backfill(Some(repo.id), 2 * HOUR).unwrap();
    let row = &second.repos[0];
    assert!(row.from < start - 3000, "the second window must cover the first: {row:?}");
    assert_eq!(row.inserted, 0, "an already-stored window was stored again: {row:?}");
    assert_eq!(row.error, None);
    assert_eq!(
        engine.list_events(None, None, None, FilterMode::All, false, None, 500).len(),
        2,
        "the overlapping window duplicated rows"
    );
}

/// A rescan restarts the contiguous run of windows from a fresh 24 hours, so
/// the floor it would otherwise leave behind would claim coverage of a stretch
/// the repo is about to stop tracking.
#[test]
fn a_rescan_clears_the_floor_and_the_next_poll_re_seeds_it() {
    let mut srv = server();
    let _mocks = mount(&mut srv, &RepoMock::quiet("acme", "platform"));
    let (engine, _dir) = engine_for(&srv);
    filter_all(&engine);
    engine.add_repo("acme/platform", None).unwrap();
    engine.poll_now();
    engine.backfill(None, DAY).unwrap();
    let lowered = floor(&engine).expect("a floor to clear");

    engine.rescan(None).unwrap();

    assert_eq!(floor(&engine), None, "the rescan left the old floor behind");
    assert_eq!(engine.list_repos()[0].last_polled_at, None);

    engine.poll_now();
    let reseeded = floor(&engine).expect("the next poll re-seeds the floor");
    assert!(reseeded > lowered, "the re-seeded floor should be today's, not the old deep one");
}

/// Without a first poll there is no floor to step down from, and guessing one
/// would claim coverage of a stretch nothing ever fetched. The repo is reported,
/// not silently skipped — and nothing about it changes.
#[test]
fn a_repo_that_has_never_polled_reports_not_polled_yet() {
    let mut srv = server();
    let _mocks = mount(&mut srv, &RepoMock::busy("acme", "platform"));
    let (engine, _dir) = engine_for(&srv);
    filter_all(&engine);
    let repo = engine.add_repo("acme/platform", None).unwrap();

    let result = engine.backfill(None, DAY).unwrap();

    assert_eq!(result.repos.len(), 1);
    let row = &result.repos[0];
    assert_eq!(row.repo_id, repo.id);
    assert_eq!(row.error.as_deref(), Some("not polled yet"));
    assert_eq!((row.from, row.to), (0, 0), "no window was chosen: {row:?}");
    assert_eq!(row.inserted, 0);

    assert_eq!(floor(&engine), None, "a refused step must not invent a floor");
    assert_eq!(engine.list_repos()[0].last_polled_at, None);
    assert!(
        engine.list_events(None, None, None, FilterMode::All, false, None, 500).is_empty(),
        "a refused step stored events"
    );
}

/// One repo's refusal must not stop another's step, exactly as a poll isolates
/// its failures.
#[test]
fn one_repos_refusal_does_not_stop_the_others() {
    let mut srv = server();
    let polled = mount(&mut srv, &RepoMock::quiet("acme", "platform"));
    let _fresh = mount(&mut srv, &RepoMock::quiet("acme", "web"));
    let (engine, _dir) = engine_for(&srv);
    filter_all(&engine);
    let platform = engine.add_repo("acme/platform", None).unwrap();
    engine.poll_now();
    // Added after the poll, so it has no floor while `platform` does.
    let web = engine.add_repo("acme/web", None).unwrap();
    let start = engine
        .list_repos()
        .iter()
        .find(|r| r.id == platform.id)
        .and_then(|r| r.backfilled_to)
        .expect("the polled repo has a floor");

    unmount(&polled);
    let _mocks = mount(
        &mut srv,
        &repo_with_commits(&[("aaaa000000000000000000000000000000000001", start - 10)]),
    );

    let result = engine.backfill(None, DAY).unwrap();

    let ok = result.repos.iter().find(|r| r.repo_id == platform.id).expect("the polled repo");
    assert_eq!(ok.inserted, 1, "the healthy repo did not backfill: {result:?}");
    assert_eq!(ok.error, None);
    let refused = result.repos.iter().find(|r| r.repo_id == web.id).expect("the fresh repo");
    assert_eq!(refused.error.as_deref(), Some("not polled yet"));
}

/// A span outside the allowed range is clamped, not rejected: the caller wanting
/// more history simply calls again, and one asking for a minute still gets a
/// useful step instead of an error.
#[test]
fn the_span_is_clamped_at_both_ends_rather_than_rejected() {
    let mut srv = server();
    let _mocks = mount(&mut srv, &RepoMock::quiet("acme", "platform"));
    let (engine, _dir) = engine_for(&srv);
    filter_all(&engine);
    let repo = engine.add_repo("acme/platform", None).unwrap();
    engine.poll_now();
    let start = floor(&engine).unwrap();

    let tiny = engine.backfill(Some(repo.id), 1).unwrap();
    assert_eq!(tiny.repos[0].to - tiny.repos[0].from, BACKFILL_MIN_SPAN_SECS);
    assert_eq!(floor(&engine), Some(start - BACKFILL_MIN_SPAN_SECS));

    let huge = engine.backfill(Some(repo.id), 10 * 365 * DAY).unwrap();
    assert_eq!(huge.repos[0].to - huge.repos[0].from, BACKFILL_MAX_SPAN_SECS);
    assert_eq!(
        floor(&engine),
        Some(start - BACKFILL_MIN_SPAN_SECS - BACKFILL_MAX_SPAN_SECS),
        "the two clamped steps should be contiguous"
    );
}

/// Naming a repo the app no longer has is a mistake worth reporting, not a
/// silent no-op — the same call that a `rescan` of an unknown id makes.
#[test]
fn backfilling_an_unknown_repo_is_not_found() {
    let mut srv = server();
    let _mocks = mount(&mut srv, &RepoMock::quiet("acme", "platform"));
    let (engine, _dir) = engine_for(&srv);
    let repo = engine.add_repo("acme/platform", None).unwrap();
    engine.poll_now();
    let start = floor(&engine).unwrap();

    let err = engine.backfill(Some(repo.id + 999), DAY).unwrap_err();

    assert_eq!(err.kind, ErrorKind::NotFound);
    assert_eq!(floor(&engine), Some(start), "the real repo's floor moved");
}

/// A repo whose account has no token this session is skipped and reported, not
/// dropped and not advanced — the floor stays put so the window is retried once
/// a token arrives.
#[test]
fn a_signed_out_repo_is_reported_and_its_floor_stays_put() {
    let mut srv = server();
    let _mocks = mount(&mut srv, &RepoMock::quiet("acme", "platform"));
    let (engine, _dir) = engine_for(&srv);
    filter_all(&engine);
    engine.add_repo("acme/platform", None).unwrap();
    engine.poll_now();
    let start = floor(&engine).unwrap();

    engine.set_token(None);
    let result = engine.backfill(None, DAY).unwrap();

    assert_eq!(result.repos[0].error.as_deref(), Some("signed out"));
    assert_eq!(result.repos[0].inserted, 0);
    assert_eq!(floor(&engine), Some(start), "a skipped repo must not advance");
    assert_eq!(result.rate_limit_remaining, None, "no request was made");
}

/// A busy historical window can outrun the ten-page cap. The events that were
/// read are kept and the cursor still moves — re-reading the same window would
/// hit the same cap for ever and the feed would never get past it — but the
/// step says so, because a window read short loses events nothing will fetch
/// again.
#[test]
fn a_window_that_outruns_the_page_cap_is_reported_and_still_advances() {
    let mut srv = server();
    let quiet = mount(&mut srv, &RepoMock::quiet("acme", "platform"));
    let (engine, _dir) = engine_for(&srv);
    filter_all(&engine);
    engine.add_repo("acme/platform", None).unwrap();
    engine.poll_now();
    let start = floor(&engine).unwrap();

    // Ten pages of commits, each advertising another. Every page after the
    // first lives on its own path, so the `Link` chain is unambiguous — an
    // eleventh is offered and must never be fetched.
    unmount(&quiet);
    let base = srv.url();
    let page_one = commits_json(&[("aaaa000000000000000000000000000000000001", start - 10)]);
    let _mocks = mount(
        &mut srv,
        &RepoMock {
            commits: Resp::ok(page_one)
                .header("link", &format!("<{base}/p2>; rel=\"next\"")),
            ..RepoMock::quiet("acme", "platform")
        },
    );
    let mut pages = Vec::new();
    for page in 2..=10 {
        pages.push(mock_get(
            &mut srv,
            &format!("/p{page}"),
            &Resp::ok("[]").header("link", &format!("<{base}/p{}>; rel=\"next\"", page + 1)),
        ));
    }

    let result = engine.backfill(None, DAY).unwrap();

    let row = &result.repos[0];
    assert_eq!(row.error.as_deref(), Some("window truncated"), "row: {row:?}");
    assert_eq!(row.inserted, 1, "what was read is still stored: {row:?}");
    assert_eq!(
        floor(&engine),
        Some(start - DAY),
        "a truncated window must still advance, or the feed stalls on it for ever"
    );
}

/// A fetch that fails leaves the floor exactly where it was, so the next call
/// retries the same window instead of stepping over it and losing it for good.
#[test]
fn a_failed_step_leaves_the_floor_where_it_was() {
    let mut srv = server();
    let quiet = mount(&mut srv, &RepoMock::quiet("acme", "platform"));
    let (engine, _dir) = engine_for(&srv);
    filter_all(&engine);
    engine.add_repo("acme/platform", None).unwrap();
    engine.poll_now();
    let start = floor(&engine).unwrap();

    unmount(&quiet);
    let _mocks = mount(
        &mut srv,
        &RepoMock {
            commits: Resp::status(500, r#"{"message":"upstream is unwell"}"#),
            ..RepoMock::quiet("acme", "platform")
        },
    );

    let result = engine.backfill(None, DAY).unwrap();

    let row = &result.repos[0];
    assert!(row.error.is_some(), "a failed fetch must be reported: {row:?}");
    assert_eq!(row.inserted, 0);
    assert_eq!(floor(&engine), Some(start), "a failed step advanced the floor anyway");
}
