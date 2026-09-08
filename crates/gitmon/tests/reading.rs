//! In-app reading: `get_thread`, `get_commit` and `get_pull_files`, per the
//! "Reading in-app" section of `docs/CONTRACT.md`.
//!
//! Every fixture here uses fixed epoch timestamps rather than `{{TS-n}}`, because
//! these calls are live fetches with no poll window to stay inside.

mod common;

use common::{engine_for, fixture, mock_get, repo_meta, server, Resp};
use gitmon::{ErrorKind, ThreadItemKind, ThreadKind};
use mockito::{Matcher, Mock, ServerGuard};

/// The sha of the single commit in `thread_pull_commits.json`, which is also
/// the first file's blob sha in `pull_files_page1.json`.
const COMMIT_SHA: &str = "aaa1111111111111111111111111111111111111";

/// Registers the repo lookup and adds `acme/platform`, returning its id.
fn engine_with_repo(srv: &mut ServerGuard) -> (gitmon::Engine, tempfile::TempDir, u64, Mock) {
    let meta = mock_get(srv, "/repos/acme/platform", &Resp::ok(repo_meta("acme", "platform")));
    let (engine, dir) = engine_for(srv);
    let repo = engine.add_repo("acme/platform", None).expect("repo added");
    (engine, dir, repo.id, meta)
}

/// The four item sources of a PR thread, each on its own endpoint.
fn mount_pull_thread(srv: &mut ServerGuard) -> Vec<Mock> {
    vec![
        mock_get(srv, "/repos/acme/platform/pulls/101", &Resp::ok(fixture("thread_pull_101.json"))),
        mock_get(
            srv,
            "/repos/acme/platform/issues/101/comments",
            &Resp::ok(fixture("thread_comments.json")),
        ),
        mock_get(
            srv,
            "/repos/acme/platform/pulls/101/reviews",
            &Resp::ok(fixture("thread_reviews.json")),
        ),
        mock_get(
            srv,
            "/repos/acme/platform/pulls/101/comments",
            &Resp::ok(fixture("thread_review_comments.json")),
        ),
        mock_get(
            srv,
            "/repos/acme/platform/pulls/101/commits",
            &Resp::ok(fixture("thread_pull_commits.json")),
        ),
    ]
}

#[test]
fn a_pull_thread_merges_all_four_item_sources_in_time_order() {
    let mut srv = server();
    let _mocks = mount_pull_thread(&mut srv);
    let (engine, _dir, repo_id, _meta) = engine_with_repo(&mut srv);

    let thread = engine.get_thread(repo_id, 101).expect("thread fetched");

    assert_eq!(thread.repo_id, repo_id);
    assert_eq!(thread.number, 101);
    assert_eq!(thread.kind, ThreadKind::Pull);
    assert_eq!(thread.title, "Split the poller out of the engine");
    assert_eq!(thread.state, "open");
    assert_eq!(thread.author_login, "alice");
    assert_eq!(
        thread.body.as_deref(),
        Some("Moves fetch/derive into its own module\n\nso the store lock is never held across a network call."),
        "the PR body is the full markdown, not a preview"
    );
    assert_eq!(thread.created_at, 10);
    // Branch names and the diff stat come from the PR and nowhere else.
    assert_eq!(thread.head_ref.as_deref(), Some("split-poller"));
    assert_eq!(thread.base_ref.as_deref(), Some("main"));
    assert_eq!(thread.additions, Some(120));
    assert_eq!(thread.deletions, Some(30));
    assert_eq!(thread.changed_files, Some(4));

    // Chronological, oldest first, across commit / comment / review /
    // review_comment — interleaved, not grouped by source.
    let order: Vec<(ThreadItemKind, i64)> =
        thread.items.iter().map(|i| (i.kind, i.at)).collect();
    assert_eq!(
        order,
        vec![
            (ThreadItemKind::Commit, 15),
            (ThreadItemKind::Comment, 20),
            (ThreadItemKind::Review, 30),
            (ThreadItemKind::ReviewComment, 40),
            (ThreadItemKind::ReviewComment, 50),
        ]
    );

    let commit = &thread.items[0];
    assert_eq!(commit.actor_login, "alice");
    assert_eq!(commit.body.as_deref(), Some("First: the commit\n\nwith a body."));
    assert_eq!(commit.state, None);
    // The commit item carries the sha the app feeds straight to `get_commit`;
    // nothing else in the thread does.
    assert_eq!(commit.sha.as_deref(), Some(COMMIT_SHA), "a commit item must carry its sha");
    for item in thread.items.iter().filter(|i| i.kind != ThreadItemKind::Commit) {
        assert_eq!(item.sha, None, "only a commit item has a sha: {item:?}");
    }

    let comment = &thread.items[1];
    assert_eq!(comment.actor_login, "bob");
    assert_eq!(comment.body.as_deref(), Some("Second: the comment."));

    // A review carries its state; a review comment carries path and line.
    let review = &thread.items[2];
    assert_eq!(review.state.as_deref(), Some("APPROVED"));
    assert_eq!(review.path, None);
    assert_eq!(review.line, None);

    let review_comment = &thread.items[3];
    assert_eq!(review_comment.state, None);
    assert_eq!(review_comment.path.as_deref(), Some("src/poll.rs"));
    assert_eq!(review_comment.line, Some(42));
    assert_eq!(review_comment.actor_avatar_url, None);

    // An outdated comment keeps the line it was written against.
    let outdated = &thread.items[4];
    assert_eq!(outdated.path.as_deref(), Some("src/store.rs"));
    assert_eq!(outdated.line, Some(17), "falls back to original_line");

    // The pending review in the fixture is visible to nobody and is not an item.
    assert!(
        !thread.items.iter().any(|i| i.actor_login == "carol" && i.kind == ThreadItemKind::Review),
        "a pending review must not appear in the thread"
    );
}

