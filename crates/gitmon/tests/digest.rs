//! `digest`: the period summary computed from stored events alone.
//!
//! Every event here is planted through a mock GitHub server and a real poll, so
//! what the digest counts is what the store actually holds — not a hand-made
//! approximation of it. Timestamps are relative to a `base` an hour in the past:
//! comfortably inside the 24h first-poll window, and exact, because the fixtures
//! below carry the times this file computes rather than any the engine invents.

mod common;

use common::{engine_for, iso8601, mount, server, RepoMock, Resp};
use gitmon::{
    Digest, EventKind, FilterMode, Settings, ThreadKind, DIGEST_PEOPLE_LIMIT, DIGEST_THREAD_LIMIT,
};
use mockito::ServerGuard;

// ---- fixture builders ---------------------------------------------------

fn json_array(items: &[String]) -> String {
    format!("[{}]", items.join(","))
}

fn user(login: &str, avatar: Option<&str>) -> String {
    let avatar = match avatar {
        Some(url) => format!("\"{url}\""),
        None => "null".to_string(),
    };
    format!(r#"{{"login":"{login}","avatar_url":{avatar}}}"#)
}

fn commit(slug: &str, sha: &str, actor: &str, avatar: Option<&str>, at: i64) -> String {
    format!(
        r#"{{"sha":"{sha}",
             "html_url":"https://github.com/{slug}/commit/{sha}",
             "commit":{{"message":"Commit {sha}",
                        "author":{{"name":"{actor}","date":"{date}"}},
                        "committer":{{"name":"{actor}","date":"{date}"}}}},
             "author":{who},"committer":{who}}}"#,
        date = iso8601(at),
        who = user(actor, avatar),
    )
}

/// A PR. `merged_at` wins over `closed_at`: a merged PR yields only `pr_merged`.
fn pull(
    slug: &str,
    number: u64,
    actor: &str,
    title: &str,
    created_at: i64,
    merged_at: Option<i64>,
    closed_at: Option<i64>,
) -> String {
    let stamp = |at: Option<i64>| match at {
        Some(at) => format!("\"{}\"", iso8601(at)),
        None => "null".to_string(),
    };
    // The poller pages `pulls` by `updated`, stopping at the first PR updated
    // before the window, so this has to be the PR's latest activity.
    let updated_at = created_at.max(merged_at.unwrap_or(0)).max(closed_at.unwrap_or(0));
    format!(
        r#"{{"number":{number},"title":"{title}",
             "html_url":"https://github.com/{slug}/pull/{number}",
             "user":{who},"body":null,"state":"closed",
             "created_at":"{created}","updated_at":"{updated}",
             "closed_at":{closed},"merged_at":{merged},
             "merged_by":null,"head":null,"base":null,
             "additions":null,"deletions":null,"changed_files":null}}"#,
        who = user(actor, None),
        created = iso8601(created_at),
        updated = iso8601(updated_at),
        closed = stamp(closed_at),
        merged = stamp(merged_at),
    )
}

fn review(slug: &str, number: u64, id: u64, actor: &str, at: i64, state: &str) -> String {
    format!(
        r#"{{"id":{id},
             "html_url":"https://github.com/{slug}/pull/{number}#pullrequestreview-{id}",
             "body":null,"user":{who},"state":"{state}","submitted_at":"{at}"}}"#,
        who = user(actor, None),
        at = iso8601(at),
    )
}

fn issue(slug: &str, number: u64, actor: &str, title: &str, created_at: i64) -> String {
    format!(
        r#"{{"number":{number},"title":"{title}",
             "html_url":"https://github.com/{slug}/issues/{number}",
             "user":{who},"body":null,"state":"open",
             "created_at":"{created}","pull_request":null}}"#,
        who = user(actor, None),
        created = iso8601(created_at),
    )
}

