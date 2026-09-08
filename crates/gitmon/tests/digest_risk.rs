//! The Summary-v3 halves of a digest: `unreviewed_merges` — what shipped with
//! nobody's review on it — and the per-repo commit concentration that says when
//! a repo is really one person's.
//!
//! Like `digest.rs` and `digest_series.rs`, every event is planted through a
//! mock GitHub server and a real poll, so these are assertions about what the
//! store actually holds rather than about a hand-built fixture.

mod common;

use common::{engine_for, iso8601, mount, server, RepoMock, Resp};
use gitmon::{FilterMode, Settings};

const PLATFORM: &str = "acme/platform";

fn json_array(items: &[String]) -> String {
    format!("[{}]", items.join(","))
}

fn user(login: &str) -> String {
    format!(r#"{{"login":"{login}","avatar_url":null}}"#)
}

fn commit(sha: &str, actor: &str, at: i64) -> String {
    format!(
        r#"{{"sha":"{sha}",
             "html_url":"https://github.com/{PLATFORM}/commit/{sha}",
             "commit":{{"message":"Commit {sha}",
                        "author":{{"name":"{actor}","date":"{date}"}},
                        "committer":{{"name":"{actor}","date":"{date}"}}}},
             "author":{who},"committer":{who}}}"#,
        date = iso8601(at),
        who = user(actor),
    )
}

/// A merged pull request. `updated_at` is the merge, because the poller pages
/// `pulls` by `updated` and stops at the first one older than the window.
fn merged_pull(number: u64, author: &str, created_at: i64, merged_at: i64) -> String {
    format!(
        r#"{{"number":{number},"title":"PR {number}",
             "html_url":"https://github.com/{PLATFORM}/pull/{number}",
             "user":{who},"body":null,"state":"closed","draft":false,
             "created_at":"{created}","updated_at":"{merged}",
             "closed_at":"{merged}","merged_at":"{merged}",
             "merged_by":null,"requested_reviewers":[],
             "head":null,"base":null,
             "additions":null,"deletions":null,"changed_files":null}}"#,
        who = user(author),
        created = iso8601(created_at),
        merged = iso8601(merged_at),
    )
}

/// A pull request that is still open, so a repo can hold PR events without
/// holding a merge.
fn open_pull(number: u64, author: &str, created_at: i64) -> String {
    format!(
        r#"{{"number":{number},"title":"PR {number}",
             "html_url":"https://github.com/{PLATFORM}/pull/{number}",
             "user":{who},"body":null,"state":"open","draft":false,
             "created_at":"{created}","updated_at":"{created}",
             "closed_at":null,"merged_at":null,
             "merged_by":null,"requested_reviewers":[],
             "head":null,"base":null,
             "additions":null,"deletions":null,"changed_files":null}}"#,
        who = user(author),
        created = iso8601(created_at),
    )
}

fn review(number: u64, id: u64, actor: &str, at: i64) -> String {
    format!(
        r#"{{"id":{id},
             "html_url":"https://github.com/{PLATFORM}/pull/{number}#pullrequestreview-{id}",
             "body":null,"user":{who},"state":"APPROVED","submitted_at":"{at}"}}"#,
        who = user(actor),
        at = iso8601(at),
    )
}

fn issue(number: u64, actor: &str, created_at: i64) -> String {
    format!(
        r#"{{"number":{number},"title":"Issue {number}",
             "html_url":"https://github.com/{PLATFORM}/issues/{number}",
             "user":{who},"body":null,"state":"open",
             "created_at":"{created}","pull_request":null}}"#,
        who = user(actor),
        created = iso8601(created_at),
    )
}

/// Everything that has to outlive the test body.
struct Keep {
    _server: mockito::ServerGuard,
    _mocks: Vec<mockito::Mock>,
    _dir: tempfile::TempDir,
}

/// An engine with one repo, polled once against `platform`.
fn polled(platform: RepoMock) -> (gitmon::Engine, Keep) {
    let mut srv = server();
    let mocks = mount(&mut srv, &platform);
    let (engine, dir) = engine_for(&srv);
    engine
        .set_settings(Settings { filter_mode: FilterMode::All, ..Settings::default() })
        .unwrap();
    engine.add_repo(PLATFORM, None).unwrap();
    let polled = engine.poll_now();
    assert!(polled.errors.is_empty(), "the fixture server misfired: {:?}", polled.errors);
    (engine, Keep { _server: srv, _mocks: mocks, _dir: dir })
}

// ---- unreviewed merges --------------------------------------------------

