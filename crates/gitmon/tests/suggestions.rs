//! `suggest_people` and `suggest_repos`.

mod common;

use common::{engine_for, fixture, mock_get, mount, server, RepoMock, Resp};
use gitmon::{FilterMode, PersonSource, Settings};

#[test]
fn people_are_ranked_by_activity_then_org_membership_then_bots() {
    let mut srv = server();
    let _mocks = mount(&mut srv, &RepoMock::busy("acme", "platform"));
    let _members =
        mock_get(&mut srv, "/orgs/acme/members", &Resp::ok(fixture("org_members.json")));
    let (engine, _dir) = engine_for(&srv);
    engine
        .set_settings(Settings { filter_mode: FilterMode::All, ..Settings::default() })
        .unwrap();
    engine.add_repo("acme/platform", None).unwrap();
    engine.poll_now();
    // Alice is already on this team, so she must not be suggested for it.
    let platform = engine.create_team("Platform", &["alice".to_string()]).unwrap();

    let people = engine.suggest_people("", Some(platform.id), 10).unwrap();
    let logins: Vec<&str> = people.iter().map(|p| p.login.as_str()).collect();
    assert_eq!(logins, vec!["bob", "carol", "dependabot[bot]"]);
    assert!(!logins.contains(&"alice"), "a team member was suggested");

    assert_eq!(people[0].source, PersonSource::Contributor);
    assert_eq!(people[0].why, "5 commits · acme/platform");
    assert_eq!(
        people[0].avatar_url.as_deref(),
        Some("https://avatars.githubusercontent.com/u/2?v=4")
    );

    assert_eq!(people[1].source, PersonSource::OrgMember);
    assert_eq!(people[1].why, "member of acme");

    // A bot is labelled and sorted last.
    assert_eq!(people[2].source, PersonSource::Bot);

    // Query narrowing is case-insensitive, matches substrings, and puts prefix
    // matches first: "bob" starts with "bo", "dependabot[bot]" merely contains it.
    let matched = engine.suggest_people("BO", Some(platform.id), 10).unwrap();
    assert_eq!(
        matched.iter().map(|p| p.login.as_str()).collect::<Vec<_>>(),
        vec!["bob", "dependabot[bot]"]
    );
    let prefix_only = engine.suggest_people("car", Some(platform.id), 10).unwrap();
    assert_eq!(prefix_only.iter().map(|p| p.login.as_str()).collect::<Vec<_>>(), vec!["carol"]);
    assert!(engine.suggest_people("zzz", Some(platform.id), 10).unwrap().is_empty());

    // The limit is honoured.
    assert_eq!(engine.suggest_people("", Some(platform.id), 2).unwrap().len(), 2);
}

/// The exclusion is scoped to the team being filled in: without a `team_id`
/// nothing is excluded, and another team's members are never excluded.
#[test]
fn only_the_named_teams_members_are_excluded() {
    let mut srv = server();
    let _mocks = mount(&mut srv, &RepoMock::busy("acme", "platform"));
    let _members =
        mock_get(&mut srv, "/orgs/acme/members", &Resp::ok(fixture("org_members.json")));
    let (engine, _dir) = engine_for(&srv);
    engine
        .set_settings(Settings { filter_mode: FilterMode::All, ..Settings::default() })
        .unwrap();
    engine.add_repo("acme/platform", None).unwrap();
    engine.poll_now();

    let platform = engine.create_team("Platform", &["alice".to_string()]).unwrap();
    let infra = engine.create_team("Infra", &["bob".to_string()]).unwrap();

    // With no team named, nobody is held back.
    let everyone: Vec<String> =
        engine.suggest_people("", None, 10).unwrap().into_iter().map(|p| p.login).collect();
    assert!(everyone.contains(&"alice".to_string()), "{everyone:?}");
    assert!(everyone.contains(&"bob".to_string()), "{everyone:?}");

    // Filling in Platform hides only alice; bob's membership of Infra is
    // irrelevant, because a login may be on several teams.
    let for_platform: Vec<String> = engine
        .suggest_people("", Some(platform.id), 10)
        .unwrap()
        .into_iter()
        .map(|p| p.login)
        .collect();
    assert!(!for_platform.contains(&"alice".to_string()), "{for_platform:?}");
    assert!(for_platform.contains(&"bob".to_string()), "{for_platform:?}");

    // And the mirror image for Infra.
    let for_infra: Vec<String> = engine
        .suggest_people("", Some(infra.id), 10)
        .unwrap()
        .into_iter()
        .map(|p| p.login)
        .collect();
    assert!(for_infra.contains(&"alice".to_string()), "{for_infra:?}");
    assert!(!for_infra.contains(&"bob".to_string()), "{for_infra:?}");

    // An id that matches no team narrows nothing rather than failing.
    let unknown: Vec<String> = engine
        .suggest_people("", Some(9_999), 10)
        .unwrap()
        .into_iter()
        .map(|p| p.login)
        .collect();
    assert_eq!(unknown, everyone);
}

