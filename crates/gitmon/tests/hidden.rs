//! Hiding a repo (docs/CONTRACT.md "Hiding a repo"): the middle state between
//! watching a repo and removing it.
//!
//! The claim these tests make is a budget claim as much as a UI one — a hidden
//! repo must cost *nothing*. So the hidden repo's endpoints are taken off the
//! mock server entirely: from that point on any request to it comes back 501
//! and lands in the poll's `errors`, which is what an empty `errors` here is
//! evidence of.

mod common;

use common::{engine_for, fixture, mock_get, mount, server, unmount, RepoMock, Resp};
use gitmon::{FilterMode, BACKFILL_DEFAULT_SPAN_SECS};

/// Every event, newest first, unscoped.
fn all_events(engine: &gitmon::Engine) -> Vec<gitmon::Event> {
    engine.list_events(None, None, None, FilterMode::All, false, None, 500)
}

#[test]
fn a_hidden_repo_is_not_polled_and_comes_back_whole() {
    let mut srv = server();
    let quiet = RepoMock::quiet("acme", "platform");
    let busy = RepoMock::busy("acme", "web");
    let _quiet_mocks = mount(&mut srv, &quiet);
    let busy_mocks = mount(&mut srv, &busy);
    let (engine, _dir) = engine_for(&srv);
    engine.add_repo("acme/platform", None).unwrap();
    let web = engine.add_repo("acme/web", None).unwrap();

    let first = engine.poll_now();
    assert!(first.errors.is_empty(), "unexpected errors: {:?}", first.errors);
    let before = all_events(&engine);
    assert!(
        before.iter().any(|e| e.repo_id == web.id),
        "the busy repo stored nothing to hide"
    );
    let unseen_before = engine.unseen_count();
    let polled_before = engine
        .list_repos()
        .into_iter()
        .find(|r| r.id == web.id)
        .and_then(|r| r.last_polled_at)
        .expect("the busy repo polled");

    // Hide it, and take every endpoint it has off the server.
    engine.set_repo_hidden(web.id, true).unwrap();
    unmount(&busy_mocks);

    // It is gone from the lists the app reads, and still in the database.
    assert!(engine.list_repos().iter().all(|r| r.id != web.id), "a hidden repo is still listed");
    let hidden_list = engine.list_hidden_repos();
    assert_eq!(hidden_list.len(), 1);
    assert_eq!(hidden_list[0].id, web.id);
    assert!(hidden_list[0].hidden);

    // It is gone from every read.
    let during = all_events(&engine);
    assert!(
        during.iter().all(|e| e.repo_id != web.id),
        "a hidden repo's events are still in the feed"
    );
    assert!(engine.list_watches().iter().all(|w| w.repo_id != web.id));
    assert!(engine.open_pulls(None, None, None).unwrap().iter().all(|p| p.repo_id != web.id));
    assert!(engine.unseen_count() < unseen_before, "the hidden repo still counts as unseen");

    // And it costs nothing: a poll with its endpoints unmounted still reports
    // no errors, which it could not if a single request had gone out to it.
    let second = engine.poll_now();
    assert!(second.errors.is_empty(), "the hidden repo was polled: {:?}", second.errors);
    assert!(second.new_events.iter().all(|e| e.repo_id != web.id));

    // Backfill is a request like any other, so an explicit one says so rather
    // than spending the budget hiding was meant to save.
    let backfilled = engine.backfill(Some(web.id), BACKFILL_DEFAULT_SPAN_SECS).unwrap();
    assert_eq!(backfilled.repos.len(), 1);
    assert_eq!(backfilled.repos[0].error.as_deref(), Some("hidden"));
    assert_eq!(backfilled.repos[0].inserted, 0);
    // The sweep never reaches it at all.
    let swept = engine.backfill(None, BACKFILL_DEFAULT_SPAN_SECS).unwrap();
    assert!(swept.repos.iter().all(|r| r.repo_id != web.id));

    // Its watermark simply stopped advancing while nothing polled it.
    let still_hidden = engine.list_hidden_repos().into_iter().next().unwrap();
    assert_eq!(still_hidden.last_polled_at, Some(polled_before));

    // Unhiding restores every one of those reads, with no refetch: the events
    // below are the ones stored before it was ever hidden, and its endpoints
    // are still off the server.
    engine.set_repo_hidden(web.id, false).unwrap();
    assert_eq!(all_events(&engine), before, "unhiding did not restore the feed");
    assert_eq!(engine.unseen_count(), unseen_before);
    assert!(engine.list_repos().iter().any(|r| r.id == web.id));
    assert!(engine.list_hidden_repos().is_empty());

    // And it polls again on the next cycle.
    let mocks = mount(&mut srv, &busy);
    let third = engine.poll_now();
    assert!(third.errors.is_empty(), "unexpected errors: {:?}", third.errors);
    let resumed = engine
        .list_repos()
        .into_iter()
        .find(|r| r.id == web.id)
        .and_then(|r| r.last_polled_at)
        .expect("the unhidden repo polled");
    assert!(resumed >= polled_before, "the watermark went backwards");
    unmount(&mocks);
}

/// Hiding is not removing, and the app must never be able to blur the two:
/// removing still takes the history with it.
#[test]
fn removing_a_hidden_repo_still_cascades_its_history() {
    let mut srv = server();
    let busy = RepoMock::busy("acme", "web");
    let _mocks = mount(&mut srv, &busy);
    let (engine, _dir) = engine_for(&srv);
    let web = engine.add_repo("acme/web", None).unwrap();
    engine.poll_now();
    assert!(!all_events(&engine).is_empty());

    engine.set_repo_hidden(web.id, true).unwrap();
    engine.remove_repo(web.id).unwrap();

    assert!(engine.list_repos().is_empty());
    assert!(engine.list_hidden_repos().is_empty(), "a removed repo is not merely hidden");
    assert!(all_events(&engine).is_empty(), "removing left the events behind");
    assert_eq!(engine.unseen_count(), 0);
}

/// A hidden repo is still watched, so it must not be offered back as
/// something to add — adding it would hand back the same, still hidden, row
/// and look like a no-op.
#[test]
fn a_hidden_repo_is_not_suggested_as_a_new_one() {
    let mut srv = server();
    let _mocks = mount(&mut srv, &RepoMock::quiet("acme", "platform"));
    let _user = mock_get(&mut srv, "/user", &Resp::ok(fixture("user.json")));
    let _repos = mock_get(&mut srv, "/user/repos", &Resp::ok(fixture("user_repos.json")));
    let _search = mock_get(&mut srv, "/search/issues", &Resp::ok(fixture("search_issues.json")));
    let (engine, _dir) = engine_for(&srv);
    let platform = engine.add_repo("acme/platform", None).unwrap();
    assert!(
        !slugs(&engine).contains(&"acme/platform".to_string()),
        "a watched repo was suggested back"
    );

    engine.set_repo_hidden(platform.id, true).unwrap();

    assert!(
        !slugs(&engine).contains(&"acme/platform".to_string()),
        "a hidden repo was suggested as a new one"
    );
}

/// The slugs `suggest_repos` offers, in its own order.
fn slugs(engine: &gitmon::Engine) -> Vec<String> {
    engine
        .suggest_repos(None)
        .unwrap()
        .iter()
        .map(|s| format!("{}/{}", s.owner, s.name))
        .collect()
}