#[test]
fn a_number_that_is_not_a_pull_request_falls_back_to_the_issue() {
    let mut srv = server();
    // `/pulls/5` 404s: number 5 is a plain issue, and that is the only way to
    // tell the two apart by number.
    let pulls = mock_get(
        &mut srv,
        "/repos/acme/platform/pulls/5",
        &Resp::status(404, fixture("error_not_found.json")),
    );
    let issue =
        mock_get(&mut srv, "/repos/acme/platform/issues/5", &Resp::ok(fixture("thread_issue_5.json")));
    let comments = mock_get(
        &mut srv,
        "/repos/acme/platform/issues/5/comments",
        &Resp::ok(fixture("thread_comments.json")),
    );
    let (engine, _dir, repo_id, _meta) = engine_with_repo(&mut srv);

    let thread = engine.get_thread(repo_id, 5).expect("issue thread fetched");

    pulls.assert();
    issue.assert();
    comments.assert();
    assert_eq!(thread.kind, ThreadKind::Issue);
    assert_eq!(thread.title, "Tray icon is blurry on non-retina displays");
    assert_eq!(thread.state, "closed");
    assert_eq!(thread.author_login, "bob");
    // An issue has no branches and no diff stat.
    assert_eq!(thread.head_ref, None);
    assert_eq!(thread.base_ref, None);
    assert_eq!(thread.additions, None);
    assert_eq!(thread.deletions, None);
    assert_eq!(thread.changed_files, None);
    assert_eq!(thread.items.len(), 1);
    assert_eq!(thread.items[0].kind, ThreadItemKind::Comment);
}

#[test]
fn a_commit_carries_its_files_with_and_without_a_patch() {
    let mut srv = server();
    let sha = "c0ffee1111111111111111111111111111111111";
    let _mock = mock_get(
        &mut srv,
        &format!("/repos/acme/platform/commits/{sha}"),
        &Resp::ok(fixture("commit_detail.json")),
    );
    let (engine, _dir, repo_id, _meta) = engine_with_repo(&mut srv);

    let commit = engine.get_commit(repo_id, sha).expect("commit fetched");

    assert_eq!(commit.repo_id, repo_id);
    assert_eq!(commit.sha, sha);
    assert_eq!(
        commit.message,
        "Add sparkline to the tray menu\n\nKeeps the last hour of activity in the menu bar.",
        "the whole message, subject line included"
    );
    assert_eq!(commit.author_login, "alice");
    assert_eq!(commit.at, 60);
    assert_eq!(commit.additions, 104);
    assert_eq!(commit.deletions, 4);
    assert_eq!(commit.files.len(), 2);

    let text = &commit.files[0];
    assert_eq!(text.path, "src/tray.rs", "GitHub's `filename` maps to the contract's `path`");
    assert_eq!(text.status, "modified");
    assert_eq!(text.additions, 100);
    assert_eq!(text.deletions, 4);
    assert_eq!(text.patch.as_deref(), Some("@@ -29,7 +29,7 @@\n-old\n+new"));

    // A binary file: GitHub omits `patch` entirely and it must land as null.
    let binary = &commit.files[1];
    assert_eq!(binary.path, "assets/sparkline.png");
    assert_eq!(binary.status, "added");
    assert_eq!(binary.patch, None);
}

#[test]
fn a_second_read_inside_the_window_makes_no_request() {
    let mut srv = server();
    let sha = "c0ffee1111111111111111111111111111111111";
    // `expect(1)`: a cache miss would make this a second hit and fail the assert.
    let commit_mock = srv
        .mock("GET", format!("/repos/acme/platform/commits/{sha}").as_str())
        .match_query(Matcher::Any)
        .expect(1)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(fixture("commit_detail.json"))
        .create();
    let pull_mocks: Vec<Mock> = mount_pull_thread(&mut srv)
        .into_iter()
        .map(|m| m.expect(1))
        .collect();
    let (engine, _dir, repo_id, _meta) = engine_with_repo(&mut srv);

    let first_commit = engine.get_commit(repo_id, sha).unwrap();
    let second_commit = engine.get_commit(repo_id, sha).unwrap();
    assert_eq!(first_commit, second_commit, "the cached value is the same value");
    commit_mock.assert();

    let first_thread = engine.get_thread(repo_id, 101).unwrap();
    let second_thread = engine.get_thread(repo_id, 101).unwrap();
    assert_eq!(first_thread, second_thread);
    for mock in &pull_mocks {
        mock.assert();
    }

    // A different key is a different entry, so it does fetch.
    let other = engine.get_commit(repo_id, "deadbeef");
    assert!(other.is_err(), "an unmocked sha is not served from another sha's cache entry");
}

