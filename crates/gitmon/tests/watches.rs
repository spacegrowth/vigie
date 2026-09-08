//! Watched threads, per the "Watched threads" section of `docs/CONTRACT.md`:
//! `watch_thread` / `unwatch_thread`, auto-watch during a poll, the state
//! refresh, and `list_events(watched_only)` composing with the team view
//! filter rather than bypassing it.
//!
//! Every watch here is reached the way the app reaches one — through a real
//! poll against a mock GitHub, or through `watch_thread` — so what is asserted
//! is the stored row, never a hand-made approximation of one.

mod common;

use common::{engine_for, fixture, iso8601, mock_get, mount, repo_meta, server, RepoMock, Resp};
use gitmon::{Engine, ErrorKind, FilterMode, Settings, ThreadKind, WatchSource};
use mockito::{Mock, ServerGuard};
use tempfile::TempDir;

// ---- fixture builders ---------------------------------------------------

fn json_array(items: &[String]) -> String {
    format!("[{}]", items.join(","))
}

fn user(login: &str) -> String {
    format!(r#"{{"login":"{login}","avatar_url":null}}"#)
}

/// A pull request as the *list* endpoint returns it, including the
/// `requested_reviewers` auto-watch reads.
#[allow(clippy::too_many_arguments)]
fn pull(
    slug: &str,
    number: u64,
    author: &str,
    title: &str,
    created_at: i64,
    merged_at: Option<i64>,
    closed_at: Option<i64>,
    reviewers: &[&str],
) -> String {
    let stamp = |at: Option<i64>| match at {
        Some(at) => format!("\"{}\"", iso8601(at)),
        None => "null".to_string(),
    };
    // GitHub never says `merged`: a merged PR is `closed` with a `merged_at`.
    let state = if closed_at.is_some() || merged_at.is_some() { "closed" } else { "open" };
    let updated_at = created_at.max(merged_at.unwrap_or(0)).max(closed_at.unwrap_or(0));
    format!(
        r#"{{"number":{number},"title":"{title}",
             "html_url":"https://github.com/{slug}/pull/{number}",
             "user":{who},"body":null,"state":"{state}",
             "created_at":"{created}","updated_at":"{updated}",
             "closed_at":{closed},"merged_at":{merged},"merged_by":null,
             "requested_reviewers":{reviewers},
             "head":null,"base":null,
             "additions":null,"deletions":null,"changed_files":null}}"#,
        who = user(author),
        created = iso8601(created_at),
        updated = iso8601(updated_at),
        closed = stamp(closed_at),
        merged = stamp(merged_at),
        reviewers =
            json_array(&reviewers.iter().map(|login| user(login)).collect::<Vec<_>>()),
    )
}

fn issue(slug: &str, number: u64, author: &str, title: &str, created_at: i64) -> String {
    format!(
        r#"{{"number":{number},"title":"{title}",
             "html_url":"https://github.com/{slug}/issues/{number}",
             "user":{who},"body":null,"state":"open",
             "created_at":"{created}","pull_request":null}}"#,
        who = user(author),
        created = iso8601(created_at),
    )
}

/// A comment on a PR conversation (`on_pull`) or on an issue. Only the
/// `html_url` tells the two apart, which is what picks `pr_commented` over
/// `issue_commented`.
fn issue_comment(
    slug: &str,
    number: u64,
    id: u64,
    actor: &str,
    at: i64,
    on_pull: bool,
) -> String {
    let segment = if on_pull { "pull" } else { "issues" };
    format!(
        r#"{{"id":{id},
             "html_url":"https://github.com/{slug}/{segment}/{number}#issuecomment-{id}",
             "body":null,"user":{who},"created_at":"{at}",
             "issue_url":"https://api.github.com/repos/{slug}/issues/{number}"}}"#,
        who = user(actor),
        at = iso8601(at),
    )
}