/// A merge counts as unreviewed when the store holds no `pr_reviewed` for it at
/// *any* time up to the merge — so a review from before the window still
/// protects it, one from after the merge does not, and a merge that happened
/// outside the window is none of this window's business.
#[test]
fn unreviewed_merges_counts_only_merges_no_review_preceded() {
    // Twenty-two hours back, so everything below is inside the 24 h first-poll
    // window and still leaves room for a digest window that starts later.
    let base = chrono::Utc::now().timestamp() - 22 * 3600;
    let window_start = base + 1_000;
    let merge = base + 5_000;

    let mut platform = RepoMock::quiet("acme", "platform");
    platform.pulls = Resp::ok(json_array(&[
        // Reviewed *before* the digest window opens, merged inside it.
        merged_pull(201, "alice", base + 100, merge),
        // Reviewed inside the window, merged inside it.
        merged_pull(202, "alice", base + 100, merge),
        // Never reviewed at all.
        merged_pull(203, "alice", base + 100, merge),
        // Never reviewed, but merged before the window opened.
        merged_pull(204, "alice", base + 100, base + 500),
        // Reviewed only *after* it was merged, which is not a review of the
        // merge — somebody rubber-stamped it afterwards.
        merged_pull(205, "alice", base + 100, merge),
    ]));
    platform.reviews = vec![
        (201, Resp::ok(json_array(&[review(201, 9_001, "bob", base + 200)]))),
        (202, Resp::ok(json_array(&[review(202, 9_002, "bob", base + 2_000)]))),
        (203, Resp::json_array()),
        (204, Resp::json_array()),
        (205, Resp::ok(json_array(&[review(205, 9_005, "bob", merge + 600)]))),
    ];
    let (engine, _keep) = polled(platform);

    let digest = engine.digest(window_start, base + 10_000, None, None, 0).unwrap();
    assert_eq!(
        digest.unreviewed_merges, 2,
        "#203 was never reviewed and #205 only after the fact; #201 and #202 were reviewed \
         in time and #204 merged before the window"
    );

    // A window that opens before #204's merge takes it in too.
    let wider = engine.digest(base, base + 10_000, None, None, 0).unwrap();
    assert_eq!(wider.unreviewed_merges, 3, "#204 merged unreviewed as well");

    // A window with no merges in it has nothing to count — zero, not a null,
    // because "no merges went unreviewed" is a true and useful answer.
    let quiet = engine.digest(base + 8_000, base + 10_000, None, None, 0).unwrap();
    assert_eq!(quiet.unreviewed_merges, 0);
}

/// `team_id` and `actor` scope the count by the PR's **author**, the same rule
/// `pr_timing` follows — and a merge whose `pr_opened` was never stored has no
/// author to scope by, so it counts unscoped and drops out the moment a scope
/// is asked for.
#[test]
fn unreviewed_merges_are_scoped_by_the_pull_requests_author() {
    let base = chrono::Utc::now().timestamp() - 22 * 3600;
    let merge = base + 5_000;

    let mut platform = RepoMock::quiet("acme", "platform");
    platform.pulls = Resp::ok(json_array(&[
        merged_pull(301, "alice", base + 100, merge),
        merged_pull(302, "bob", base + 100, merge),
        // Opened two days ago, so the first poll's 24 h window never stores its
        // `pr_opened`; only the merge is in the store.
        merged_pull(303, "alice", base - 48 * 3600, merge),
    ]));
    platform.reviews = vec![
        (301, Resp::json_array()),
        (302, Resp::json_array()),
        (303, Resp::json_array()),
    ];
    let (engine, _keep) = polled(platform);

    let all = engine.digest(base, base + 10_000, None, None, 0).unwrap();
    assert_eq!(all.unreviewed_merges, 3, "every merge in the window, author known or not");

    let hers = engine.digest(base, base + 10_000, None, Some("ALICE"), 0).unwrap();
    assert_eq!(
        hers.unreviewed_merges, 1,
        "#301 is hers; #302 is bob's and #303 has no stored opening to name an author"
    );

    let his = engine.digest(base, base + 10_000, None, Some("bob"), 0).unwrap();
    assert_eq!(his.unreviewed_merges, 1);

    // A team is the same rule over a set of logins.
    let team = engine.create_team("Platform", &["alice".to_string(), "bob".to_string()]).unwrap();
    let theirs = engine.digest(base, base + 10_000, Some(team.id), None, 0).unwrap();
    assert_eq!(theirs.unreviewed_merges, 2, "#301 and #302, but still not #303");

    // An unknown team matches nobody, quietly.
    let nobody = engine.digest(base, base + 10_000, Some(9_999), None, 0).unwrap();
    assert_eq!(nobody.unreviewed_merges, 0);
}

// ---- author concentration ----------------------------------------------

