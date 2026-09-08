//! Conditional requests and GitHub's polling hint: `If-None-Match`, 304s, and
//! `X-Poll-Interval`.
//!
//! Every mock here is deliberately *narrow*. A mock that insists on
//! `If-None-Match: <tag>` answers nothing else, and one that insists the header
//! is absent answers nothing else either — so a poll that sends the wrong thing
//! matches no mock, comes back 501, and lands in `errors` rather than passing
//! quietly.

mod common;

use common::{
    engine_for, mock_get, mock_get_if_none_match, mock_get_page, mock_get_unconditional,
    repo_meta, server, unmount, Resp,
};
use gitmon::{PollOptions, Repo};
use mockito::{Mock, ServerGuard};

const OWNER: &str = "acme";
const NAME: &str = "platform";
/// The tag every listing hands back on the first poll.
const TAG: &str = "\"v1\"";

/// Every list endpoint one poll of a repo touches, in no particular order. A
/// quiet repo has no PRs, so no per-PR review endpoint is ever asked for.
const LIST_PATHS: [&str; 5] =
    ["commits", "pulls", "issues", "issues/comments", "pulls/comments"];

fn path(endpoint: &str) -> String {
    format!("/repos/{OWNER}/{NAME}/{endpoint}")
}

/// An empty listing that hands back [`TAG`] and reports `remaining` budget.
fn empty_with_tag(remaining: &str) -> Resp {
    Resp::ok("[]").header("etag", TAG).header("x-ratelimit-remaining", remaining)
}

/// Mounts every list endpoint, insisting that no `If-None-Match` goes out —
/// what a first poll (and the first poll after a rescan) must look like.
fn mount_unconditional(srv: &mut ServerGuard, response: &Resp) -> Vec<Mock> {
    LIST_PATHS.iter().map(|e| mock_get_unconditional(srv, &path(e), response)).collect()
}

/// The repo, added against a freshly mounted metadata endpoint.
fn add_repo(srv: &mut ServerGuard) -> (gitmon::Engine, tempfile::TempDir, Repo, Mock) {
    let meta = mock_get(srv, &format!("/repos/{OWNER}/{NAME}"), &Resp::ok(repo_meta(OWNER, NAME)));
    let (engine, dir) = engine_for(srv);
    let repo = engine.add_repo(&format!("{OWNER}/{NAME}"), None).expect("repo added");
    (engine, dir, repo, meta)
}

/// The first poll sends no tag (it has none) and stores what came back; the
/// second sends it, and the 304 it earns costs nothing and yields nothing.
///
/// The rate-limit assertion is the point of the `9999`: GitHub sends
/// `x-ratelimit-remaining` beside a 304 too, describing a budget the 304 never
/// spent. Reading it would hand the app a number that is both stale and
/// wrong-way-round — higher than the budget actually left.
#[test]
fn a_stored_etag_goes_back_out_and_its_304_yields_nothing() {
    let mut srv = server();
    let (engine, _dir, _repo, _meta) = add_repo(&mut srv);

    let first = mount_unconditional(&mut srv, &empty_with_tag("100"));
    let result = engine.poll_now();
    assert!(result.errors.is_empty(), "first poll: {:?}", result.errors);
    assert!(result.new_events.is_empty(), "a quiet repo has no events");
    assert_eq!(result.rate_limit_remaining, Some(100));
    unmount(&first);

    // Second poll. Commits 304s with a *stale* budget attached; the other four
    // answer 200 with the real one.
    let not_modified = mock_get_if_none_match(
        &mut srv,
        &path("commits"),
        TAG,
        &Resp::status(304, "").header("x-ratelimit-remaining", "9999"),
    );
    let mut rest: Vec<Mock> = LIST_PATHS
        .iter()
        .filter(|e| **e != "commits")
        .map(|e| {
            mock_get_if_none_match(&mut srv, &path(e), TAG, &empty_with_tag("90"))
        })
        .collect();

    let result = engine.poll_now();
    assert!(result.errors.is_empty(), "second poll: {:?}", result.errors);
    assert!(result.new_events.is_empty(), "a 304 contributes no events");
    assert_eq!(
        result.rate_limit_remaining,
        Some(90),
        "the 304's own x-ratelimit-remaining must not be recorded"
    );
    // Every listing was asked conditionally: the mocks would not have answered
    // otherwise, and the poll would have errored.
    not_modified.assert();
    for mock in &mut rest {
        mock.assert();
    }
    unmount(&rest);
    unmount(&[not_modified]);
}

