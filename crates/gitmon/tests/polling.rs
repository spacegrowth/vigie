//! `poll_now`: event derivation, dedupe, and error isolation. `Settings.filter_mode`
//! no longer touches ingestion (see `feed_filter.rs` for what it means now,
//! applied at `list_events` read time instead) — `filter_all` below just keeps
//! these fixtures' default `Settings` out of the way.

mod common;

use common::{
    engine_for, mock_get, mount, mount_logging_since, recorded, server, since_log, RepoMock,
    Resp,
};
use gitmon::{ErrorKind, EventKind, FilterMode, Settings, FIRST_POLL_LOOKBACK_SECS};
use std::collections::HashSet;

/// Vestigial since `filter_mode` stopped touching ingestion (packet
/// gm-feed-r1): kept only so the many `filter_all(&engine)` call sites below
/// don't all need editing for a setting that no longer changes what a poll
/// stores. Left in place rather than deleted call-by-call — a mechanical
/// no-op, not a behavioural claim.
fn filter_all(engine: &gitmon::Engine) {
    engine
        .set_settings(Settings { filter_mode: FilterMode::All, ..Settings::default() })
        .expect("settings accepted");
}

/// 2. A first poll of a busy repo yields every kind, correctly titled and URL'd.
#[test]
fn a_first_poll_yields_every_event_kind() {
    let mut srv = server();
    let _mocks = mount(&mut srv, &RepoMock::busy("acme", "platform"));
    let (engine, _dir) = engine_for(&srv);
    filter_all(&engine);
    engine.add_repo("acme/platform", None).unwrap();

    let result = engine.poll_now();
    assert!(result.errors.is_empty(), "unexpected errors: {:?}", result.errors);

    let kinds: HashSet<EventKind> = result.new_events.iter().map(|e| e.kind).collect();
    for kind in EventKind::ALL {
        assert!(kinds.contains(&kind), "no {} event; got {kinds:?}", kind.as_str());
    }
    assert_eq!(result.new_events.len(), 12, "events: {:#?}", result.new_events);

    // Newest first.
    let times: Vec<i64> = result.new_events.iter().map(|e| e.occurred_at).collect();
    let mut sorted = times.clone();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    assert_eq!(times, sorted, "poll result must be newest first");

    let find = |kind: EventKind, number: Option<u64>| {
        result
            .new_events
            .iter()
            .find(|e| e.kind == kind && e.number == number)
            .unwrap_or_else(|| panic!("no {} event for {number:?}", kind.as_str()))
    };

    // A commit: subject line as the title, body collapsed to one line, no number.
    let commit = result
        .new_events
        .iter()
        .find(|e| e.kind == EventKind::Commit && e.actor_login == "alice")
        .expect("alice's commit");
    assert_eq!(commit.title, "Add sparkline to the tray menu");
    assert_eq!(
        commit.body_preview.as_deref(),
        Some("Keeps the last hour of activity in the menu bar.")
    );
    // The untruncated body is stored beside the preview: for a commit that is
    // the whole message, subject line and original line breaks intact.
    assert_eq!(
        commit.body.as_deref(),
        Some(
            "Add sparkline to the tray menu\n\nKeeps   the   last hour of activity\nin the menu bar."
        ),
        "body is the raw message, not the collapsed preview"
    );
    assert_eq!(
        commit.url,
        "https://github.com/acme/platform/commit/c0ffee1111111111111111111111111111111111"
    );
    assert_eq!(commit.number, None);
    assert_eq!(
        commit.actor_avatar_url.as_deref(),
        Some("https://avatars.githubusercontent.com/u/1?v=4")
    );
    assert!(!commit.seen);

    let opened = find(EventKind::PrOpened, Some(101));
    assert_eq!(opened.title, "Split the poller out of the engine");
    assert_eq!(opened.url, "https://github.com/acme/platform/pull/101");
    assert_eq!(opened.actor_login, "alice");

    let merged = find(EventKind::PrMerged, Some(102));
    assert_eq!(merged.actor_login, "bob");
    assert_eq!(merged.title, "Bundle rusqlite instead of linking the system library");

    let closed = find(EventKind::PrClosed, Some(103));
    assert_eq!(closed.title, "Try tokio for the poll timer");

    // A review carries the contract's title, not the PR title.
    let reviewed = find(EventKind::PrReviewed, Some(101));
    assert_eq!(reviewed.title, "Review: approved");
    assert_eq!(reviewed.actor_login, "bob");
    assert_eq!(
        reviewed.url,
        "https://github.com/acme/platform/pull/101#pullrequestreview-7001"
    );
    assert_eq!(reviewed.body_preview.as_deref(), Some("Reads well. Shipping."));
    assert_eq!(reviewed.body.as_deref(), Some("Reads well. Shipping."));

    let issue = find(EventKind::IssueOpened, Some(5));
    assert_eq!(issue.title, "Tray icon is blurry on non-retina displays");

    // Comments are titled with their PR/issue title and linked to the comment.
    let issue_comment = find(EventKind::IssueCommented, Some(5));
    assert_eq!(issue_comment.title, "Tray icon is blurry on non-retina displays");
    assert_eq!(
        issue_comment.url,
        "https://github.com/acme/platform/issues/5#issuecomment-9002"
    );

    let pr_comments: Vec<_> =
        result.new_events.iter().filter(|e| e.kind == EventKind::PrCommented).collect();
    assert_eq!(pr_comments.len(), 2, "one issue comment and one review comment");
    for comment in &pr_comments {
        assert_eq!(comment.title, "Split the poller out of the engine");
        assert_eq!(comment.number, Some(101));
    }
    let urls: HashSet<&str> = pr_comments.iter().map(|e| e.url.as_str()).collect();
    assert!(urls.contains("https://github.com/acme/platform/pull/101#issuecomment-9001"));
    assert!(urls.contains("https://github.com/acme/platform/pull/101#discussion_r9100"));

    // A pending review is not an event, and the PR in the issues payload is not
    // an issue.
    assert_eq!(result.new_events.iter().filter(|e| e.kind == EventKind::PrReviewed).count(), 1);
    assert_eq!(result.new_events.iter().filter(|e| e.kind == EventKind::IssueOpened).count(), 1);

    assert_eq!(engine.unseen_count(), 12);
}