/// A comment on the conversation of a PR (`on_pull`) or an issue. Only the
/// `html_url` tells the two apart, which is what decides `pr_commented` vs
/// `issue_commented`.
fn issue_comment(
    slug: &str,
    number: u64,
    id: u64,
    actor: &str,
    at: i64,
    on_pull: bool,
) -> String {
    let segment = if on_pull { "pull" } else { "issues" };
    format!(
        r#"{{"id":{id},
             "html_url":"https://github.com/{slug}/{segment}/{number}#issuecomment-{id}",
             "body":null,"user":{who},"created_at":"{at}",
             "issue_url":"https://api.github.com/repos/{slug}/issues/{number}"}}"#,
        who = user(actor, None),
        at = iso8601(at),
    )
}

fn review_comment(slug: &str, number: u64, id: u64, actor: &str, at: i64) -> String {
    format!(
        r#"{{"id":{id},
             "html_url":"https://github.com/{slug}/pull/{number}#discussion_r{id}",
             "body":null,"user":{who},"created_at":"{at}",
             "path":"src/store.rs","line":10,"original_line":10,
             "pull_request_url":"https://api.github.com/repos/{slug}/pulls/{number}"}}"#,
        who = user(actor, None),
        at = iso8601(at),
    )
}

// ---- the shared scenario ------------------------------------------------

const PLATFORM: &str = "acme/platform";
const WEB: &str = "acme/web";
const ALICE_AVATAR: &str = "https://avatars.githubusercontent.com/u/1?v=4";

const PR_101_TITLE: &str = "Split the poller out of the engine";
const PR_202_TITLE: &str = "Drop the vendored icons";
const ISSUE_303_TITLE: &str = "Tray icon is blurry on HiDPI";

/// Everything the scenario needs to make assertions: the engine, the two repo
/// ids the store handed out, and the window's `base`.
struct Scenario {
    engine: gitmon::Engine,
    platform_id: u64,
    web_id: u64,
    base: i64,
    _server: ServerGuard,
    _mocks: Vec<mockito::Mock>,
    _dir: tempfile::TempDir,
}

/// Two repos, three actors inside the window and a fourth outside it, and every
/// one of the eight `EventKind`s.
///
/// Three threads, not two: a merged PR never also emits `pr_closed`, so the
/// merge and the close have to live on different pull requests, and the issue
/// kinds need an issue of their own.
///
/// Window `[base, base + 100)`. Two events sit deliberately outside it — one
/// exactly at `end`, one a second before `start` — so the half-open boundary is
/// proved in both directions.
fn scenario() -> Scenario {
    let mut srv = server();
    let base = chrono::Utc::now().timestamp() - 3600;

    let mut platform = RepoMock::quiet("acme", "platform");
    platform.commits = Resp::ok(json_array(&[
        // Exactly at `start`: included.
        commit(PLATFORM, "aaa1", "alice", Some(ALICE_AVATAR), base),
        commit(PLATFORM, "aaa2", "alice", Some(ALICE_AVATAR), base + 10),
        commit(PLATFORM, "bbb1", "bob", None, base + 20),
        // Exactly at `end`: excluded.
        commit(PLATFORM, "aaa3", "alice", Some(ALICE_AVATAR), base + 100),
        // A second before `start`, by someone who appears nowhere else.
        commit(PLATFORM, "ddd1", "dave", None, base - 1),
    ]));
    // Thread A — PR #101: opened, reviewed, commented on, merged.
    platform.pulls = Resp::ok(json_array(&[pull(
        PLATFORM,
        101,
        "alice",
        PR_101_TITLE,
        base + 30,
        Some(base + 60),
        Some(base + 60),
    )]));
    let approval = review(PLATFORM, 101, 9001, "bob", base + 40, "APPROVED");
    platform.reviews = vec![(101, Resp::ok(json_array(&[approval])))];
    platform.issue_comments =
        Resp::ok(json_array(&[issue_comment(PLATFORM, 101, 5001, "carol", base + 50, true)]));

    let mut web = RepoMock::quiet("acme", "web");
    // Thread B — PR #202: opened, commented on, closed without merging.
    web.pulls = Resp::ok(json_array(&[pull(
        WEB,
        202,
        "carol",
        PR_202_TITLE,
        base + 15,
        None,
        Some(base + 45),
    )]));
    web.reviews = vec![(202, Resp::json_array())];
    web.pr_comments =
        Resp::ok(json_array(&[review_comment(WEB, 202, 6001, "alice", base + 35)]));
    // Thread C — issue #303: opened, then commented on.
    web.issues = Resp::ok(json_array(&[issue(WEB, 303, "bob", ISSUE_303_TITLE, base + 5)]));
    web.issue_comments =
        Resp::ok(json_array(&[issue_comment(WEB, 303, 5002, "alice", base + 70, false)]));

    let mut mocks = mount(&mut srv, &platform);
    mocks.extend(mount(&mut srv, &web));

    let (engine, dir) = engine_for(&srv);
    // `all`, so every actor's events are stored; the digest's own team scoping
    // is what these tests are about, and it filters on read, not on ingest.
    engine
        .set_settings(Settings { filter_mode: FilterMode::All, ..Settings::default() })
        .unwrap();
    let platform_id = engine.add_repo(PLATFORM, None).unwrap().id;
    let web_id = engine.add_repo(WEB, None).unwrap().id;
    let polled = engine.poll_now();
    assert!(polled.errors.is_empty(), "the fixture server misfired: {:?}", polled.errors);
    assert_eq!(polled.new_events.len(), 14, "the scenario planted 14 events");

    Scenario { engine, platform_id, web_id, base, _server: srv, _mocks: mocks, _dir: dir }
}