/// `suggest_active_people` is `suggest_people`'s mirror image: with a
/// `team_id` it keeps only that team's members instead of excluding them —
/// the "narrow a search to this team" flow (Summary's person filter), not
/// the "who's left to add" flow `suggest_people` serves.
#[test]
fn suggest_active_people_narrows_to_the_named_teams_members() {
    let mut srv = server();
    let _mocks = mount(&mut srv, &RepoMock::busy("acme", "platform"));
    let _members =
        mock_get(&mut srv, "/orgs/acme/members", &Resp::ok(fixture("org_members.json")));
    let (engine, _dir) = engine_for(&srv);
    engine
        .set_settings(Settings { filter_mode: FilterMode::All, ..Settings::default() })
        .unwrap();
    engine.add_repo("acme/platform", None).unwrap();
    engine.poll_now();

    let platform = engine.create_team("Platform", &["alice".to_string()]).unwrap();
    let infra = engine.create_team("Infra", &["bob".to_string()]).unwrap();

    // With no team named, nothing is restricted — same population as
    // `suggest_people` with no team_id.
    let everyone: Vec<String> = engine
        .suggest_active_people("", None, 10)
        .unwrap()
        .into_iter()
        .map(|p| p.login)
        .collect();
    assert!(everyone.contains(&"alice".to_string()), "{everyone:?}");
    assert!(everyone.contains(&"bob".to_string()), "{everyone:?}");

    // Scoped to Platform, only alice (its member) matches — the opposite of
    // `suggest_people`, which would hide her.
    let for_platform: Vec<String> = engine
        .suggest_active_people("", Some(platform.id), 10)
        .unwrap()
        .into_iter()
        .map(|p| p.login)
        .collect();
    assert_eq!(for_platform, vec!["alice".to_string()]);

    // And the mirror image for Infra.
    let for_infra: Vec<String> = engine
        .suggest_active_people("", Some(infra.id), 10)
        .unwrap()
        .into_iter()
        .map(|p| p.login)
        .collect();
    assert_eq!(for_infra, vec!["bob".to_string()]);

    // A query still narrows within the team's members.
    let queried: Vec<String> = engine
        .suggest_active_people("al", Some(platform.id), 10)
        .unwrap()
        .into_iter()
        .map(|p| p.login)
        .collect();
    assert_eq!(queried, vec!["alice".to_string()]);
    assert!(engine.suggest_active_people("bo", Some(platform.id), 10).unwrap().is_empty());

    // An id matching no team narrows to nobody, not to everybody — unlike
    // `suggest_people`, where an unknown id excludes nobody. Here the id
    // names the population, same as `digest`'s `team_id`.
    assert!(engine.suggest_active_people("", Some(9_999), 10).unwrap().is_empty());
}

#[test]
fn repos_are_suggested_from_recent_pushes_and_open_pull_requests() {
    let mut srv = server();
    let _mocks = mount(&mut srv, &RepoMock::quiet("acme", "platform"));
    let _user = mock_get(&mut srv, "/user", &Resp::ok(fixture("user.json")));
    let _repos = mock_get(&mut srv, "/user/repos", &Resp::ok(fixture("user_repos.json")));
    let _search =
        mock_get(&mut srv, "/search/issues", &Resp::ok(fixture("search_issues.json")));
    let (engine, _dir) = engine_for(&srv);
    // Already watched, so it must not be suggested back.
    engine.add_repo("acme/platform", None).unwrap();

    let suggestions = engine.suggest_repos(None).unwrap();
    let slugs: Vec<String> =
        suggestions.iter().map(|s| format!("{}/{}", s.owner, s.name)).collect();

    assert_eq!(slugs, vec!["acme/tray", "acme/vigie-web"]);
    assert_eq!(suggestions[0].why, "you pushed 2 days ago");
    assert_eq!(suggestions[1].why, "2 open PRs");
    assert!(!slugs.contains(&"acme/ancient".to_string()), "older than 90 days");
    assert!(!slugs.contains(&"acme/platform".to_string()), "already watched");
}
