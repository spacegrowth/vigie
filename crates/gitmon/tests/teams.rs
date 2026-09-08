//! Teams: CRUD and ordering, and the `team_id` view over `list_events`.
//!
//! Membership is never stored on an event — `team_ids` is resolved on every
//! read — so several of these tests edit a team and re-read *without polling
//! again*, which is the only way to prove the feed re-categorises rather than
//! backfills.

mod common;

use common::{engine_for, mount, server, RepoMock};
use gitmon::{Engine, ErrorKind, FilterMode, Settings, Team};
use tempfile::TempDir;

/// An engine already holding the busy fixture's 12 events — 7 by alice, 5 by
/// bob — ingested under `all`, so what the store contains is not itself the
/// work of a team filter. Every assertion afterwards is store-only, so the
/// mocks are free to drop when this returns.
fn engine_with_events(srv: &mut mockito::ServerGuard) -> (Engine, TempDir) {
    let _mocks = mount(srv, &RepoMock::busy("acme", "platform"));
    let (engine, dir) = engine_for(srv);
    engine
        .set_settings(Settings { filter_mode: FilterMode::All, ..Settings::default() })
        .unwrap();
    engine.add_repo("acme/platform", None).unwrap();
    let polled = engine.poll_now();
    assert_eq!(polled.new_events.len(), 12, "the busy fixture drifted");
    (engine, dir)
}

#[test]
fn teams_are_created_updated_reordered_and_deleted() {
    let srv = server();
    let (engine, _dir) = engine_for(&srv);

    let platform = engine.create_team("Platform", &["alice".to_string()]).unwrap();
    let infra = engine.create_team("Infra", &["bob".to_string(), "carol".to_string()]).unwrap();
    assert_ne!(platform.id, infra.id, "each team gets its own id");
    assert_eq!(
        engine.list_teams().iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        vec!["Platform", "Infra"],
        "a new team is appended last"
    );

    // Update replaces name and logins wholesale, and leaves the other team be.
    engine
        .update_team(Team {
            id: platform.id,
            name: "Platform Eng".into(),
            logins: vec!["alice".into(), "dave".into()],
        })
        .unwrap();
    let teams = engine.list_teams();
    assert_eq!(teams[0].name, "Platform Eng");
    assert_eq!(teams[0].logins, vec!["alice", "dave"]);
    assert_eq!(teams[1].logins, vec!["bob", "carol"], "Infra was not touched");

    // Reorder is by id, and changes only display order.
    engine.reorder_teams(&[infra.id, platform.id]).unwrap();
    assert_eq!(
        engine.list_teams().iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        vec!["Infra", "Platform Eng"]
    );
    assert_eq!(engine.list_teams()[0].logins, vec!["bob", "carol"], "logins moved too");

    // Delete takes that team's logins with it and leaves the survivor whole.
    engine.delete_team(platform.id).unwrap();
    let left = engine.list_teams();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].id, infra.id);
    assert_eq!(left[0].logins, vec!["bob", "carol"]);
    // alice and dave belonged only to the deleted team, so nothing claims them.
    assert!(engine.list_events(None, None, Some(platform.id), FilterMode::All, false, None, 500).is_empty());
}

#[test]
fn an_unknown_team_id_is_not_found_on_update_and_delete() {
    let srv = server();
    let (engine, _dir) = engine_for(&srv);
    let team = engine.create_team("Platform", &[]).unwrap();

    let err = engine
        .update_team(Team { id: 9_999, name: "Ghost".into(), logins: vec![] })
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::NotFound);

    let err = engine.delete_team(9_999).unwrap_err();
    assert_eq!(err.kind, ErrorKind::NotFound);

    // Deleting twice: the second call is not_found, not a silent success.
    engine.delete_team(team.id).unwrap();
    assert_eq!(engine.delete_team(team.id).unwrap_err().kind, ErrorKind::NotFound);
}

#[test]
fn reorder_teams_demands_a_permutation_of_every_id() {
    let srv = server();
    let (engine, _dir) = engine_for(&srv);
    let a = engine.create_team("Platform", &[]).unwrap();
    let b = engine.create_team("Infra", &[]).unwrap();
    let order = || engine.list_teams().iter().map(|t| t.id).collect::<Vec<_>>();
    let before = order();

    // Missing one id: a partial order would silently leave a team where it was.
    assert_eq!(engine.reorder_teams(&[a.id]).unwrap_err().kind, ErrorKind::Invalid);
    // An extra id that is not a team at all.
    assert_eq!(
        engine.reorder_teams(&[a.id, b.id, 9_999]).unwrap_err().kind,
        ErrorKind::Invalid
    );
    // The right count, but one id repeated instead of both present.
    assert_eq!(engine.reorder_teams(&[a.id, a.id]).unwrap_err().kind, ErrorKind::Invalid);
    // Empty, while teams exist.
    assert_eq!(engine.reorder_teams(&[]).unwrap_err().kind, ErrorKind::Invalid);

    assert_eq!(order(), before, "a rejected reorder must change nothing");
    engine.reorder_teams(&[b.id, a.id]).unwrap();
    assert_eq!(order(), vec![b.id, a.id]);
}