/// `totals` as `(kind, count)` pairs, so an assertion reads like the contract.
fn totals(digest: &Digest) -> Vec<(EventKind, u64)> {
    EventKind::ALL.into_iter().map(|kind| (kind, digest.totals.get(kind))).collect()
}

fn counts(digest: &Digest, login: &str) -> Vec<(EventKind, u64)> {
    let person = digest
        .people
        .iter()
        .find(|p| p.login == login)
        .unwrap_or_else(|| panic!("{login} is not in {:?}", digest.people));
    EventKind::ALL.into_iter().map(|kind| (kind, person.counts.get(kind))).collect()
}

// ---- the tests ----------------------------------------------------------

/// The whole shape of one window: totals, people, threads and repos, each
/// counted and ordered exactly as `docs/CONTRACT.md` specifies.
#[test]
fn a_window_is_summarised_by_kind_person_thread_and_repo() {
    let s = scenario();
    let digest = s.engine.digest(s.base, s.base + 100, None, None, 0).unwrap();

    assert_eq!(digest.start, s.base);
    assert_eq!(digest.end, s.base + 100);
    assert_eq!(digest.team_id, None);
    assert_eq!(digest.actor, None);

    // Twelve events: the fourteen planted, less the one at `end` and the one
    // before `start`.
    assert_eq!(
        totals(&digest),
        vec![
            (EventKind::Commit, 3),
            (EventKind::PrOpened, 2),
            (EventKind::PrMerged, 1),
            (EventKind::PrClosed, 1),
            (EventKind::PrReviewed, 1),
            (EventKind::PrCommented, 2),
            (EventKind::IssueOpened, 1),
            (EventKind::IssueCommented, 1),
        ]
    );
    assert_eq!(digest.totals.total(), 12);

    // People, by total descending. Dave is absent: his only commit is a second
    // before the window opens.
    // Alice merged her own PR: GitHub's PR *list* endpoint omits `merged_by`,
    // so the poller attributes the merge to the author, and this fixture says
    // what that endpoint really says. Bob and carol tie on three, and the tie
    // is broken by login.
    let people: Vec<(&str, u64)> =
        digest.people.iter().map(|p| (p.login.as_str(), p.total)).collect();
    assert_eq!(people, vec![("alice", 6), ("bob", 3), ("carol", 3)]);
    assert_eq!(
        digest.people.iter().map(|p| p.total).sum::<u64>(),
        digest.totals.total(),
        "the people's totals must add up to the window's"
    );
    assert_eq!(digest.people[0].avatar_url.as_deref(), Some(ALICE_AVATAR));
    assert_eq!(digest.people[1].avatar_url, None, "bob never carried one");

    assert_eq!(
        counts(&digest, "alice"),
        vec![
            (EventKind::Commit, 2),
            (EventKind::PrOpened, 1),
            (EventKind::PrMerged, 1),
            (EventKind::PrClosed, 0),
            (EventKind::PrReviewed, 0),
            (EventKind::PrCommented, 1),
            (EventKind::IssueOpened, 0),
            (EventKind::IssueCommented, 1),
        ]
    );
    assert_eq!(
        counts(&digest, "bob"),
        vec![
            (EventKind::Commit, 1),
            (EventKind::PrOpened, 0),
            (EventKind::PrMerged, 0),
            (EventKind::PrClosed, 0),
            (EventKind::PrReviewed, 1),
            (EventKind::PrCommented, 0),
            (EventKind::IssueOpened, 1),
            (EventKind::IssueCommented, 0),
        ]
    );
    assert_eq!(
        counts(&digest, "carol"),
        vec![
            (EventKind::Commit, 0),
            (EventKind::PrOpened, 1),
            (EventKind::PrMerged, 0),
            (EventKind::PrClosed, 1),
            (EventKind::PrReviewed, 0),
            (EventKind::PrCommented, 1),
            (EventKind::IssueOpened, 0),
            (EventKind::IssueCommented, 0),
        ]
    );

    // Threads, by event count descending. Commits are in none of them.
    let threads: Vec<(u64, u64, u64, i64)> =
        digest.threads.iter().map(|t| (t.repo_id, t.number, t.events, t.last_at)).collect();
    assert_eq!(
        threads,
        vec![
            (s.platform_id, 101, 4, s.base + 60),
            (s.web_id, 202, 3, s.base + 45),
            (s.web_id, 303, 2, s.base + 70),
        ]
    );
    assert_eq!(digest.threads[0].kind, ThreadKind::Pull);
    assert_eq!(digest.threads[1].kind, ThreadKind::Pull);
    assert_eq!(digest.threads[2].kind, ThreadKind::Issue, "issue kinds make an issue thread");

    // `title` and `url` are the newest event's. #101's newest is the merge, so
    // it carries the PR's own title and url rather than the review's
    // "Review: approved" from twenty seconds earlier.
    assert_eq!(digest.threads[0].title, PR_101_TITLE);
    assert_eq!(digest.threads[0].url, format!("https://github.com/{PLATFORM}/pull/101"));
    assert_eq!(digest.threads[1].title, PR_202_TITLE);
    assert_eq!(digest.threads[1].url, format!("https://github.com/{WEB}/pull/202"));
    assert_eq!(digest.threads[2].title, ISSUE_303_TITLE);
    // #303's newest is the comment, so the url points at the comment.
    assert_eq!(
        digest.threads[2].url,
        format!("https://github.com/{WEB}/issues/303#issuecomment-5002")
    );

    // Repos, by total descending: platform's three commits plus #101's four,
    // then web's #202 and #303.
    let repos: Vec<(u64, u64)> =
        digest.repos.iter().map(|r| (r.repo_id, r.total)).collect();
    assert_eq!(repos, vec![(s.platform_id, 7), (s.web_id, 5)]);
    assert_eq!(digest.repos.iter().map(|r| r.total).sum::<u64>(), digest.totals.total());
}