/// 3. Re-polling the overlap window must not produce duplicate rows.
#[test]
fn polling_twice_stores_nothing_new() {
    let mut srv = server();
    let _mocks = mount(&mut srv, &RepoMock::busy("acme", "platform"));
    let (engine, _dir) = engine_for(&srv);
    filter_all(&engine);
    engine.add_repo("acme/platform", None).unwrap();

    let first = engine.poll_now();
    assert_eq!(first.new_events.len(), 12);

    let second = engine.poll_now();
    assert!(second.errors.is_empty(), "unexpected errors: {:?}", second.errors);
    assert!(
        second.new_events.is_empty(),
        "second poll invented events: {:#?}",
        second.new_events
    );
    assert_eq!(engine.list_events(None, None, None, FilterMode::All, false, None, 500).len(), 12);
}

/// Ingestion no longer applies a team filter at all (packet gm-feed-r1):
/// before this, `team` mode with alice on a team and bob on neither dropped
/// every one of bob's events at ingestion — permanently, since nothing
/// filtered out at insert time was ever stored to recover later. Now both
/// actors are stored regardless of `Settings.filter_mode` or team membership;
/// `feed_filter.rs` covers the "My team / Everyone" scoping this moved to,
/// applied at `list_events` read time instead.
#[test]
fn ingestion_stores_every_actor_regardless_of_team_membership() {
    let mut srv = server();
    let _mocks = mount(&mut srv, &RepoMock::busy("acme", "platform"));
    let (engine, _dir) = engine_for(&srv);
    // Default filter_mode is "team"; mixed case proves logins are normalized
    // the same as they always were, just no longer for an ingestion decision.
    // Only alice is on a team here — bob is on neither.
    engine.create_team("Platform", &["Alice".to_string()]).unwrap();
    engine.create_team("Infra", &["carol".to_string()]).unwrap();
    engine.add_repo("acme/platform", None).unwrap();

    let result = engine.poll_now();
    assert!(result.errors.is_empty(), "unexpected errors: {:?}", result.errors);
    assert_eq!(result.new_events.len(), 12, "nobody is dropped at ingestion any more");
    let actors: HashSet<&str> = result.new_events.iter().map(|e| e.actor_login.as_str()).collect();
    assert!(actors.contains("alice") && actors.contains("bob"), "actors: {actors:?}");
    // Stored, not merely reported.
    assert_eq!(
        engine.list_events(None, Some("bob"), None, FilterMode::All, false, None, 500).len(),
        5,
        "bob's events must actually be in the store, not just counted in the poll result"
    );
}