/// The repo's busiest commit author and their share of its commits. Only
/// commits count: the reviews and issues in the same window inflate `total`
/// and leave the share alone.
#[test]
fn a_repos_concentration_is_its_top_commit_authors_share() {
    let base = chrono::Utc::now().timestamp() - 20 * 3600;

    let mut platform = RepoMock::quiet("acme", "platform");
    platform.commits = Resp::ok(json_array(&[
        commit(&format!("{:040x}", 1), "alice", base + 10),
        commit(&format!("{:040x}", 2), "alice", base + 20),
        commit(&format!("{:040x}", 3), "alice", base + 30),
        commit(&format!("{:040x}", 4), "alice", base + 40),
        commit(&format!("{:040x}", 5), "bob", base + 50),
    ]));
    // A PR bob opened and alice reviewed: more events for the repo, and not one
    // of them a commit.
    platform.pulls = Resp::ok(json_array(&[open_pull(401, "bob", base + 60)]));
    platform.reviews = vec![(401, Resp::ok(json_array(&[review(401, 9_401, "alice", base + 70)])))];
    let (engine, _keep) = polled(platform);

    let digest = engine.digest(base, base + 1_000, None, None, 0).unwrap();
    let repo = &digest.repos[0];
    assert_eq!(repo.total, 7, "five commits, one PR opened, one review");
    assert_eq!(repo.top_author_login.as_deref(), Some("alice"));
    assert_eq!(repo.top_author_share, 0.8, "four of the repo's five commits");

    // A window holding only bob's commit flips the answer — the concentration
    // is the window's, like everything else in `repos`.
    let later = engine.digest(base + 45, base + 55, None, None, 0).unwrap();
    assert_eq!(later.repos[0].top_author_login.as_deref(), Some("bob"));
    assert_eq!(later.repos[0].top_author_share, 1.0);
}

/// One author and nobody else is a share of exactly 1.0 — the "this repo is
/// one person's" reading the Summary page exists to surface.
#[test]
fn a_single_author_repo_reads_as_a_share_of_one() {
    let base = chrono::Utc::now().timestamp() - 20 * 3600;

    let mut platform = RepoMock::quiet("acme", "platform");
    platform.commits = Resp::ok(json_array(
        &(1..=6)
            .map(|n| commit(&format!("{n:040x}"), "solo", base + n * 10))
            .collect::<Vec<_>>(),
    ));
    let (engine, _keep) = polled(platform);

    let digest = engine.digest(base, base + 1_000, None, None, 0).unwrap();
    assert_eq!(digest.repos[0].top_author_login.as_deref(), Some("solo"));
    assert_eq!(digest.repos[0].top_author_share, 1.0);
}

/// Two authors level on commits: the lower login wins, so the same window
/// always gives the same answer instead of whichever the hash map yielded.
#[test]
fn a_tied_concentration_goes_to_the_lower_login() {
    let base = chrono::Utc::now().timestamp() - 20 * 3600;

    let mut platform = RepoMock::quiet("acme", "platform");
    // Emitted with the *later* login first, so insertion order cannot break
    // the tie the right way by accident.
    platform.commits = Resp::ok(json_array(&[
        commit(&format!("{:040x}", 1), "zoe", base + 10),
        commit(&format!("{:040x}", 2), "zoe", base + 20),
        commit(&format!("{:040x}", 3), "adam", base + 30),
        commit(&format!("{:040x}", 4), "adam", base + 40),
    ]));
    let (engine, _keep) = polled(platform);

    let digest = engine.digest(base, base + 1_000, None, None, 0).unwrap();
    assert_eq!(digest.repos[0].top_author_login.as_deref(), Some("adam"));
    assert_eq!(digest.repos[0].top_author_share, 0.5);

    // And it is stable: the same window asked twice gives the same author.
    let again = engine.digest(base, base + 1_000, None, None, 0).unwrap();
    assert_eq!(again.repos[0].top_author_login, digest.repos[0].top_author_login);
}

/// A repo whose window holds no commits at all names nobody. It still appears
/// in `repos` — it has events — but with `None` and a zero share rather than a
/// reviewer promoted to author.
#[test]
fn a_repo_with_no_commits_in_the_window_names_nobody() {
    let base = chrono::Utc::now().timestamp() - 20 * 3600;

    let mut platform = RepoMock::quiet("acme", "platform");
    platform.pulls = Resp::ok(json_array(&[open_pull(501, "bob", base + 10)]));
    platform.reviews = vec![(501, Resp::ok(json_array(&[review(501, 9_501, "alice", base + 20)])))];
    platform.issues = Resp::ok(json_array(&[issue(502, "carol", base + 30)]));
    let (engine, _keep) = polled(platform);

    let digest = engine.digest(base, base + 1_000, None, None, 0).unwrap();
    let repo = &digest.repos[0];
    assert!(repo.total > 0, "the repo is in `repos` because it has events");
    assert_eq!(repo.top_author_login, None);
    assert_eq!(repo.top_author_share, 0.0);
}