/// `[start, end)`: an event exactly at `start` counts, one exactly at `end`
/// does not — and widening the window by a single second picks it up.
#[test]
fn the_window_includes_start_and_excludes_end() {
    let s = scenario();

    let inside = s.engine.digest(s.base, s.base + 100, None, None, 0).unwrap();
    assert_eq!(inside.totals.get(EventKind::Commit), 3, "the commit at `end` is out");

    // One second wider at the top, and the commit sitting on the old boundary
    // joins in.
    let wider = s.engine.digest(s.base, s.base + 101, None, None, 0).unwrap();
    assert_eq!(wider.totals.get(EventKind::Commit), 4);
    assert_eq!(wider.people[0].total, 7, "alice picked up her third commit");

    // Starting one second later drops the commit that sat exactly on `start`.
    let later = s.engine.digest(s.base + 1, s.base + 100, None, None, 0).unwrap();
    assert_eq!(later.totals.get(EventKind::Commit), 2);

    // And a window that opens a second earlier admits dave, who is otherwise
    // never in a digest at all.
    let earlier = s.engine.digest(s.base - 1, s.base + 100, None, None, 0).unwrap();
    assert_eq!(earlier.totals.get(EventKind::Commit), 4);
    assert!(earlier.people.iter().any(|p| p.login == "dave"));
    assert!(!inside.people.iter().any(|p| p.login == "dave"));
}

