//! `pulls`: the PR state the poller records, and `open_pulls` over it.
//!
//! Everything here is planted through a mock GitHub server and a real poll, so
//! what the table holds is what the poller actually wrote — not a hand-made
//! approximation of it. No test mounts a `pulls` endpoint of its own: the whole
//! point of the table is that it costs no extra request, and it is filled in
//! from the very listing the poller already fetches for events.

mod common;

use common::{engine_for, iso8601, mount, server, unmount, RepoMock, Resp};
use gitmon::{FilterMode, Settings};

const PLATFORM: &str = "acme/platform";

fn json_array(items: &[String]) -> String {
    format!("[{}]", items.join(","))
}

fn user(login: &str) -> String {
    format!(r#"{{"login":"{login}","avatar_url":"https://avatars.example/{login}.png"}}"#)
}

/// One PR exactly as GitHub's list endpoint describes it — `additions` and
/// `deletions` absent, because "Pull Request Simple" carries neither.
#[allow(clippy::too_many_arguments)]
fn pull(
    number: u64,
    author: &str,
    title: &str,
    created_at: i64,
    updated_at: i64,
    merged_at: Option<i64>,
    draft: bool,
    reviewers: &[&str],
) -> String {
    let stamp = |at: Option<i64>| match at {
        Some(at) => format!("\"{}\"", iso8601(at)),
        None => "null".to_string(),
    };
    let state = if merged_at.is_some() { "closed" } else { "open" };
    let requested: Vec<String> = reviewers.iter().map(|r| user(r)).collect();
    format!(
        r#"{{"number":{number},"title":"{title}",
             "html_url":"https://github.com/{PLATFORM}/pull/{number}",
             "user":{who},"body":null,"state":"{state}","draft":{draft},
             "created_at":"{created}","updated_at":"{updated}",
             "closed_at":{closed},"merged_at":{merged},
             "merged_by":null,"requested_reviewers":[{requested}],
             "head":null,"base":null,
             "additions":null,"deletions":null,"changed_files":null}}"#,
        who = user(author),
        created = iso8601(created_at),
        updated = iso8601(updated_at),
        closed = stamp(merged_at),
        merged = stamp(merged_at),
        requested = requested.join(","),
    )
}

fn review(number: u64, id: u64, actor: &str, at: i64, state: &str) -> String {
    format!(
        r#"{{"id":{id},
             "html_url":"https://github.com/{PLATFORM}/pull/{number}#pullrequestreview-{id}",
             "body":null,"user":{who},"state":"{state}","submitted_at":"{at}"}}"#,
        who = user(actor),
        at = iso8601(at),
    )
}

/// An engine with one repo whose PR listing is `pulls`, already polled once.
fn polled(pulls: &[String], reviews: Vec<(u64, Resp)>) -> (gitmon::Engine, u64, Scenery) {
    let mut srv = server();
    let mut platform = RepoMock::quiet("acme", "platform");
    platform.pulls = Resp::ok(json_array(pulls));
    platform.reviews = reviews;
    let mocks = mount(&mut srv, &platform);

    let (engine, dir) = engine_for(&srv);
    // `all`, so the ingestion filter never stands between the fixture and the
    // store: these tests are about the PR rows, not about who is on a team.
    engine
        .set_settings(Settings { filter_mode: FilterMode::All, ..Settings::default() })
        .unwrap();
    let repo_id = engine.add_repo(PLATFORM, None).unwrap().id;
    let polled = engine.poll_now();
    assert!(polled.errors.is_empty(), "the fixture server misfired: {:?}", polled.errors);
    (engine, repo_id, Scenery { server: srv, mocks, _dir: dir })
}

/// The mock server and its mounts, kept alive for the length of a test.
struct Scenery {
    server: mockito::ServerGuard,
    mocks: Vec<mockito::Mock>,
    _dir: tempfile::TempDir,
}

fn base() -> i64 {
    chrono::Utc::now().timestamp() - 3600
}