/// The union really is a union: with alice in one team and bob in another,
/// `team` mode keeps both, and an actor in neither is still dropped.
#[test]
fn team_mode_admits_an_actor_from_any_team() {
    let mut srv = server();
    let _mocks = mount(&mut srv, &RepoMock::busy("acme", "platform"));
    let (engine, _dir) = engine_for(&srv);
    let platform = engine.create_team("Platform", &["alice".to_string()]).unwrap();
    let infra = engine.create_team("Infra", &["bob".to_string()]).unwrap();
    engine.add_repo("acme/platform", None).unwrap();

    let result = engine.poll_now();

    let actors: HashSet<&str> = result.new_events.iter().map(|e| e.actor_login.as_str()).collect();
    assert!(actors.contains("alice"), "alice is on Platform: {actors:?}");
    assert!(actors.contains("bob"), "bob is on Infra and must not be dropped: {actors:?}");
    assert_eq!(result.new_events.len(), 12, "the same 12 as `all`, via two teams");

    // Each event is attributed to its own actor's team, and only that one.
    for event in &result.new_events {
        let expected = match event.actor_login.as_str() {
            "alice" => platform.id,
            "bob" => infra.id,
            other => panic!("unexpected actor {other}"),
        };
        assert_eq!(event.team_ids, vec![expected], "wrong team on {event:?}");
    }

    // Nobody outside either team got in.
    assert!(engine.list_events(None, Some("carol"), None, FilterMode::All, false, None, 500).is_empty());
}

/// With no teams at all, `team` mode admits *everyone*. An install in this
/// state has not been configured yet rather than configured to exclude
/// everybody, and the contract is explicit that a fresh install must never show
/// an empty feed because nobody has been added.
#[test]
fn team_mode_with_no_teams_stores_everything() {
    let mut srv = server();
    let _mocks = mount(&mut srv, &RepoMock::busy("acme", "platform"));
    let (engine, _dir) = engine_for(&srv);
    assert!(engine.list_teams().is_empty());
    engine.add_repo("acme/platform", None).unwrap();

    let result = engine.poll_now();

    assert!(result.errors.is_empty(), "unexpected errors: {:?}", result.errors);
    assert_eq!(result.new_events.len(), 12, "the same 12 as `all` mode");
    let actors: HashSet<&str> = result.new_events.iter().map(|e| e.actor_login.as_str()).collect();
    assert!(actors.contains("alice") && actors.contains("bob"), "actors: {actors:?}");
}

/// The owner's real install, reduced: repos polling cleanly, one team, nobody on
/// it, and the default `team` filter. Before this the filter silently dropped
/// every event and the feed sat empty with nothing visibly wrong; an empty team
/// is "not filled in yet", so it must drop nobody.
#[test]
fn a_team_with_no_members_drops_nobody() {
    let mut srv = server();
    let _mocks = mount(&mut srv, &RepoMock::busy("acme", "platform"));
    let (engine, _dir) = engine_for(&srv);
    // Default filter_mode is "team" — deliberately not overridden here.
    let team = engine.create_team("Platform", &[]).unwrap();
    assert!(team.logins.is_empty());
    engine.add_repo("acme/platform", None).unwrap();

    let result = engine.poll_now();

    assert!(result.errors.is_empty(), "unexpected errors: {:?}", result.errors);
    assert_eq!(result.new_events.len(), 12, "an empty team must not empty the feed");
    let actors: HashSet<&str> = result.new_events.iter().map(|e| e.actor_login.as_str()).collect();
    assert!(actors.contains("alice") && actors.contains("bob"), "actors: {actors:?}");
    // Stored, not merely reported: the filter runs before the insert.
    assert_eq!(engine.list_events(None, None, None, FilterMode::All, false, None, 500).len(), 12);
    // Nobody is on the team, so nothing is attributed to it.
    for event in &result.new_events {
        assert!(event.team_ids.is_empty(), "unexpected team on {event:?}");
    }
}

