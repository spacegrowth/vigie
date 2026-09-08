//! `list_events`'s `repo_id` scoping and its "My team / Everyone" view filter
//! (`mode: FilterMode`) — packet gm-feed-r1. Ingestion no longer drops
//! anybody (see `polling.rs` and `watches.rs` for that half of the packet);
//! this file is about what a *read* narrows to, and that every filter
//! composes with the others rather than replacing them.

mod common;

use common::{engine_for, mount, server, RepoMock};
use gitmon::FilterMode;

/// `repo_id` narrows the feed to one repo and composes with `team_id`: two
/// repos with the same two actors each, and scoping to one repo plus one team
/// keeps only that team's member's events in that repo — not the same
/// person's events in the other repo, and not the other actor's in this one.
#[test]
fn repo_id_narrows_the_feed_and_composes_with_team_id() {
    let mut srv = server();
    let _platform = mount(&mut srv, &RepoMock::busy("acme", "platform"));
    let _tools = mount(&mut srv, &RepoMock::busy("acme", "tools"));
    let (engine, _dir) = engine_for(&srv);
    let platform_repo = engine.add_repo("acme/platform", None).unwrap();
    let tools_repo = engine.add_repo("acme/tools", None).unwrap();
    let team = engine.create_team("Platform", &["alice".to_string()]).unwrap();

    let result = engine.poll_now();
    assert!(result.errors.is_empty(), "unexpected errors: {:?}", result.errors);
    assert_eq!(result.new_events.len(), 24, "12 events per repo");

    // repo_id alone: exactly one repo's dozen, both actors.
    let platform_only =
        engine.list_events(Some(platform_repo.id), None, None, FilterMode::All, false, None, 500);
    assert_eq!(platform_only.len(), 12, "{platform_only:#?}");
    assert!(platform_only.iter().all(|e| e.repo_id == platform_repo.id));

    // repo_id AND team_id together: alice's events in the platform repo only —
    // not her events in acme/tools, and not bob's in acme/platform.
    let scoped = engine.list_events(
        Some(platform_repo.id),
        None,
        Some(team.id),
        FilterMode::All,
        false,
        None,
        500,
    );
    assert!(!scoped.is_empty(), "alice must have some events in the platform repo");
    assert!(
        scoped.iter().all(|e| e.repo_id == platform_repo.id && e.actor_login == "alice"),
        "repo_id and team_id must intersect, not each stand alone: {scoped:#?}"
    );

    // An unknown repo id narrows to nothing rather than erroring — the same
    // shape of answer an unknown team_id already gives.
    assert!(engine
        .list_events(
            Some(platform_repo.id + tools_repo.id + 999),
            None,
            None,
            FilterMode::All,
            false,
            None,
            500,
        )
        .is_empty());
}

/// The "My team" view filter composes with `repo_id` too: narrowing within
/// one repo works the same way it does across every repo, and switching back
/// to "Everyone" recovers that repo's other actor's events immediately.
#[test]
fn team_view_filter_composes_with_repo_id() {
    let mut srv = server();
    let _platform = mount(&mut srv, &RepoMock::busy("acme", "platform"));
    let (engine, _dir) = engine_for(&srv);
    let repo = engine.add_repo("acme/platform", None).unwrap();
    engine.create_team("Platform", &["alice".to_string()]).unwrap();
    let result = engine.poll_now();
    assert!(result.errors.is_empty(), "unexpected errors: {:?}", result.errors);

    let team_only = engine.list_events(Some(repo.id), None, None, FilterMode::Team, false, None, 500);
    assert!(!team_only.is_empty());
    assert!(
        team_only.iter().all(|e| e.actor_login == "alice"),
        "bob is on no team and must not show under Team mode: {team_only:#?}"
    );

    let everyone = engine.list_events(Some(repo.id), None, None, FilterMode::All, false, None, 500);
    assert_eq!(everyone.len(), 12, "switching to Everyone recovers bob's events in this repo too");
}

/// With no team having a single member yet, `mode: Team` behaves exactly
/// like `All` — the same fresh-install guard the old ingestion filter had,
/// now applied at read time instead.
#[test]
fn team_view_filter_admits_everyone_with_no_teams_configured() {
    let mut srv = server();
    let _mocks = mount(&mut srv, &RepoMock::busy("acme", "platform"));
    let (engine, _dir) = engine_for(&srv);
    assert!(engine.list_teams().is_empty());
    engine.add_repo("acme/platform", None).unwrap();
    engine.poll_now();

    assert_eq!(
        engine.list_events(None, None, None, FilterMode::Team, false, None, 500).len(),
        12,
        "an install with no teams configured must not read as configured to exclude everybody"
    );
}