fn commit(slug: &str, sha: &str, actor: &str, at: i64) -> String {
    format!(
        r#"{{"sha":"{sha}",
             "html_url":"https://github.com/{slug}/commit/{sha}",
             "commit":{{"message":"Commit {sha}",
                        "author":{{"name":"{actor}","date":"{date}"}},
                        "committer":{{"name":"{actor}","date":"{date}"}}}},
             "author":{who},"committer":{who}}}"#,
        date = iso8601(at),
        who = user(actor),
    )
}

const SLUG: &str = "acme/platform";

/// Mocks `/user` so `verify_token` can resolve a login — the only way the
/// engine ever learns who is signed in, and therefore the only way auto-watch
/// turns on.
fn mock_user(srv: &mut ServerGuard, login: &str) -> Mock {
    mock_get(srv, "/user", &Resp::ok(format!(r#"{{"login":"{login}","avatar_url":null}}"#)))
}

fn filter_all(engine: &Engine) {
    let settings = engine.get_settings();
    engine.set_settings(Settings { filter_mode: FilterMode::All, ..settings }).unwrap();
}

// ---- watch_thread from stored events ------------------------------------

/// The repo used by the `watch_thread` tests: three PRs (open, merged, closed
/// without merging) and one issue, all by alice, all inside the window.
///
/// Nothing that `get_thread` would need is mocked, so any test here that
/// accidentally takes the live path fails on an unmocked request rather than
/// passing for the wrong reason.
fn engine_with_threads(srv: &mut ServerGuard) -> (Engine, TempDir, u64, i64) {
    let base = chrono::Utc::now().timestamp() - 600;
    let mut repo = RepoMock::quiet("acme", "platform");
    repo.pulls = Resp::ok(json_array(&[
        pull(SLUG, 101, "alice", "Split the poller out of the engine", base, None, None, &[]),
        pull(SLUG, 102, "alice", "Bundle rusqlite", base, Some(base + 10), Some(base + 10), &[]),
        pull(SLUG, 103, "alice", "Try tokio for the poll timer", base, None, Some(base + 20), &[]),
    ]));
    repo.reviews = vec![
        (101, Resp::json_array()),
        (102, Resp::json_array()),
        (103, Resp::json_array()),
    ];
    repo.issues =
        Resp::ok(json_array(&[issue(SLUG, 5, "alice", "Tray icon is blurry", base + 5)]));
    let mocks = mount(srv, &repo);
    let (engine, dir) = engine_for(srv);
    filter_all(&engine);
    let repo_id = engine.add_repo(SLUG, None).unwrap().id;
    let polled = engine.poll_now();
    assert!(polled.errors.is_empty(), "the fixture server misfired: {:?}", polled.errors);
    // Nothing is watched yet: auto-watch needs a signed-in login and there is
    // none, so every watch below is the one the test made.
    assert!(engine.list_watches().is_empty(), "the poll watched something on its own");
    drop(mocks);
    (engine, dir, repo_id, base)
}

/// `kind`, `title` and `state` all come from the stored events, with no request
/// at all — the common case, where the user watches something already in the
/// feed.
#[test]
fn watching_a_thread_reads_its_facts_from_the_stored_events() {
    let mut srv = server();
    let (engine, _dir, repo_id, _base) = engine_with_threads(&mut srv);

    let open = engine.watch_thread(repo_id, 101).expect("open PR watched");
    assert_eq!(open.repo_id, repo_id);
    assert_eq!(open.number, 101);
    assert_eq!(open.kind, ThreadKind::Pull);
    assert_eq!(open.title, "Split the poller out of the engine");
    assert_eq!(open.state, "open");
    assert_eq!(open.source, WatchSource::Manual, "watch_thread is always manual");
    assert!(open.since > 0);

    // A merged PR's newest stored event is `pr_merged`, which is `merged` —
    // never the `closed` GitHub reports beside it.
    let merged = engine.watch_thread(repo_id, 102).expect("merged PR watched");
    assert_eq!(merged.kind, ThreadKind::Pull);
    assert_eq!(merged.state, "merged");
    assert_eq!(merged.title, "Bundle rusqlite");

    let closed = engine.watch_thread(repo_id, 103).expect("closed PR watched");
    assert_eq!(closed.state, "closed");

    // An issue is `issue`, and an opened-and-nothing-else issue is open.
    let issue = engine.watch_thread(repo_id, 5).expect("issue watched");
    assert_eq!(issue.kind, ThreadKind::Issue);
    assert_eq!(issue.state, "open");
    assert_eq!(issue.title, "Tray icon is blurry");

    // Newest `since` first, and every row is there exactly once.
    let watches = engine.list_watches();
    assert_eq!(watches.len(), 4, "{watches:?}");
    let mut sinces: Vec<i64> = watches.iter().map(|w| w.since).collect();
    let sorted = {
        let mut s = sinces.clone();
        s.sort_unstable_by(|a, b| b.cmp(a));
        s
    };
    assert_eq!(sinces, sorted, "list_watches must be newest `since` first");
    sinces.dedup();
    assert!(watches.iter().all(|w| w.source == WatchSource::Manual));
}

/// Watching twice returns the row that is already there, unchanged. This is
/// what stops the app's "watch" button relabelling an auto-watch or moving its
/// `since` every time it is pressed.
#[test]
fn watching_an_already_watched_thread_returns_it_unchanged() {
    let mut srv = server();
    let (engine, _dir, repo_id, _base) = engine_with_threads(&mut srv);

    let first = engine.watch_thread(repo_id, 101).unwrap();
    let second = engine.watch_thread(repo_id, 101).unwrap();

    assert_eq!(first, second, "the second watch invented a new row");
    assert_eq!(engine.list_watches().len(), 1, "watching twice made two rows");
}

/// With nothing stored for the thread, the facts come from one live
/// `get_thread` — the app watching something it has only seen on GitHub.
#[test]
fn watching_an_unpolled_thread_falls_back_to_one_live_fetch() {
    let mut srv = server();
    // The thread fixtures, each `expect(1)`: exactly one live read, no more.
    let thread_mocks: Vec<Mock> = vec![
        mock_get(
            &mut srv,
            "/repos/acme/platform/pulls/101",
            &Resp::ok(fixture("thread_pull_101.json")),
        ),
        mock_get(
            &mut srv,
            "/repos/acme/platform/issues/101/comments",
            &Resp::ok(fixture("thread_comments.json")),
        ),
        mock_get(
            &mut srv,
            "/repos/acme/platform/pulls/101/reviews",
            &Resp::ok(fixture("thread_reviews.json")),
        ),
        mock_get(
            &mut srv,
            "/repos/acme/platform/pulls/101/comments",
            &Resp::ok(fixture("thread_review_comments.json")),
        ),
        mock_get(
            &mut srv,
            "/repos/acme/platform/pulls/101/commits",
            &Resp::ok(fixture("thread_pull_commits.json")),
        ),
    ]
    .into_iter()
    .map(|m| m.expect(1))
    .collect();
    let _meta =
        mock_get(&mut srv, "/repos/acme/platform", &Resp::ok(repo_meta("acme", "platform")));
    let (engine, _dir) = engine_for(&srv);
    let repo_id = engine.add_repo(SLUG, None).unwrap().id;
    // Deliberately never polled, so there is nothing stored to read.
    assert!(engine.list_events(None, None, None, FilterMode::All, false, None, 500).is_empty());

    let watch = engine.watch_thread(repo_id, 101).expect("thread watched from a live fetch");

    for mock in &thread_mocks {
        mock.assert();
    }
    assert_eq!(watch.kind, ThreadKind::Pull);
    assert_eq!(watch.title, "Split the poller out of the engine");
    assert_eq!(watch.state, "open");
    assert_eq!(watch.source, WatchSource::Manual);
}

#[test]
fn unwatching_a_thread_that_is_not_watched_is_not_found() {
    let mut srv = server();
    let (engine, _dir, repo_id, _base) = engine_with_threads(&mut srv);

    // Never watched at all.
    let err = engine.unwatch_thread(repo_id, 101).unwrap_err();
    assert_eq!(err.kind, ErrorKind::NotFound);

    engine.watch_thread(repo_id, 101).unwrap();
    engine.unwatch_thread(repo_id, 101).expect("the watch was there");
    assert!(engine.list_watches().is_empty());

    // Unwatching twice: the second call is not_found, not a silent success.
    assert_eq!(engine.unwatch_thread(repo_id, 101).unwrap_err().kind, ErrorKind::NotFound);
}

// ---- the team view filter and watched_only -------------------------------

/// The repo `watching_a_thread_does_not_exempt_its_actor_from_the_team_view_filter`
/// polls: alice (on the team) opens two PRs, bob (on no team) comments on
/// both and pushes a commit.
fn bypass_repo(base: i64) -> RepoMock {
    let mut repo = RepoMock::quiet("acme", "platform");
    repo.pulls = Resp::ok(json_array(&[
        pull(SLUG, 101, "alice", "Split the poller out of the engine", base, None, None, &[]),
        pull(SLUG, 202, "alice", "Drop the vendored icons", base, None, None, &[]),
    ]));
    repo.reviews = vec![(101, Resp::json_array()), (202, Resp::json_array())];
    repo.commits = Resp::ok(json_array(&[commit(SLUG, "bbb1", "bob", base + 10)]));
    repo.issue_comments = Resp::ok(json_array(&[
        issue_comment(SLUG, 101, 9001, "bob", base + 20, true),
        issue_comment(SLUG, 202, 9002, "bob", base + 30, true),
    ]));
    repo
}

/// Before this packet, a non-member's event survived ingestion only when it
/// landed on a watched thread — the "filter bypass" this test was named for.
/// Now ingestion drops nobody (packet gm-feed-r1), so there is nothing left
/// to bypass at that layer: bob's events are all stored on the very first
/// poll, watched or not. What still matters is that `list_events`'s filters
/// compose as a plain intersection — `watched_only` never exempts an actor
/// from the "My team" view filter, exactly as it never exempted one from
/// `team_id` before this packet either.
#[test]
fn watching_a_thread_does_not_exempt_its_actor_from_the_team_view_filter() {
    let mut srv = server();
    let base = chrono::Utc::now().timestamp() - 100;
    let _mocks = mount(&mut srv, &bypass_repo(base));
    let (engine, _dir) = engine_for(&srv);
    // Only alice is on a team; bob is on none and not signed in.
    engine.create_team("Platform", &["alice".to_string()]).unwrap();
    let repo_id = engine.add_repo(SLUG, None).unwrap().id;

    let first = engine.poll_now();
    assert!(first.errors.is_empty(), "{:?}", first.errors);
    // All three of bob's events (a commit and a comment on each PR) are
    // stored already, before anything is watched.
    let bobs_before = engine.list_events(None, Some("bob"), None, FilterMode::All, false, None, 500);
    assert_eq!(bobs_before.len(), 3, "bob's events must all be stored: {bobs_before:#?}");
    assert!(bobs_before.iter().all(|e| !e.watched), "nothing is watched yet");
    // The "My team" view still drops every one of them, same as `team_id`
    // would for a team bob is not on.
    assert!(
        engine.list_events(None, Some("bob"), None, FilterMode::Team, false, None, 500).is_empty(),
        "bob is on no team, so the team view filter must drop him"
    );

    engine.watch_thread(repo_id, 101).expect("PR 101 watched");

    // "Watched only" (under `All`) narrows to PR 101's two events — alice's
    // `pr_opened` and bob's comment — and marks them watched.
    let watched_only = engine.list_events(None, None, None, FilterMode::All, true, None, 500);
    assert_eq!(watched_only.len(), 2, "expected PR 101's two events: {watched_only:#?}");
    assert!(watched_only.iter().all(|e| e.number == Some(101) && e.watched));
    assert!(watched_only.iter().any(|e| e.actor_login == "bob"));

    // But combined with the "My team" view filter, bob drops back out: being
    // watched does not exempt an actor from the team filter any more than
    // `team_id` ever did.
    let watched_and_team_only = engine.list_events(None, None, None, FilterMode::Team, true, None, 500);
    assert!(
        watched_and_team_only.iter().all(|e| e.actor_login != "bob"),
        "watching a thread must not bypass the team view filter: {watched_and_team_only:#?}"
    );
    assert!(
        watched_and_team_only.iter().any(|e| e.actor_login == "alice"),
        "alice's own event on the watched thread must still show"
    );

    // Unwatching un-marks the feed at once, without deleting anything.
    engine.unwatch_thread(repo_id, 101).unwrap();
    let after = engine.list_events(None, Some("bob"), None, FilterMode::All, false, None, 500);
    assert_eq!(after.len(), 3, "unwatching must not delete what was stored");
    assert!(after.iter().all(|e| !e.watched), "unwatching un-marks the feed on the next read");
}

// ---- auto-watch ---------------------------------------------------------

/// alice's own PR, a PR she was asked to review, a PR that is neither, and an
/// issue she opened — only the first two are auto-watchable.
fn auto_watch_repo(base: i64) -> RepoMock {
    let mut repo = RepoMock::quiet("acme", "platform");
    repo.pulls = Resp::ok(json_array(&[
        pull(SLUG, 101, "alice", "Split the poller out of the engine", base, None, None, &[]),
        pull(SLUG, 202, "bob", "Drop the vendored icons", base, None, None, &["Alice", "carol"]),
        pull(SLUG, 303, "bob", "Try tokio for the poll timer", base, None, None, &["carol"]),
    ]));
    repo.reviews = vec![
        (101, Resp::json_array()),
        (202, Resp::json_array()),
        (303, Resp::json_array()),
    ];
    // An issue alice opened: issues never auto-watch, whoever wrote them.
    repo.issues = Resp::ok(json_array(&[issue(SLUG, 5, "alice", "Tray icon is blurry", base)]));
    repo
}

/// An engine pointed at `auto_watch_repo`, optionally signed in as alice.
fn auto_watch_engine(
    srv: &mut ServerGuard,
    sign_in: bool,
    auto_watch: bool,
) -> (Engine, TempDir, u64, Vec<Mock>) {
    // Inside 300 seconds of now, so a *second* poll's overlap window still
    // reaches these PRs: the walk down `pulls` stops at the first one updated
    // before the window, and a test that polls twice needs the second pass to
    // see the same threads the first did.
    let base = chrono::Utc::now().timestamp() - 100;
    let mut mocks = mount(srv, &auto_watch_repo(base));
    mocks.push(mock_user(srv, "alice"));
    let (engine, dir) = engine_for(srv);
    filter_all(&engine);
    engine
        .set_settings(Settings {
            filter_mode: FilterMode::All,
            auto_watch,
            ..Settings::default()
        })
        .unwrap();
    if sign_in {
        assert_eq!(engine.verify_token().unwrap(), "alice");
    }
    let repo_id = engine.add_repo(SLUG, None).unwrap().id;
    (engine, dir, repo_id, mocks)
}

/// With auto-watch on and a login known, a poll watches the PRs the signed-in
/// user wrote (`author`) and the ones they were asked to review (`reviewer`),
/// and nothing else.
#[test]
fn a_poll_auto_watches_authored_and_review_requested_pull_requests() {
    let mut srv = server();
    let (engine, _dir, repo_id, _mocks) = auto_watch_engine(&mut srv, true, true);

    let polled = engine.poll_now();
    assert!(polled.errors.is_empty(), "{:?}", polled.errors);

    let watches = engine.list_watches();
    let mut by_number: Vec<_> = watches.iter().map(|w| (w.number, w.source)).collect();
    by_number.sort_unstable_by_key(|(number, _)| *number);
    assert_eq!(
        by_number,
        vec![(101, WatchSource::Author), (202, WatchSource::Reviewer)],
        "watches: {watches:?}"
    );
    // PR 303 is bob's and carol's; the issue is alice's but issues never
    // auto-watch.
    assert!(watches.iter().all(|w| w.number != 303 && w.number != 5));

    for watch in &watches {
        assert_eq!(watch.repo_id, repo_id);
        assert_eq!(watch.kind, ThreadKind::Pull);
        assert_eq!(watch.state, "open");
    }
    let authored = watches.iter().find(|w| w.number == 101).unwrap();
    assert_eq!(authored.title, "Split the poller out of the engine");
    // The reviewer match ignores case: GitHub spells it "Alice" in the payload.
    let reviewing = watches.iter().find(|w| w.number == 202).unwrap();
    assert_eq!(reviewing.title, "Drop the vendored icons");

    // And the feed marks exactly those threads' events.
    let events = engine.list_events(None, None, None, FilterMode::All, false, None, 500);
    for event in &events {
        let expected = matches!(event.number, Some(101) | Some(202));
        assert_eq!(event.watched, expected, "wrong `watched` on {event:?}");
    }
}

#[test]
fn auto_watch_does_nothing_when_it_is_switched_off() {
    let mut srv = server();
    let (engine, _dir, _repo_id, _mocks) = auto_watch_engine(&mut srv, true, false);

    let polled = engine.poll_now();

    assert!(polled.errors.is_empty(), "{:?}", polled.errors);
    assert!(engine.list_watches().is_empty(), "auto_watch is off");
    assert!(polled.new_events.iter().all(|e| !e.watched));
}

/// With no login resolved — a token set but never verified — auto-watch has
/// nobody to match against and must watch nothing rather than guess.
#[test]
fn auto_watch_does_nothing_when_no_login_is_known() {
    let mut srv = server();
    let (engine, _dir, _repo_id, _mocks) = auto_watch_engine(&mut srv, false, true);

    let polled = engine.poll_now();

    assert!(polled.errors.is_empty(), "{:?}", polled.errors);
    assert!(engine.list_watches().is_empty(), "nobody is signed in");
}

/// Signing out forgets the login, so auto-watch stops with the credential.
#[test]
fn signing_out_stops_auto_watch() {
    let mut srv = server();
    let (engine, _dir, _repo_id, _mocks) = auto_watch_engine(&mut srv, true, true);
    engine.set_token(None);
    engine.set_token(Some("test-credential".to_string()));

    let polled = engine.poll_now();

    assert!(polled.errors.is_empty(), "{:?}", polled.errors);
    assert!(engine.list_watches().is_empty(), "the login was forgotten at sign-out");
}

/// Auto-watch never overwrites a row that is already there: a manual watch
/// keeps its `manual` source and its original `since`.
#[test]
fn auto_watch_leaves_an_existing_manual_watch_alone() {
    let mut srv = server();
    // Auto-watch off for the first poll, so the events land with nothing
    // watched and the manual watch below is genuinely the first row.
    let (engine, _dir, repo_id, _mocks) = auto_watch_engine(&mut srv, true, false);
    engine.poll_now();
    let manual = engine.watch_thread(repo_id, 101).expect("PR 101 watched by hand");
    assert_eq!(manual.source, WatchSource::Manual);

    // Now switch auto-watch on and poll again: 101 is alice's own PR, so
    // auto-watch would claim it if it overwrote.
    let settings = engine.get_settings();
    engine.set_settings(Settings { auto_watch: true, ..settings }).unwrap();
    engine.poll_now();

    let watches = engine.list_watches();
    let kept = watches.iter().find(|w| w.number == 101).expect("101 is still watched");
    assert_eq!(kept.source, WatchSource::Manual, "auto-watch relabelled a manual watch");
    assert_eq!(kept.since, manual.since, "auto-watch moved a manual watch's `since`");
    // The review request on 202 still auto-watches, so this is not a poll that
    // simply did nothing.
    assert_eq!(
        watches.iter().find(|w| w.number == 202).map(|w| w.source),
        Some(WatchSource::Reviewer)
    );
}

// ---- state refresh and "Clear closed" -----------------------------------

/// Each poll refreshes the `title` and `state` of every watched thread it sees,
/// and "Clear closed" then unwatches everything that is no longer open.
#[test]
fn a_poll_refreshes_watched_state_and_clear_closed_drops_the_finished_ones() {
    let mut srv = server();
    let base = chrono::Utc::now().timestamp() - 100;
    let open_pulls = |title_101: &str, merged: Option<i64>| {
        Resp::ok(json_array(&[
            pull(SLUG, 101, "alice", title_101, base, merged, merged, &[]),
            pull(SLUG, 202, "alice", "Drop the vendored icons", base, None, None, &[]),
        ]))
    };

    let mut repo = RepoMock::quiet("acme", "platform");
    repo.pulls = open_pulls("Split the poller out of the engine", None);
    repo.reviews = vec![(101, Resp::json_array()), (202, Resp::json_array())];
    let mut mocks = mount(&mut srv, &repo);
    mocks.push(mock_user(&mut srv, "alice"));
    let (engine, _dir) = engine_for(&srv);
    engine
        .set_settings(Settings { filter_mode: FilterMode::All, ..Settings::default() })
        .unwrap();
    engine.verify_token().unwrap();
    engine.add_repo(SLUG, None).unwrap();

    engine.poll_now();
    let watches = engine.list_watches();
    assert_eq!(watches.len(), 2, "both of alice's PRs are auto-watched: {watches:?}");
    assert!(watches.iter().all(|w| w.state == "open"));

    // "Clear closed" while everything is open removes nothing.
    assert_eq!(engine.clear_closed_watches(), 0);
    assert_eq!(engine.list_watches().len(), 2);

    // PR 101 is merged and renamed on GitHub. Re-point the mock and poll again.
    drop(mocks);
    let mut merged_repo = RepoMock::quiet("acme", "platform");
    merged_repo.pulls = open_pulls("Split the poller out of the engine (merged)", Some(base + 10));
    merged_repo.reviews = vec![(101, Resp::json_array()), (202, Resp::json_array())];
    let mut mocks = mount(&mut srv, &merged_repo);
    mocks.push(mock_user(&mut srv, "alice"));

    engine.poll_now();

    let watches = engine.list_watches();
    let refreshed = watches.iter().find(|w| w.number == 101).expect("101 is still watched");
    assert_eq!(refreshed.state, "merged", "a merged PR's watch must say merged, not closed");
    assert_eq!(
        refreshed.title, "Split the poller out of the engine (merged)",
        "the title is refreshed too"
    );
    // Merged threads stay watched until unwatched.
    assert_eq!(watches.len(), 2);
    assert_eq!(watches.iter().find(|w| w.number == 202).unwrap().state, "open");

    // "Clear closed" now takes the merged one and leaves the open one.
    assert_eq!(engine.clear_closed_watches(), 1);
    let left = engine.list_watches();
    assert_eq!(left.len(), 1, "{left:?}");
    assert_eq!(left[0].number, 202);
    assert_eq!(left[0].state, "open");
}

// ---- list_events(watched_only) ------------------------------------------

/// The `limit` must count rows that survived the watched filter, not rows read
/// before it — the same trap the team filter has, and the same shape of proof.
#[test]
fn the_limit_applies_after_the_watched_filter() {
    let mut srv = server();
    let _mocks = mount(&mut srv, &RepoMock::busy("acme", "platform"));
    let (engine, _dir) = engine_for(&srv);
    filter_all(&engine);
    let repo_id = engine.add_repo(SLUG, None).unwrap().id;
    assert_eq!(engine.poll_now().new_events.len(), 12, "the busy fixture drifted");
    engine.watch_thread(repo_id, 101).expect("PR 101 watched");

    // The busy fixture interleaves threads, so the newest three events are not
    // all on PR 101. That is what gives the next assertion teeth: an
    // implementation that read three rows and *then* dropped the unwatched ones
    // would hand back fewer than three here.
    let newest_three = engine.list_events(None, None, None, FilterMode::All, false, None, 3);
    assert!(
        newest_three.iter().any(|e| e.number != Some(101)),
        "fixture drifted; this test no longer distinguishes the two orders"
    );

    let page = engine.list_events(None, None, None, FilterMode::All, true, None, 3);
    assert_eq!(page.len(), 3, "a full page of PR 101's events");
    assert!(page.iter().all(|e| e.number == Some(101)));
    assert!(page.iter().all(|e| e.watched), "every row in a watched page is watched");

    // Paging through with the cursor still respects the filter, and no event
    // comes back twice.
    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let page = engine.list_events(None, None, None, FilterMode::All, true, cursor, 2);
        if page.is_empty() {
            break;
        }
        cursor = Some(page.last().unwrap().id);
        seen.extend(page.into_iter().map(|e| e.id));
    }
    assert_eq!(seen.len(), 4, "every one of PR 101's events, once: {seen:?}");
    let mut unique = seen.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), seen.len(), "an event was paged twice");

    // Unwatching empties the view without deleting anything.
    engine.unwatch_thread(repo_id, 101).unwrap();
    assert!(engine.list_events(None, None, None, FilterMode::All, true, None, 500).is_empty());
    assert_eq!(engine.list_events(None, None, None, FilterMode::All, false, None, 500).len(), 12);
}