/// `team_id` keeps only that team's members — and because the filter runs
/// before the aggregation, a thread's "newest event" is the newest one that
/// survived it.
#[test]
fn a_team_id_keeps_only_its_members() {
    let s = scenario();
    let platform_team =
        s.engine.create_team("Platform", &["alice".into(), "bob".into()]).unwrap();

    let digest = s.engine.digest(s.base, s.base + 100, Some(platform_team.id), None, 0).unwrap();

    assert_eq!(digest.team_id, Some(platform_team.id), "the request is echoed back");
    assert_eq!(
        totals(&digest),
        vec![
            (EventKind::Commit, 3),
            (EventKind::PrOpened, 1),
            (EventKind::PrMerged, 1),
            (EventKind::PrClosed, 0),
            (EventKind::PrReviewed, 1),
            (EventKind::PrCommented, 1),
            (EventKind::IssueOpened, 1),
            (EventKind::IssueCommented, 1),
        ],
        "carol's three events are gone"
    );

    let people: Vec<(&str, u64)> =
        digest.people.iter().map(|p| (p.login.as_str(), p.total)).collect();
    assert_eq!(people, vec![("alice", 6), ("bob", 3)]);

    // #101 loses carol's comment, #202 loses everything but alice's — and its
    // newest surviving event is now that comment, so the thread's url moves to
    // it while the title (resolved from the PR) stays.
    let threads: Vec<(u64, u64, i64)> =
        digest.threads.iter().map(|t| (t.number, t.events, t.last_at)).collect();
    assert_eq!(
        threads,
        vec![(101, 3, s.base + 60), (303, 2, s.base + 70), (202, 1, s.base + 35)]
    );
    assert_eq!(digest.threads[2].title, PR_202_TITLE);
    assert_eq!(
        digest.threads[2].url,
        format!("https://github.com/{WEB}/pull/202#discussion_r6001"),
        "the label follows the newest event that survived the filter"
    );

    let repos: Vec<(u64, u64)> = digest.repos.iter().map(|r| (r.repo_id, r.total)).collect();
    assert_eq!(repos, vec![(s.platform_id, 6), (s.web_id, 3)]);
}

/// `actor` keeps one login, case-insensitively, and echoes back what was asked
/// for rather than a normalised form.
#[test]
fn an_actor_keeps_one_login() {
    let s = scenario();

    let digest = s.engine.digest(s.base, s.base + 100, None, Some("carol"), 0).unwrap();
    assert_eq!(digest.actor.as_deref(), Some("carol"));
    assert_eq!(digest.totals.total(), 3);
    let people: Vec<(&str, u64)> =
        digest.people.iter().map(|p| (p.login.as_str(), p.total)).collect();
    assert_eq!(people, vec![("carol", 3)]);
    // Only the two threads carol touched, and only her events in them.
    let threads: Vec<(u64, u64)> =
        digest.threads.iter().map(|t| (t.number, t.events)).collect();
    assert_eq!(threads, vec![(202, 2), (101, 1)]);
    let repos: Vec<(u64, u64)> = digest.repos.iter().map(|r| (r.repo_id, r.total)).collect();
    assert_eq!(repos, vec![(s.web_id, 2), (s.platform_id, 1)]);

    // The match ignores case; the echo does not.
    let shouted = s.engine.digest(s.base, s.base + 100, None, Some("CAROL"), 0).unwrap();
    assert_eq!(shouted.actor.as_deref(), Some("CAROL"));
    assert_eq!(shouted.totals, digest.totals);
    assert_eq!(shouted.people, digest.people);
    assert_eq!(shouted.threads, digest.threads);
}