#[test]
fn an_actor_on_two_teams_is_listed_under_both() {
    let mut srv = server();
    let (engine, _dir) = engine_with_events(&mut srv);

    let platform = engine.create_team("Platform", &["alice".to_string()]).unwrap();
    let oncall = engine.create_team("On-call", &["alice".to_string(), "bob".to_string()]).unwrap();

    // The same events, seen through each team.
    let via_platform = engine.list_events(None, None, Some(platform.id), FilterMode::All, false, None, 500);
    let via_oncall = engine.list_events(None, None, Some(oncall.id), FilterMode::All, false, None, 500);
    assert_eq!(via_platform.len(), 7, "alice's events");
    assert_eq!(via_oncall.len(), 12, "alice's and bob's");

    // And each event names every team its actor is on, in display order.
    for event in &via_oncall {
        let expected = match event.actor_login.as_str() {
            "alice" => vec![platform.id, oncall.id],
            "bob" => vec![oncall.id],
            other => panic!("unexpected actor {other}"),
        };
        assert_eq!(event.team_ids, expected, "wrong teams on {event:?}");
    }

    // An id that matches no team is an empty page, not an error.
    assert!(engine.list_events(None, None, Some(9_999), FilterMode::All, false, None, 500).is_empty());
}

#[test]
fn editing_a_team_recategorises_the_feed_without_polling_again() {
    let mut srv = server();
    let (engine, _dir) = engine_with_events(&mut srv);
    let platform =
        engine.create_team("Platform", &["alice".to_string(), "bob".to_string()]).unwrap();
    assert_eq!(engine.list_events(None, None, Some(platform.id), FilterMode::All, false, None, 500).len(), 12);

    // Drop bob from the team. No poll, no new events, no mutation of any row.
    engine
        .update_team(Team {
            id: platform.id,
            name: "Platform".into(),
            logins: vec!["alice".into()],
        })
        .unwrap();

    let after = engine.list_events(None, None, Some(platform.id), FilterMode::All, false, None, 500);
    assert_eq!(after.len(), 7, "bob's events left the team view");
    assert!(after.iter().all(|e| e.actor_login == "alice"));
    // Nothing was deleted: bob's events are still stored, just unattributed.
    let everything = engine.list_events(None, None, None, FilterMode::All, false, None, 500);
    assert_eq!(everything.len(), 12, "the filter is a view, not a delete");
    for event in &everything {
        let expected: Vec<u64> =
            if event.actor_login == "alice" { vec![platform.id] } else { vec![] };
        assert_eq!(event.team_ids, expected, "stale team on {event:?}");
    }
}

/// The `limit` must count rows that survived the team filter, not rows read
/// before it — otherwise a page of bob's events comes back short.
#[test]
fn the_limit_applies_after_the_team_filter() {
    let mut srv = server();
    let (engine, _dir) = engine_with_events(&mut srv);
    let platform = engine.create_team("Platform", &["bob".to_string()]).unwrap();

    // The fixture interleaves the two actors, so the newest three events are
    // not all bob's. That is what gives the next assertion teeth: an
    // implementation that read three rows and *then* dropped non-members would
    // hand back fewer than three here.
    let newest_three = engine.list_events(None, None, None, FilterMode::All, false, None, 3);
    assert!(
        newest_three.iter().any(|e| e.actor_login != "bob"),
        "fixture drifted; this test no longer distinguishes the two orders"
    );

    let page = engine.list_events(None, None, Some(platform.id), FilterMode::All, false, None, 3);
    assert_eq!(page.len(), 3, "a full page of bob's events");
    assert!(page.iter().all(|e| e.actor_login == "bob"));

    // And paging through with the cursor still respects the filter.
    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let page = engine.list_events(None, None, Some(platform.id), FilterMode::All, false, cursor, 2);
        if page.is_empty() {
            break;
        }
        cursor = Some(page.last().unwrap().id);
        seen.extend(page.into_iter().map(|e| e.id));
    }
    assert_eq!(seen.len(), 5, "every one of bob's events, once: {seen:?}");
    let mut unique = seen.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), seen.len(), "an event was paged twice");
}