/// A rescan exists to refetch. A tag GitHub would still match turns that
/// refetch into an empty 304, so `rescan` drops the repo's cached tags — and
/// the poll after it goes out bare again.
#[test]
fn rescan_forgets_the_cached_etags() {
    let mut srv = server();
    let (engine, _dir, repo, _meta) = add_repo(&mut srv);

    let first = mount_unconditional(&mut srv, &empty_with_tag("100"));
    assert!(engine.poll_now().errors.is_empty());
    unmount(&first);

    // Without the rescan this poll would carry `If-None-Match` and match
    // nothing here.
    engine.rescan(Some(repo.id)).expect("rescan");
    let after = mount_unconditional(&mut srv, &empty_with_tag("100"));
    let result = engine.poll_now();
    assert!(result.errors.is_empty(), "the poll after a rescan must send no tag: {result:?}");
    unmount(&after);
}

/// A 304 on page one ends the listing. There is no point asking for page two of
/// something GitHub just said is unchanged.
#[test]
fn a_304_on_page_one_never_asks_for_page_two() {
    let mut srv = server();
    let (engine, _dir, _repo, _meta) = add_repo(&mut srv);

    // First poll: commits comes back paginated, so page two is real and gets
    // fetched. Everything else is a quiet single page.
    let next = format!("<{}{}?page=2>; rel=\"next\"", srv.url(), path("commits"));
    let page_one = mock_get_unconditional(
        &mut srv,
        &path("commits"),
        &Resp::ok("[]").header("etag", TAG).header("link", &next),
    );
    let page_two = mock_get_page(&mut srv, &path("commits"), "2", &Resp::ok("[]"));
    let others: Vec<Mock> = LIST_PATHS
        .iter()
        .filter(|e| **e != "commits")
        .map(|e| mock_get_unconditional(&mut srv, &path(e), &empty_with_tag("100")))
        .collect();

    assert!(engine.poll_now().errors.is_empty(), "first poll");
    page_two.assert();
    unmount(&[page_one, page_two]);
    unmount(&others);

    // Second poll: page one 304s. Page two must not be asked for at all.
    let not_modified =
        mock_get_if_none_match(&mut srv, &path("commits"), TAG, &Resp::status(304, ""));
    let page_two = mock_get_page(&mut srv, &path("commits"), "2", &Resp::ok("[]")).expect(0);
    let others: Vec<Mock> = LIST_PATHS
        .iter()
        .filter(|e| **e != "commits")
        .map(|e| mock_get_if_none_match(&mut srv, &path(e), TAG, &empty_with_tag("100")))
        .collect();

    let result = engine.poll_now();
    assert!(result.errors.is_empty(), "second poll: {:?}", result.errors);
    not_modified.assert();
    page_two.assert();
    unmount(&[not_modified, page_two]);
    unmount(&others);
}

/// GitHub's `X-Poll-Interval` is a floor on how often one repo may be asked
/// about. The background poller honours it and says so in `skipped`; a
/// hand-driven "Poll now" passes `force` and ignores it.
#[test]
fn a_repo_under_its_poll_interval_is_skipped_unless_forced() {
    let mut srv = server();
    let (engine, _dir, repo, _meta) = add_repo(&mut srv);

    let first = mount_unconditional(
        &mut srv,
        &Resp::ok("[]").header("etag", TAG).header("x-poll-interval", "600"),
    );
    let result = engine.poll_now();
    assert!(result.errors.is_empty(), "first poll: {:?}", result.errors);
    assert!(result.skipped.is_empty(), "nothing to skip on a first poll");
    // Every mock is taken off the server, so any request the next poll makes
    // comes back 501 and lands in `errors`.
    unmount(&first);

    let result = engine.poll_now();
    assert_eq!(result.skipped, vec![repo.id], "600s have not elapsed");
    assert!(result.errors.is_empty(), "a skip is not a failure: {:?}", result.errors);
    assert!(result.new_events.is_empty());

    // Forced, the same poll goes out anyway — carrying the tags it stored.
    let forced: Vec<Mock> = LIST_PATHS
        .iter()
        .map(|e| mock_get_if_none_match(&mut srv, &path(e), TAG, &Resp::status(304, "")))
        .collect();
    let result = engine.poll_now_with(PollOptions { force: true });
    assert!(result.skipped.is_empty(), "force ignores the interval");
    assert!(result.errors.is_empty(), "forced poll: {:?}", result.errors);
    for mock in &forced {
        mock.assert();
    }
    unmount(&forced);
}

/// A repo whose account has no token is not polled, so it never learns a hint
/// and is never skipped for one — it keeps reporting `signed out` instead.
#[test]
fn a_repo_with_no_hint_is_always_due() {
    let mut srv = server();
    let (engine, _dir, _repo, _meta) = add_repo(&mut srv);

    // No `x-poll-interval` anywhere: two polls back to back both go out.
    let first = mount_unconditional(&mut srv, &empty_with_tag("100"));
    assert!(engine.poll_now().errors.is_empty());
    unmount(&first);

    let second: Vec<Mock> = LIST_PATHS
        .iter()
        .map(|e| mock_get_if_none_match(&mut srv, &path(e), TAG, &Resp::status(304, "")))
        .collect();
    let result = engine.poll_now();
    assert!(result.skipped.is_empty(), "no hint means no floor");
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    unmount(&second);
}