/// Given together, the two scopes intersect rather than union.
#[test]
fn a_team_and_an_actor_intersect() {
    let s = scenario();
    let platform =
        s.engine.create_team("Platform", &["alice".into(), "bob".into()]).unwrap();
    let design = s.engine.create_team("Design", &["bob".into(), "carol".into()]).unwrap();

    // In the team and the named person: carol's three events.
    let both = s.engine.digest(s.base, s.base + 100, Some(design.id), Some("carol"), 0).unwrap();
    assert_eq!(both.team_id, Some(design.id));
    assert_eq!(both.actor.as_deref(), Some("carol"));
    assert_eq!(both.totals.total(), 3);
    assert_eq!(both.people.len(), 1);
    assert_eq!(both.people[0].login, "carol");

    // Bob is in both teams, so scoping by either keeps his four events.
    for team in [&platform, &design] {
        let bob = s.engine.digest(s.base, s.base + 100, Some(team.id), Some("bob"), 0).unwrap();
        assert_eq!(bob.totals.total(), 3, "bob belongs to {}", team.name);
    }

    // Carol is not on Platform, so the intersection is empty — an empty digest,
    // not an error, and the scoping is still echoed.
    let neither =
        s.engine.digest(s.base, s.base + 100, Some(platform.id), Some("carol"), 0).unwrap();
    assert_eq!(neither.team_id, Some(platform.id));
    assert_eq!(neither.actor.as_deref(), Some("carol"));
    assert_eq!(neither.totals.total(), 0);
    assert!(neither.people.is_empty());
    assert!(neither.threads.is_empty());
    assert!(neither.repos.is_empty());
}

/// Twenty-five actors yield twenty people; the twenty-first by total is absent,
/// and the pair tied on the cut is separated by login.
#[test]
fn people_are_capped_at_twenty_with_ties_broken_by_login() {
    let mut srv = server();
    let base = chrono::Utc::now().timestamp() - 3600;

    // p01 gets 25 commits, p02 24, … p19 7; then p20 and p21 tie on 6, and
    // p22…p25 trail off. So the cut falls exactly on a tie.
    let commit_counts: Vec<(String, u64)> = (1..=25u64)
        .map(|i| {
            let count = match i {
                1..=19 => 26 - i,
                20 | 21 => 6,
                _ => 27 - i,
            };
            (format!("p{i:02}"), count)
        })
        .collect();

    let mut commits = Vec::new();
    let mut sha = 0u64;
    // Emitted last-login-first, so nothing downstream can break the tie by
    // insertion order and accidentally look right.
    for (login, count) in commit_counts.iter().rev() {
        for _ in 0..*count {
            sha += 1;
            commits.push(commit(PLATFORM, &format!("{sha:040x}"), login, None, base + 10));
        }
    }

    let mut platform = RepoMock::quiet("acme", "platform");
    platform.commits = Resp::ok(json_array(&commits));
    let _mocks = mount(&mut srv, &platform);

    let (engine, _dir) = engine_for(&srv);
    engine
        .set_settings(Settings { filter_mode: FilterMode::All, ..Settings::default() })
        .unwrap();
    engine.add_repo(PLATFORM, None).unwrap();
    let planted: u64 = commit_counts.iter().map(|(_, c)| c).sum();
    assert_eq!(engine.poll_now().new_events.len() as u64, planted);

    let digest = engine.digest(base, base + 100, None, None, 0).unwrap();

    assert_eq!(digest.people.len(), DIGEST_PEOPLE_LIMIT);
    assert_eq!(
        digest.people.iter().map(|p| p.login.as_str()).collect::<Vec<_>>(),
        (1..=20).map(|i| format!("p{i:02}")).collect::<Vec<_>>()
    );
    // p20 and p21 both did six things; p20 takes the last slot because its
    // login sorts first, and p21 — the 21st — is gone.
    assert_eq!(digest.people[19].login, "p20");
    assert_eq!(digest.people[19].total, 6);
    assert!(
        !digest.people.iter().any(|p| p.login == "p21"),
        "the 21st by total must not be listed"
    );

    // The cut trims the list, never the counting: `totals` still covers
    // everyone, including the five people who did not make it.
    assert_eq!(digest.totals.get(EventKind::Commit), planted);
    assert_eq!(digest.totals.total(), planted);
    assert!(
        digest.people.iter().map(|p| p.total).sum::<u64>() < planted,
        "this fixture is only meaningful if people were actually dropped"
    );
    // p01 wrote the most commits of anyone, so the repo's concentration names
    // her — 25 of the planted total, which is nowhere near a one-owner repo.
    assert_eq!(
        digest.repos,
        vec![gitmon::DigestRepo {
            repo_id: 1,
            total: planted,
            top_author_login: Some("p01".into()),
            top_author_share: 25.0 / planted as f32,
        }]
    );
}

