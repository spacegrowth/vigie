//! `list_events`: ordering, clamping, and keyset pagination via `before_id`.

mod common;

use common::{engine_for, iso8601, mount, server, RepoMock, Resp};
use gitmon::{FilterMode, Settings};
use std::collections::HashSet;

/// Thirty commits, in groups of three sharing one timestamp, so paging has to
/// break ties on id rather than time alone.
fn thirty_commits() -> String {
    let now = chrono::Utc::now().timestamp();
    let commits: Vec<String> = (0..30)
        .map(|i| {
            let date = iso8601(now - (i / 3) * 600 - 60);
            format!(
                r#"{{"sha":"{sha:040x}",
                     "html_url":"https://github.com/acme/platform/commit/{sha:040x}",
                     "commit":{{"message":"Commit {i}",
                                "author":{{"name":"Alice Ng","date":"{date}"}},
                                "committer":{{"name":"Alice Ng","date":"{date}"}}}},
                     "author":{{"login":"alice","id":1,"avatar_url":null}},
                     "committer":{{"login":"alice","id":1,"avatar_url":null}}}}"#,
                sha = i + 1,
                i = i,
                date = date
            )
        })
        .collect();
    format!("[{}]", commits.join(","))
}

/// 8. Walking a 30-event set in pages of 10 visits every event exactly once.
#[test]
fn before_id_walks_every_event_with_no_gaps_or_repeats() {
    let mut srv = server();
    let mut repo = RepoMock::quiet("acme", "platform");
    repo.commits = Resp::ok(thirty_commits());
    let _mocks = mount(&mut srv, &repo);
    let (engine, _dir) = engine_for(&srv);
    engine
        .set_settings(Settings { filter_mode: FilterMode::All, ..Settings::default() })
        .unwrap();
    engine.add_repo("acme/platform", None).unwrap();
    assert_eq!(engine.poll_now().new_events.len(), 30);

    let everything = engine.list_events(None, None, None, FilterMode::All, false, None, 500);
    assert_eq!(everything.len(), 30);

    let mut pages = Vec::new();
    let mut walked: Vec<u64> = Vec::new();
    let mut cursor: Option<u64> = None;
    loop {
        let page = engine.list_events(None, None, None, FilterMode::All, false, cursor, 10);
        if page.is_empty() {
            break;
        }
        pages.push(page.len());
        cursor = Some(page.last().unwrap().id);
        walked.extend(page.iter().map(|e| e.id));
        assert!(pages.len() <= 5, "pagination did not terminate: {pages:?}");
    }

    assert_eq!(pages, vec![10, 10, 10], "three full pages, then nothing");
    let unique: HashSet<u64> = walked.iter().copied().collect();
    assert_eq!(unique.len(), 30, "an event was visited twice: {walked:?}");
    assert_eq!(
        walked,
        everything.iter().map(|e| e.id).collect::<Vec<u64>>(),
        "paging must yield the same order as one unpaged read"
    );
}

#[test]
fn events_are_newest_first_and_the_limit_is_clamped() {
    let mut srv = server();
    let mut repo = RepoMock::quiet("acme", "platform");
    repo.commits = Resp::ok(thirty_commits());
    let _mocks = mount(&mut srv, &repo);
    let (engine, _dir) = engine_for(&srv);
    engine
        .set_settings(Settings { filter_mode: FilterMode::All, ..Settings::default() })
        .unwrap();
    engine.add_repo("acme/platform", None).unwrap();
    engine.poll_now();

    // An absurd limit is clamped, not rejected.
    let events = engine.list_events(None, None, None, FilterMode::All, false, None, u32::MAX);
    assert_eq!(events.len(), 30);
    assert!(events.len() as u32 <= gitmon::MAX_EVENT_LIMIT);

    for pair in events.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        assert!(
            (a.occurred_at, a.id) > (b.occurred_at, b.id),
            "order broke at {:?} then {:?}",
            (a.occurred_at, a.id),
            (b.occurred_at, b.id)
        );
    }
}

#[test]
fn seen_flags_and_filters_narrow_the_list() {
    let mut srv = server();
    let _mocks = mount(&mut srv, &RepoMock::busy("acme", "platform"));
    let (engine, _dir) = engine_for(&srv);
    engine
        .set_settings(Settings { filter_mode: FilterMode::All, ..Settings::default() })
        .unwrap();
    let repo = engine.add_repo("acme/platform", None).unwrap();
    engine.poll_now();

    // The actor filter is case-insensitive.
    let lower = engine.list_events(None, Some("bob"), None, FilterMode::All, false, None, 500);
    let upper = engine.list_events(None, Some("BOB"), None, FilterMode::All, false, None, 500);
    assert!(!lower.is_empty());
    assert_eq!(lower.len(), upper.len());
    assert!(lower.iter().all(|e| e.actor_login == "bob"));

    assert_eq!(engine.list_events(Some(repo.id), None, None, FilterMode::All, false, None, 500).len(), 12);
    assert!(engine.list_events(Some(repo.id + 999), None, None, FilterMode::All, false, None, 500).is_empty());

    let ids: Vec<u64> = lower.iter().map(|e| e.id).collect();
    let before = engine.unseen_count();
    engine.mark_seen(&ids).unwrap();
    assert_eq!(engine.unseen_count(), before - ids.len() as u64);
    // Marking the same events again is idempotent.
    engine.mark_seen(&ids).unwrap();
    assert_eq!(engine.unseen_count(), before - ids.len() as u64);
}

#[test]
fn removing_a_repo_removes_its_events() {
    let mut srv = server();
    let _mocks = mount(&mut srv, &RepoMock::busy("acme", "platform"));
    let (engine, _dir) = engine_for(&srv);
    engine
        .set_settings(Settings { filter_mode: FilterMode::All, ..Settings::default() })
        .unwrap();
    let repo = engine.add_repo("acme/platform", None).unwrap();
    engine.poll_now();
    assert_eq!(engine.list_events(None, None, None, FilterMode::All, false, None, 500).len(), 12);

    engine.remove_repo(repo.id).unwrap();
    assert!(engine.list_repos().is_empty());
    assert!(engine.list_events(None, None, None, FilterMode::All, false, None, 500).is_empty());
    assert_eq!(engine.unseen_count(), 0);
}