/// `watched_only` combines with the other filters rather than replacing them,
/// and never admits a commit — a commit has no `number`, so it belongs to no
/// thread and can never be watched.
#[test]
fn watched_only_never_admits_a_commit_and_still_honours_the_other_filters() {
    let mut srv = server();
    let _mocks = mount(&mut srv, &RepoMock::busy("acme", "platform"));
    let (engine, _dir) = engine_for(&srv);
    filter_all(&engine);
    let repo_id = engine.add_repo(SLUG, None).unwrap().id;
    engine.poll_now();
    engine.watch_thread(repo_id, 101).unwrap();
    engine.watch_thread(repo_id, 5).unwrap();

    let watched = engine.list_events(None, None, None, FilterMode::All, true, None, 500);
    assert!(!watched.is_empty());
    assert!(
        watched.iter().all(|e| e.number.is_some()),
        "a commit reached a watched-only page: {watched:#?}"
    );
    assert!(watched.iter().all(|e| matches!(e.number, Some(101) | Some(5))));

    // Narrowing by actor on top of it intersects rather than overriding.
    let bobs = engine.list_events(None, Some("bob"), None, FilterMode::All, true, None, 500);
    assert!(!bobs.is_empty());
    assert!(bobs.iter().all(|e| e.actor_login == "bob" && e.watched));
    // And an unwatched repo id yields nothing rather than everything.
    assert!(engine.list_events(Some(repo_id + 999), None, None, FilterMode::All, true, None, 500).is_empty());
}