/// Threads are capped at ten, ordered by event count and then, on a tie, by
/// which one was touched most recently.
#[test]
fn threads_are_capped_at_ten_by_events_then_recency() {
    let mut srv = server();
    let base = chrono::Utc::now().timestamp() - 3600;

    // Issues #1…#12 get one comment each, then two, … then twelve — so every
    // one has a distinct event count (its opening plus its comments) and the
    // ranking by count alone is unambiguous. Issue #13 then ties #12 on count
    // but was commented on ten seconds later, which is the only thing that can
    // separate them.
    let mut issues = Vec::new();
    let mut comments = Vec::new();
    let mut comment_id = 0u64;
    for number in 1..=13u64 {
        let (count, at) = if number == 13 { (12, base + 20) } else { (number, base + 10) };
        issues.push(issue(PLATFORM, number, "alice", &format!("Issue {number}"), base + 1));
        for _ in 0..count {
            comment_id += 1;
            comments.push(issue_comment(PLATFORM, number, comment_id, "alice", at, false));
        }
    }

    let mut platform = RepoMock::quiet("acme", "platform");
    platform.issues = Resp::ok(json_array(&issues));
    platform.issue_comments = Resp::ok(json_array(&comments));
    let _mocks = mount(&mut srv, &platform);

    let (engine, _dir) = engine_for(&srv);
    engine
        .set_settings(Settings { filter_mode: FilterMode::All, ..Settings::default() })
        .unwrap();
    engine.add_repo(PLATFORM, None).unwrap();
    engine.poll_now();

    let digest = engine.digest(base, base + 100, None, None, 0).unwrap();

    assert_eq!(digest.threads.len(), DIGEST_THREAD_LIMIT);
    // #13 and #12 both have thirteen events; #13 leads because its last event
    // is newer. Then #11 down to #4 by count, and #3, #2 and #1 are cut.
    let mut expected: Vec<(u64, u64, i64)> =
        vec![(13, 13, base + 20), (12, 13, base + 10)];
    expected.extend((4..=11u64).rev().map(|n| (n, n + 1, base + 10)));
    assert_eq!(
        digest.threads.iter().map(|t| (t.number, t.events, t.last_at)).collect::<Vec<_>>(),
        expected
    );

    // The cut trims the list, never the counting: the three quietest threads
    // are gone from `threads` but their events are still in `totals`.
    let planted = 13 + 12 + (1..=12u64).sum::<u64>();
    assert_eq!(digest.totals.get(EventKind::IssueOpened), 13);
    assert_eq!(digest.totals.total(), planted);
    assert!(
        digest.threads.iter().map(|t| t.events).sum::<u64>() < planted,
        "this fixture is only meaningful if threads were actually dropped"
    );
}

/// A window that ends where it starts, or before it, is `invalid`.
#[test]
fn an_end_at_or_before_start_is_invalid() {
    let s = scenario();

    for (start, end) in [(s.base, s.base), (s.base + 100, s.base), (0, -1)] {
        let error = s
            .engine
            .digest(start, end, None, None, 0)
            .expect_err("a window with no duration must be rejected");
        assert_eq!(error.kind, gitmon::ErrorKind::Invalid, "start {start}, end {end}");
    }

    // A one-second window is the narrowest legal one.
    assert!(s.engine.digest(s.base, s.base + 1, None, None, 0).is_ok());
}

/// A `team_id` matching no team is an empty digest with zeroed totals, not an
/// error — and so is a team that exists but has nobody in it.
#[test]
fn an_unknown_or_empty_team_is_an_empty_digest() {
    let s = scenario();
    let empty_team = s.engine.create_team("Nobody", &[]).unwrap();

    for team_id in [9999, empty_team.id] {
        let digest = s
            .engine
            .digest(s.base, s.base + 100, Some(team_id), None, 0)
            .expect("an unknown team is not an error");

        assert_eq!(digest.team_id, Some(team_id));
        assert_eq!(digest.start, s.base);
        assert_eq!(digest.end, s.base + 100);
        assert!(digest.people.is_empty());
        assert!(digest.threads.is_empty());
        assert!(digest.repos.is_empty());
        // Every kind is still present, all zero.
        assert_eq!(
            totals(&digest),
            EventKind::ALL.into_iter().map(|kind| (kind, 0)).collect::<Vec<_>>()
        );
        assert_eq!(digest.totals.total(), 0);
    }
}