/// Before this packet, narrowing a team only took effect on the *next* poll —
/// filtering happened at ingestion, so a member added after a poll could not
/// change what that poll had already (permanently) dropped. Now filtering
/// happens at `list_events` read time instead (packet gm-feed-r1): adding a
/// team's first member narrows the "My team" view immediately, no poll
/// required, and reverses just as instantly back to "Everyone" — because
/// nothing was ever dropped from storage to begin with.
#[test]
fn adding_the_first_team_member_narrows_the_view_filter_immediately() {
    let mut srv = server();
    let _platform = mount(&mut srv, &RepoMock::busy("acme", "platform"));
    let (engine, _dir) = engine_for(&srv);
    let mut team = engine.create_team("Platform", &[]).unwrap();
    engine.add_repo("acme/platform", None).unwrap();

    // Empty team: everybody is stored, and the "My team" view's fresh-install
    // guard means it shows everyone too — the same guard `Settings.filter_mode
    // = team` had at ingestion, just applied here instead.
    assert_eq!(engine.poll_now().new_events.len(), 12);
    assert_eq!(
        engine.list_events(None, None, None, FilterMode::Team, false, None, 500).len(),
        12,
        "an empty team must not narrow the view either"
    );

    // One member is added to that team — not a new team, and no new poll.
    team.logins = vec!["alice".to_string()];
    engine.update_team(team).unwrap();

    let team_only = engine.list_events(None, None, None, FilterMode::Team, false, None, 500);
    assert!(!team_only.is_empty(), "the narrowed view must still show alice");
    for event in &team_only {
        assert_eq!(event.actor_login, "alice", "bob got through the narrowed view filter: {event:?}");
    }

    // Nothing was lost: switching back to "Everyone" recovers bob's events
    // with no re-poll and no backfill.
    assert_eq!(
        engine.list_events(None, None, None, FilterMode::All, false, None, 500).len(),
        12,
        "narrowing the view must never drop a stored event"
    );
}