/// Every pull request the listing carried lands in the table, with the facts
/// the contract's `OpenPull` names — and none of it costs a request of its own.
#[test]
fn a_poll_records_every_pull_request_it_lists() {
    let base = base();
    let (engine, repo_id, _s) = polled(
        &[
            pull(103, "carol", "Draft: sharding", base + 30, base + 40, None, true, &[]),
            pull(102, "bob", "Bundle rusqlite", base + 20, base + 35, Some(base + 35), false, &[]),
            pull(101, "alice", "Split the poller", base + 10, base + 25, None, false, &["bob"]),
        ],
        vec![(101, Resp::json_array()), (102, Resp::json_array()), (103, Resp::json_array())],
    );

    let open = engine.open_pulls(None, None, None).unwrap();
    assert_eq!(
        open.iter().map(|p| p.number).collect::<Vec<_>>(),
        vec![101, 103],
        "the merged one is not open, and the rest are oldest first"
    );

    let first = &open[0];
    assert_eq!(first.repo_id, repo_id);
    assert_eq!(first.title, "Split the poller");
    assert_eq!(first.url, format!("https://github.com/{PLATFORM}/pull/101"));
    assert_eq!(first.author_login, "alice");
    assert_eq!(
        first.author_avatar_url.as_deref(),
        Some("https://avatars.example/alice.png")
    );
    assert!(!first.draft);
    assert_eq!(first.created_at, base + 10);
    assert_eq!(first.updated_at, base + 25);
    assert_eq!(first.requested_reviewers, vec!["bob".to_string()]);
    assert_eq!(
        first.review_decision.as_deref(),
        Some("review_required"),
        "bob was asked and has not answered"
    );
    assert_eq!(
        (first.additions, first.deletions),
        (None, None),
        "the list shape carries no diff stats, and none are invented"
    );
    // `pr_opened` for #101 landed at `created_at`, which is later than
    // GitHub's own `updated_at` for nothing here — so the newest of the two is
    // the listing's.
    assert_eq!(first.last_activity_at, base + 25);

    assert!(open[1].draft, "#103 is a draft");
    assert_eq!(open[1].review_decision, None, "nobody asked, nobody reviewed");
}

/// A review that arrived with the same poll is already reflected in the PR's
/// `review_decision` — the events are stored before the PR rows are written,
/// so the decision never lags a poll behind.
#[test]
fn a_review_in_the_same_poll_decides_the_pull_request() {
    let base = base();
    let (engine, _repo_id, _s) = polled(
        &[pull(101, "alice", "Split the poller", base + 10, base + 50, None, false, &["bob"])],
        vec![(
            101,
            Resp::ok(json_array(&[review(101, 9001, "bob", base + 40, "CHANGES_REQUESTED")])),
        )],
    );

    let open = engine.open_pulls(None, None, None).unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].review_decision.as_deref(), Some("changes_requested"));
    assert_eq!(
        open[0].last_activity_at,
        base + 50,
        "GitHub's updated_at is newer than the review that came with it"
    );
}

/// The state transition the app's open-PR count depends on: a PR listed open on
/// one poll and merged on the next leaves `open_pulls`, and leaves one row
/// behind rather than two.
#[test]
fn a_later_poll_moves_a_merged_pull_request_out_of_open_pulls() {
    let base = base();
    let (engine, _repo_id, mut s) = polled(
        &[pull(101, "alice", "Split the poller", base + 10, base + 25, None, false, &["bob"])],
        vec![(101, Resp::json_array())],
    );
    assert_eq!(engine.open_pulls(None, None, None).unwrap().len(), 1);

    // Re-point the PR listing at the merged shape. The old mocks have to be
    // unmounted first, or mockito may keep answering from them.
    unmount(&s.mocks);
    let mut merged = RepoMock::quiet("acme", "platform");
    merged.pulls = Resp::ok(json_array(&[pull(
        101,
        "alice",
        "Split the poller",
        base + 10,
        base + 90,
        Some(base + 90),
        false,
        &[],
    )]));
    merged.reviews = vec![(101, Resp::json_array())];
    s.mocks = mount(&mut s.server, &merged);

    // A rescan so the second poll's window reaches the fixture again.
    engine.rescan(None).unwrap();
    let polled = engine.poll_now();
    assert!(polled.errors.is_empty(), "the second fixture misfired: {:?}", polled.errors);

    assert!(
        engine.open_pulls(None, None, None).unwrap().is_empty(),
        "the merged PR is no longer open"
    );
}