/// GitHub preserves a login's casing and teams store it lowercased, so both
/// scopes have to match case-insensitively — and one person spelled two ways is
/// one person.
#[test]
fn scoping_and_grouping_ignore_the_casing_of_a_login() {
    let mut srv = server();
    let base = chrono::Utc::now().timestamp() - 3600;

    let mut platform = RepoMock::quiet("acme", "platform");
    platform.commits = Resp::ok(json_array(&[
        commit(PLATFORM, "c001", "AliceNg", Some(ALICE_AVATAR), base + 1),
        commit(PLATFORM, "c002", "AliceNg", Some(ALICE_AVATAR), base + 2),
        // The same person, as GitHub happened to spell her that day.
        commit(PLATFORM, "c003", "aliceng", None, base + 3),
        commit(PLATFORM, "c004", "Bob", None, base + 4),
    ]));
    let _mocks = mount(&mut srv, &platform);

    let (engine, _dir) = engine_for(&srv);
    engine
        .set_settings(Settings { filter_mode: FilterMode::All, ..Settings::default() })
        .unwrap();
    engine.add_repo(PLATFORM, None).unwrap();
    assert_eq!(engine.poll_now().new_events.len(), 4);

    // Unscoped: two people, not three.
    let all = engine.digest(base, base + 100, None, None, 0).unwrap();
    assert_eq!(all.people.len(), 2, "two spellings are one person: {:?}", all.people);
    assert_eq!(all.people[0].total, 3);
    assert_eq!(all.people[0].login.to_lowercase(), "aliceng");
    assert_eq!(all.people[0].avatar_url.as_deref(), Some(ALICE_AVATAR));

    // `create_team` lowercases, so the team holds "aliceng" while the events
    // mostly say "AliceNg". Both spellings must still be hers.
    let team = engine.create_team("Platform", &["AliceNg".into()]).unwrap();
    assert_eq!(team.logins, vec!["aliceng"]);
    let scoped = engine.digest(base, base + 100, Some(team.id), None, 0).unwrap();
    assert_eq!(scoped.totals.get(EventKind::Commit), 3, "all three of hers, none of bob's");
    assert_eq!(scoped.people.len(), 1);
    assert_eq!(scoped.people[0].total, 3);

    // And the `actor` scope matches the same way, however it is spelled.
    for spelling in ["AliceNg", "aliceng", "ALICENG"] {
        let one = engine.digest(base, base + 100, None, Some(spelling), 0).unwrap();
        assert_eq!(one.totals.total(), 3, "actor {spelling:?}");
    }
    // Team and actor together, each case-folded independently.
    let both = engine.digest(base, base + 100, Some(team.id), Some("ALICENG"), 0).unwrap();
    assert_eq!(both.totals.total(), 3);
    let outsider = engine.digest(base, base + 100, Some(team.id), Some("bob"), 0).unwrap();
    assert_eq!(outsider.totals.total(), 0, "bob is not on the team");
}

/// A digest never reaches GitHub: it answers from the store even when every
/// endpoint is gone.
#[test]
fn a_digest_reads_the_store_and_never_the_network() {
    let s = scenario();
    let before = s.engine.digest(s.base, s.base + 100, None, None, 0).unwrap();

    // Shut the fixture server down outright, so there is nothing left to talk
    // to at the TCP level — not merely an endpoint that answers differently.
    drop(s._mocks);
    drop(s._server);
    assert_eq!(
        s.engine.poll_now().errors.len(),
        2,
        "both repos must now fail, or this test is not proving GitHub is unreachable"
    );

    let after = s.engine.digest(s.base, s.base + 100, None, None, 0).unwrap();
    assert_eq!(after, before, "the digest is unchanged by an unreachable GitHub");
}