/// Your own activity is never somebody else's to filter out: the signed-in
/// account's login passes the "My team" *view* filter even with a populated
/// team it is not on. (Ingestion itself drops nobody any more — see
/// `ingestion_stores_every_actor_regardless_of_team_membership` above — so
/// this is now a `list_events(mode)` property, not a poll one.)
#[test]
fn the_signed_in_login_passes_the_team_view_filter_without_being_on_a_team() {
    let mut srv = server();
    let _mocks = mount(&mut srv, &RepoMock::busy("acme", "platform"));
    let _user = mock_get(
        &mut srv,
        "/user",
        &Resp::ok(r#"{"login":"bob","avatar_url":"https://avatars.githubusercontent.com/bob"}"#),
    );
    let (engine, _dir) = engine_for(&srv);
    // A team with a member in it, so the fresh-install guard is shut. bob is
    // not on it; neither is alice.
    engine.create_team("Infra", &["carol".to_string()]).unwrap();
    assert_eq!(engine.verify_token().unwrap(), "bob");
    engine.add_repo("acme/platform", None).unwrap();

    let result = engine.poll_now();
    // Everything is stored regardless of team membership now.
    assert_eq!(result.new_events.len(), 12, "unexpected errors: {:?}", result.errors);

    // But the "My team" view still keeps bob's own events, because he is
    // signed in, while alice — on no team and not signed in — is dropped.
    let bobs = engine.list_events(None, Some("bob"), None, FilterMode::Team, false, None, 500);
    assert_eq!(bobs.len(), 5, "bob's own activity must pass the team view filter");
    assert!(
        engine.list_events(None, Some("alice"), None, FilterMode::Team, false, None, 500).is_empty(),
        "alice is on no team and not signed in, so the team view filter must drop her"
    );
}

/// `rescan(None)` drops every watermark, so the next poll asks GitHub for the
/// last 24 hours again instead of the 5-minute overlap. The window really does
/// move back on the wire, and the dedupe index makes the re-fetch a no-op: no
/// duplicate rows, no invented "new" events.
#[test]
fn rescan_refetches_the_last_24_hours_without_duplicating_anything() {
    let mut srv = server();
    let windows = since_log();
    let _mocks = mount_logging_since(&mut srv, &RepoMock::busy("acme", "platform"), &windows);
    let (engine, _dir) = engine_for(&srv);
    filter_all(&engine);
    engine.add_repo("acme/platform", None).unwrap();

    assert_eq!(engine.poll_now().new_events.len(), 12);
    // A second poll rides the watermark: a narrow window, and nothing new.
    assert!(engine.poll_now().new_events.is_empty());

    engine.rescan(None).unwrap();
    let third = engine.poll_now();

    assert!(third.errors.is_empty(), "unexpected errors: {:?}", third.errors);
    assert!(
        third.new_events.is_empty(),
        "re-fetched events were stored twice: {:#?}",
        third.new_events
    );
    assert_eq!(
        engine.list_events(None, None, None, FilterMode::All, false, None, 500).len(),
        12,
        "the rescan duplicated rows"
    );

    // The mock saw the window widen back out. Exact timestamps depend on when
    // the poll ran, so the assertions are on the shape: narrow, then wide again.
    let seen = recorded(&windows);
    assert_eq!(seen.len(), 3, "one commits request per poll: {seen:?}");
    assert!(
        seen[1] - seen[0] > FIRST_POLL_LOOKBACK_SECS / 2,
        "the second poll should have ridden the watermark, not looked back a day: {seen:?}"
    );
    assert!(seen[2] < seen[1], "the rescan did not move `since` back: {seen:?}");
    assert!(
        (seen[2] - seen[0]).abs() <= 5,
        "the rescan should reopen the 24h first-poll window: {seen:?}"
    );
}

/// A rescan names a repo the app believes in; an id that no longer exists is a
/// mistake worth reporting rather than a silent no-op that clears nothing.
#[test]
fn rescanning_an_unknown_repo_is_not_found() {
    let mut srv = server();
    let _mocks = mount(&mut srv, &RepoMock::busy("acme", "platform"));
    let (engine, _dir) = engine_for(&srv);
    let repo = engine.add_repo("acme/platform", None).unwrap();
    engine.poll_now();

    let err = engine.rescan(Some(repo.id + 999)).unwrap_err();

    assert_eq!(err.kind, ErrorKind::NotFound);
    // The real repo's watermark is untouched by the failed call.
    assert!(engine.list_repos()[0].last_polled_at.is_some());
    // And rescanning it by id does clear it.
    engine.rescan(Some(repo.id)).unwrap();
    assert!(engine.list_repos()[0].last_polled_at.is_none());
}

/// 5. A merged PR is `pr_merged` and never also `pr_closed`.
#[test]
fn a_merged_pull_request_is_never_reported_as_closed() {
    let mut srv = server();
    let _mocks = mount(&mut srv, &RepoMock::busy("acme", "platform"));
    let (engine, _dir) = engine_for(&srv);
    filter_all(&engine);
    engine.add_repo("acme/platform", None).unwrap();

    let result = engine.poll_now();
    let for_pr = |number: u64, kind: EventKind| {
        result.new_events.iter().filter(|e| e.number == Some(number) && e.kind == kind).count()
    };

    assert_eq!(for_pr(102, EventKind::PrMerged), 1);
    assert_eq!(for_pr(102, EventKind::PrClosed), 0, "merged PR 102 must not be closed too");
    // The PR that was closed without merging still reports pr_closed.
    assert_eq!(for_pr(103, EventKind::PrClosed), 1);
    assert_eq!(for_pr(103, EventKind::PrMerged), 0);
}

/// 6. One failing repo does not stop the others, and does not advance its own
///    watermark.
#[test]
fn a_failing_repo_is_isolated() {
    let mut srv = server();
    let mut broken = RepoMock::quiet("acme", "broken");
    broken.commits = Resp::status(500, r#"{"message":"Server Error"}"#);
    let _broken_mocks = mount(&mut srv, &broken);
    let _healthy_mocks = mount(&mut srv, &RepoMock::busy("acme", "platform"));

    let (engine, _dir) = engine_for(&srv);
    filter_all(&engine);
    let broken_repo = engine.add_repo("acme/broken", None).unwrap();
    let healthy_repo = engine.add_repo("acme/platform", None).unwrap();

    let result = engine.poll_now();

    assert_eq!(result.errors.len(), 1, "errors: {:?}", result.errors);
    assert_eq!(result.errors[0].repo_id, broken_repo.id);
    assert!(!result.errors[0].message.is_empty());
    // The healthy repo still produced its events.
    assert_eq!(result.new_events.len(), 12);
    assert!(result.new_events.iter().all(|e| e.repo_id == healthy_repo.id));

    let repos = engine.list_repos();
    let broken_row = repos.iter().find(|r| r.id == broken_repo.id).unwrap();
    let healthy_row = repos.iter().find(|r| r.id == healthy_repo.id).unwrap();

    assert_eq!(broken_row.last_polled_at, None, "a failed poll must not advance the watermark");
    assert!(broken_row.last_error.is_some(), "the failure must be recorded");
    assert!(healthy_row.last_polled_at.is_some(), "the healthy repo's watermark advanced");
    assert_eq!(healthy_row.last_error, None);
}

/// A commit with no linked GitHub account falls back to the git author name and
/// carries no avatar.
#[test]
fn a_commit_without_an_account_falls_back_to_the_git_name() {
    let mut srv = server();
    let mut repo = RepoMock::quiet("acme", "platform");
    repo.commits = Resp::ok(common::fixture("commits_unlinked_author.json"));
    let _mocks = mount(&mut srv, &repo);
    let (engine, _dir) = engine_for(&srv);
    filter_all(&engine);
    engine.add_repo("acme/platform", None).unwrap();

    let result = engine.poll_now();
    assert_eq!(result.new_events.len(), 1);
    let event = &result.new_events[0];
    assert_eq!(event.actor_login, "Carla Diaz");
    assert_eq!(event.actor_avatar_url, None);
    assert_eq!(event.title, "Vendor the sqlite amalgamation");
}

/// A poller-side rate limit carries the refill time out with it: a 403 with an
/// exhausted `x-ratelimit-remaining` and an `x-ratelimit-reset` becomes a
/// `RepoError` whose `reset_at` is that header, so the app's banner can say
/// when polling resumes rather than only that it stopped.
#[test]
fn a_rate_limited_repo_reports_its_reset_time() {
    const RESET_AT: i64 = 1_760_003_600;

    let mut srv = server();
    let mut limited = RepoMock::quiet("acme", "platform");
    limited.commits = Resp::status(403, r#"{"message":"API rate limit exceeded"}"#)
        .header("x-ratelimit-remaining", "0")
        .header("x-ratelimit-reset", &RESET_AT.to_string());
    let _mocks = mount(&mut srv, &limited);

    let (engine, _dir) = engine_for(&srv);
    filter_all(&engine);
    let repo = engine.add_repo("acme/platform", None).unwrap();

    let result = engine.poll_now();

    assert_eq!(result.errors.len(), 1, "errors: {:?}", result.errors);
    let error = &result.errors[0];
    assert_eq!(error.repo_id, repo.id);
    assert!(
        error.message.starts_with("rate_limited: "),
        "a 403 with no budget left is rate_limited, got {:?}",
        error.message
    );
    assert_eq!(error.reset_at, Some(RESET_AT), "the reset header must survive into RepoError");

    // A failure of any other kind still carries no reset time.
    let mut srv2 = server();
    let mut broken = RepoMock::quiet("acme", "web");
    broken.commits = Resp::status(500, r#"{"message":"Server Error"}"#);
    let _broken_mocks = mount(&mut srv2, &broken);
    let (engine2, _dir2) = engine_for(&srv2);
    filter_all(&engine2);
    engine2.add_repo("acme/web", None).unwrap();
    let result2 = engine2.poll_now();
    assert_eq!(result2.errors.len(), 1, "errors: {:?}", result2.errors);
    assert_eq!(result2.errors[0].reset_at, None);
}