/// `open_pulls` narrows by the author's team, by one author, and by who is
/// being asked to review — the three intersecting, and none of them caring
/// about the casing of a login.
#[test]
fn open_pulls_scopes_by_team_author_and_reviewer() {
    let base = base();
    let (engine, _repo_id, _s) = polled(
        &[
            pull(103, "carol", "Third", base + 30, base + 40, None, false, &["alice"]),
            pull(102, "bob", "Second", base + 20, base + 35, None, false, &["Alice", "carol"]),
            pull(101, "alice", "First", base + 10, base + 25, None, false, &["bob"]),
        ],
        vec![(101, Resp::json_array()), (102, Resp::json_array()), (103, Resp::json_array())],
    );

    let numbers = |pulls: Vec<gitmon::OpenPull>| -> Vec<u64> {
        pulls.into_iter().map(|p| p.number).collect()
    };

    assert_eq!(numbers(engine.open_pulls(None, None, None).unwrap()), vec![101, 102, 103]);

    // By author, case-insensitively.
    assert_eq!(numbers(engine.open_pulls(None, Some("BOB"), None).unwrap()), vec![102]);

    // By team: the author has to be a current member.
    let platform = engine
        .create_team("Platform", &["alice".to_string(), "bob".to_string()])
        .unwrap();
    assert_eq!(
        numbers(engine.open_pulls(Some(platform.id), None, None).unwrap()),
        vec![101, 102],
        "carol is not on the team"
    );

    // By reviewer: the PRs asking one login for a review, whatever the casing
    // GitHub listed them under.
    assert_eq!(numbers(engine.open_pulls(None, None, Some("alice")).unwrap()), vec![102, 103]);

    // All three intersect.
    assert_eq!(
        numbers(engine.open_pulls(Some(platform.id), Some("bob"), Some("ALICE")).unwrap()),
        vec![102],
    );
    assert!(engine
        .open_pulls(Some(platform.id), Some("carol"), None)
        .unwrap()
        .is_empty());

    // An unknown team matches nobody, quietly — the same answer `digest` gives.
    assert!(engine.open_pulls(Some(999_999), None, None).unwrap().is_empty());
}

/// Removing a repo forgets its pull requests, so an open PR can never outlive
/// the repo it belongs to and go on being counted.
#[test]
fn removing_a_repo_forgets_its_pull_requests() {
    let base = base();
    let (engine, repo_id, _s) = polled(
        &[pull(101, "alice", "Split the poller", base + 10, base + 25, None, false, &[])],
        vec![(101, Resp::json_array())],
    );
    assert_eq!(engine.open_pulls(None, None, None).unwrap().len(), 1);

    engine.remove_repo(repo_id).unwrap();

    assert!(engine.open_pulls(None, None, None).unwrap().is_empty());
}

/// A backfill fetches the same PR listing a poll does, so it records the PR
/// rows too — a repo added today knows the state of a PR opened last month
/// without a request of its own.
#[test]
fn a_backfill_records_the_pull_requests_it_sees() {
    let base = base();
    let mut srv = server();
    let mut platform = RepoMock::quiet("acme", "platform");
    let mocks = mount(&mut srv, &platform);
    let (engine, dir) = engine_for(&srv);
    engine
        .set_settings(Settings { filter_mode: FilterMode::All, ..Settings::default() })
        .unwrap();
    engine.add_repo(PLATFORM, None).unwrap();
    // The first poll sees nothing, which is what gives backfill a floor to
    // step down from — and leaves `pulls` genuinely empty before the backfill.
    assert!(engine.poll_now().errors.is_empty());
    assert!(engine.open_pulls(None, None, None).unwrap().is_empty());

    unmount(&mocks);
    platform.pulls = Resp::ok(json_array(&[pull(
        101,
        "alice",
        "Split the poller",
        base - 100_000,
        base - 90_000,
        None,
        false,
        &["bob"],
    )]));
    platform.reviews = vec![(101, Resp::json_array())];
    let _mocks = mount(&mut srv, &platform);

    let result = engine.backfill(None, 7 * 24 * 3600).unwrap();
    assert!(result.repos.iter().all(|r| r.error.is_none()), "backfill failed: {result:?}");

    let open = engine.open_pulls(None, None, None).unwrap();
    assert_eq!(open.len(), 1, "the backfill recorded the PR it listed");
    assert_eq!(open[0].number, 101);
    assert_eq!(open[0].review_decision.as_deref(), Some("review_required"));
    drop(dir);
}