/// A cross-check that the `(repo_id, number)` key is really a pair: the same
/// PR number in a different repo is a different thread.
#[test]
fn a_watch_is_keyed_on_the_repo_and_the_number_together() {
    let mut srv = server();
    let base = chrono::Utc::now().timestamp() - 600;
    let one_pull = |slug: &str| {
        let mut repo = RepoMock::quiet(
            slug.split('/').next().unwrap(),
            slug.split('/').nth(1).unwrap(),
        );
        repo.pulls =
            Resp::ok(json_array(&[pull(slug, 101, "alice", "Same number", base, None, None, &[])]));
        repo.reviews = vec![(101, Resp::json_array())];
        repo
    };
    let mut mocks = mount(&mut srv, &one_pull("acme/platform"));
    mocks.extend(mount(&mut srv, &one_pull("acme/web")));
    let (engine, _dir) = engine_for(&srv);
    filter_all(&engine);
    let platform = engine.add_repo("acme/platform", None).unwrap().id;
    let web = engine.add_repo("acme/web", None).unwrap().id;
    engine.poll_now();

    engine.watch_thread(platform, 101).unwrap();

    let watches = engine.list_watches();
    assert_eq!(watches.len(), 1);
    assert_eq!(watches[0].repo_id, platform);
    let events = engine.list_events(None, None, None, FilterMode::All, true, None, 500);
    assert!(!events.is_empty());
    assert!(
        events.iter().all(|e| e.repo_id == platform),
        "acme/web's PR 101 was marked watched too: {events:#?}"
    );
    assert_eq!(engine.unwatch_thread(web, 101).unwrap_err().kind, ErrorKind::NotFound);
}