#[test]
fn reading_an_unwatched_repo_is_not_found_without_a_request() {
    let srv = server();
    let (engine, _dir) = engine_for(&srv);
    // No repo was ever added, so there is no slug to build a URL from.
    let err = engine.get_thread(404, 1).unwrap_err();
    assert_eq!(err.kind, ErrorKind::NotFound);
    let err = engine.get_commit(404, "c0ffee").unwrap_err();
    assert_eq!(err.kind, ErrorKind::NotFound);
}

#[test]
fn a_blank_sha_is_invalid() {
    let srv = server();
    let (engine, _dir) = engine_for(&srv);
    let err = engine.get_commit(1, "   ").unwrap_err();
    assert_eq!(err.kind, ErrorKind::Invalid);
}

/// `get_pull_files` walks `Link` to the last page, and keeps a null `patch`
/// null rather than turning it into an empty diff.
#[test]
fn pull_files_follow_link_pagination_and_keep_a_null_patch() {
    let mut srv = server();
    let base = srv.url();
    // Page 1 is the request the engine builds: `per_page=100`, no `page`.
    let page_one = srv
        .mock("GET", "/repos/acme/platform/pulls/101/files")
        .match_query(Matcher::UrlEncoded("per_page".into(), "100".into()))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_header(
            "link",
            &format!("<{base}/repos/acme/platform/pulls/101/files?page=2>; rel=\"next\""),
        )
        .with_body(fixture("pull_files_page1.json"))
        .create();
    // Page 2 is the URL off that header, which carries `page` and nothing else.
    let page_two = srv
        .mock("GET", "/repos/acme/platform/pulls/101/files")
        .match_query(Matcher::UrlEncoded("page".into(), "2".into()))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(fixture("pull_files_page2.json"))
        .create();
    let (engine, _dir, repo_id, _meta) = engine_with_repo(&mut srv);

    let files = engine.get_pull_files(repo_id, 101).expect("files fetched");

    page_one.assert();
    page_two.assert();
    assert_eq!(
        files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
        vec!["crates/gitmon/src/poll.rs", "docs/design/feed.png", "crates/gitmon/src/store.rs"],
        "both pages, in order"
    );

    let text = &files[0];
    assert_eq!(text.status, "modified");
    assert_eq!(text.additions, 40);
    assert_eq!(text.deletions, 12);
    assert_eq!(
        text.patch.as_deref(),
        Some("@@ -18,7 +18,7 @@\n-const OVERLAP: i64 = 60;\n+pub const OVERLAP_SECS: i64 = 300;")
    );

    // A binary file: GitHub omits `patch` entirely and it must land as null, so
    // the app can show "too large to show here" instead of a blank diff.
    let binary = &files[1];
    assert_eq!(binary.path, "docs/design/feed.png");
    assert_eq!(binary.status, "added");
    assert_eq!(binary.patch, None);

    // The second page's entry survived the merge whole.
    let renamed = &files[2];
    assert_eq!(renamed.status, "renamed");
    assert_eq!(renamed.patch.as_deref(), Some("@@ -1,3 +1,3 @@\n-mod db;\n+mod store;"));
}

/// The same 60-second cache as `get_thread` and `get_commit`, keyed by its own
/// arguments: a second read inside the window makes no request.
#[test]
fn a_second_pull_files_read_inside_the_window_makes_no_request() {
    let mut srv = server();
    // `expect(1)`: a cache miss would make this a second hit and fail the assert.
    let files_mock = srv
        .mock("GET", "/repos/acme/platform/pulls/101/files")
        .match_query(Matcher::Any)
        .expect(1)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(fixture("pull_files_page1.json"))
        .create();
    let (engine, _dir, repo_id, _meta) = engine_with_repo(&mut srv);

    let first = engine.get_pull_files(repo_id, 101).unwrap();
    let second = engine.get_pull_files(repo_id, 101).unwrap();
    assert_eq!(first, second, "the cached value is the same value");
    assert_eq!(first.len(), 2);
    files_mock.assert();

    // A different number is a different entry, so it does fetch — and misses,
    // proving the cache is keyed on the arguments and not on the repo alone.
    assert!(
        engine.get_pull_files(repo_id, 999).is_err(),
        "an unmocked PR is not served from another PR's cache entry"
    );
}

#[test]
fn pull_files_for_an_unwatched_repo_is_not_found_without_a_request() {
    let srv = server();
    let (engine, _dir) = engine_for(&srv);
    let err = engine.get_pull_files(404, 1).unwrap_err();
    assert_eq!(err.kind, ErrorKind::NotFound);
}
