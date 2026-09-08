//! The wire shapes. Every assertion here is a literal copy of the JSON in
//! `docs/CONTRACT.md`; if a key is renamed or reordered, this file fails.

mod common;

use common::{engine_for, fixture, mock_get, mount, server, RepoMock, Resp};
use gitmon::{
    Account, BackfillResult, CommitDetail, CommitFile, DayCounts, Digest, DigestPerson, DigestRepo,
    DigestThread, EngineError, ErrorKind, Event, EventKind, FilterMode, KindCounts, OpenPull,
    PersonSource,
    PersonSuggestion, PollOptions, PollResult, PrTiming, QuietHours, Repo, RepoBackfill, RepoError,
    RepoSuggestion,
    Settings, Team, Thread, ThreadItem, ThreadItemKind, ThreadKind, Watch, WatchSource,
    UNSAVED_TEAM_ID,
};
use serde::{de::DeserializeOwned, Serialize};

/// A `KindCounts` with only the named kinds set; the rest stay zero.
fn counts(pairs: &[(EventKind, u64)]) -> KindCounts {
    let mut counts = KindCounts::default();
    for (kind, count) in pairs {
        counts.set(*kind, *count);
    }
    counts
}

/// Serializes to exactly `expected`, and survives a round trip.
fn assert_shape<T>(value: &T, expected: &str)
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let json = serde_json::to_string(value).expect("serializes");
    assert_eq!(json, expected);
    let back: T = serde_json::from_str(&json).expect("deserializes");
    assert_eq!(&back, value, "round trip changed the value");
}

/// 10a. Every contract type, one instance each.
#[test]
fn every_type_matches_the_contract_json() {
    assert_shape(
        &Team { id: 1, name: "Platform".into(), logins: vec!["alice".into(), "bob".into()] },
        r#"{"id":1,"name":"Platform","logins":["alice","bob"]}"#,
    );
    // id 0 is the not-yet-saved team `import_org_team` hands back.
    assert_shape(
        &Team { id: UNSAVED_TEAM_ID, name: "platform".into(), logins: vec![] },
        r#"{"id":0,"name":"platform","logins":[]}"#,
    );

    assert_shape(
        &Account {
            login: "alice".into(),
            avatar_url: Some("https://avatars.githubusercontent.com/u/1?v=4".into()),
            added_at: 1_760_000_000,
        },
        r#"{"login":"alice","avatar_url":"https://avatars.githubusercontent.com/u/1?v=4","added_at":1760000000}"#,
    );
    // An account whose avatar GitHub did not give us serialises the key as null
    // rather than dropping it, like every other optional field.
    assert_shape(
        &Account { login: "bob".into(), avatar_url: None, added_at: 1_760_000_001 },
        r#"{"login":"bob","avatar_url":null,"added_at":1760000001}"#,
    );

    assert_shape(
        &Repo {
            id: 1,
            owner: "acme".into(),
            name: "platform".into(),
            url: "https://github.com/acme/platform".into(),
            account_login: "alice".into(),
            default_branch: "main".into(),
            last_polled_at: None,
            backfilled_to: None,
            last_error: None,
            hidden: false,
        },
        r#"{"id":1,"owner":"acme","name":"platform","url":"https://github.com/acme/platform","account_login":"alice","default_branch":"main","last_polled_at":null,"backfilled_to":null,"last_error":null,"hidden":false}"#,
    );
    // A repo no account has claimed yet — every repo a pre-v5 install added,
    // until the first `add_account` takes them — and, here, one the user has
    // hidden: still watched, still holding everything already collected, just
    // out of every view and every poll until it is unhidden.
    assert_shape(
        &Repo {
            id: 2,
            owner: "acme".into(),
            name: "web".into(),
            url: "https://github.com/acme/web".into(),
            account_login: String::new(),
            default_branch: "main".into(),
            last_polled_at: Some(1_760_000_000),
            backfilled_to: Some(1_759_913_600),
            last_error: Some("signed out".into()),
            hidden: true,
        },
        r#"{"id":2,"owner":"acme","name":"web","url":"https://github.com/acme/web","account_login":"","default_branch":"main","last_polled_at":1760000000,"backfilled_to":1759913600,"last_error":"signed out","hidden":true}"#,
    );

    assert_shape(&FilterMode::All, r#""all""#);
    assert_shape(&FilterMode::Team, r#""team""#);

    let kinds = [
        (EventKind::Commit, r#""commit""#),
        (EventKind::PrOpened, r#""pr_opened""#),
        (EventKind::PrMerged, r#""pr_merged""#),
        (EventKind::PrClosed, r#""pr_closed""#),
        (EventKind::PrReviewed, r#""pr_reviewed""#),
        (EventKind::PrCommented, r#""pr_commented""#),
        (EventKind::IssueOpened, r#""issue_opened""#),
        (EventKind::IssueCommented, r#""issue_commented""#),
    ];
    for (kind, expected) in kinds {
        assert_shape(&kind, expected);
    }
    assert_eq!(kinds.len(), EventKind::ALL.len(), "a kind is missing from this test");

    let event = Event {
        id: 7,
        repo_id: 1,
        kind: EventKind::PrMerged,
        actor_login: "bob".into(),
        actor_avatar_url: None,
        title: "Bundle rusqlite".into(),
        body_preview: None,
        body: None,
        url: "https://github.com/acme/platform/pull/102".into(),
        number: Some(102),
        occurred_at: 1_760_000_000,
        seen: false,
        team_ids: vec![],
        watched: false,
    };
    assert_shape(
        &event,
        r#"{"id":7,"repo_id":1,"kind":"pr_merged","actor_login":"bob","actor_avatar_url":null,"title":"Bundle rusqlite","body_preview":null,"body":null,"url":"https://github.com/acme/platform/pull/102","number":102,"occurred_at":1760000000,"seen":false,"team_ids":[],"watched":false}"#,
    );

    // An actor on two teams carries both ids, in team display order.
    assert_shape(
        &Event { team_ids: vec![1, 2], ..event.clone() },
        r#"{"id":7,"repo_id":1,"kind":"pr_merged","actor_login":"bob","actor_avatar_url":null,"title":"Bundle rusqlite","body_preview":null,"body":null,"url":"https://github.com/acme/platform/pull/102","number":102,"occurred_at":1760000000,"seen":false,"team_ids":[1,2],"watched":false}"#,
    );

    // `watched` sits last, after `team_ids`, and is the other field resolved at
    // read time rather than stored.
    assert_shape(
        &Event { watched: true, ..event.clone() },
        r#"{"id":7,"repo_id":1,"kind":"pr_merged","actor_login":"bob","actor_avatar_url":null,"title":"Bundle rusqlite","body_preview":null,"body":null,"url":"https://github.com/acme/platform/pull/102","number":102,"occurred_at":1760000000,"seen":false,"team_ids":[],"watched":true}"#,
    );

    // The same event carrying text: `body` is the untruncated markdown and
    // `body_preview` the collapsed one-liner beside it.
    assert_shape(
        &Event {
            body_preview: Some("Keeps the bundle self-contained.".into()),
            body: Some("Keeps the bundle\nself-contained.".into()),
            ..event.clone()
        },
        r#"{"id":7,"repo_id":1,"kind":"pr_merged","actor_login":"bob","actor_avatar_url":null,"title":"Bundle rusqlite","body_preview":"Keeps the bundle self-contained.","body":"Keeps the bundle\nself-contained.","url":"https://github.com/acme/platform/pull/102","number":102,"occurred_at":1760000000,"seen":false,"team_ids":[],"watched":false}"#,
    );

    assert_shape(&ThreadKind::Pull, r#""pull""#);
    assert_shape(&ThreadKind::Issue, r#""issue""#);
    for (kind, expected) in [
        (ThreadItemKind::Comment, r#""comment""#),
        (ThreadItemKind::Review, r#""review""#),
        (ThreadItemKind::ReviewComment, r#""review_comment""#),
        (ThreadItemKind::Commit, r#""commit""#),
    ] {
        assert_shape(&kind, expected);
    }

    let item = ThreadItem {
        kind: ThreadItemKind::ReviewComment,
        actor_login: "bob".into(),
        actor_avatar_url: None,
        body: "Nit: rename this.".to_string().into(),
        state: None,
        path: Some("src/poll.rs".into()),
        line: Some(42),
        url: "https://github.com/acme/platform/pull/101#discussion_r1".into(),
        at: 1_760_000_100,
        sha: None,
    };
    assert_shape(
        &item,
        r#"{"kind":"review_comment","actor_login":"bob","actor_avatar_url":null,"body":"Nit: rename this.","state":null,"path":"src/poll.rs","line":42,"url":"https://github.com/acme/platform/pull/101#discussion_r1","at":1760000100,"sha":null}"#,
    );

    // A commit item is the one kind that carries a sha; `sha` is the last key.
    assert_shape(
        &ThreadItem {
            kind: ThreadItemKind::Commit,
            path: None,
            line: None,
            body: Some("Split the poller out".into()),
            sha: Some("c0ffee1".into()),
            ..item.clone()
        },
        r#"{"kind":"commit","actor_login":"bob","actor_avatar_url":null,"body":"Split the poller out","state":null,"path":null,"line":null,"url":"https://github.com/acme/platform/pull/101#discussion_r1","at":1760000100,"sha":"c0ffee1"}"#,
    );

    assert_shape(
        &Thread {
            repo_id: 1,
            number: 101,
            kind: ThreadKind::Pull,
            title: "Split the poller out of the engine".into(),
            state: "open".into(),
            author_login: "alice".into(),
            author_avatar_url: None,
            body: Some("Moves fetch/derive into its own module.".into()),
            url: "https://github.com/acme/platform/pull/101".into(),
            created_at: 1_760_000_000,
            head_ref: Some("split-poller".into()),
            base_ref: Some("main".into()),
            additions: Some(120),
            deletions: Some(30),
            changed_files: Some(4),
            items: vec![item],
        },
        r#"{"repo_id":1,"number":101,"kind":"pull","title":"Split the poller out of the engine","state":"open","author_login":"alice","author_avatar_url":null,"body":"Moves fetch/derive into its own module.","url":"https://github.com/acme/platform/pull/101","created_at":1760000000,"head_ref":"split-poller","base_ref":"main","additions":120,"deletions":30,"changed_files":4,"items":[{"kind":"review_comment","actor_login":"bob","actor_avatar_url":null,"body":"Nit: rename this.","state":null,"path":"src/poll.rs","line":42,"url":"https://github.com/acme/platform/pull/101#discussion_r1","at":1760000100,"sha":null}]}"#,
    );

    assert_shape(
        &CommitFile {
            path: "src/store.rs".into(),
            status: "modified".into(),
            additions: 10,
            deletions: 2,
            patch: None,
        },
        r#"{"path":"src/store.rs","status":"modified","additions":10,"deletions":2,"patch":null}"#,
    );

    assert_shape(
        &CommitDetail {
            repo_id: 1,
            sha: "c0ffee1".into(),
            message: "Add sparkline".into(),
            author_login: "alice".into(),
            author_avatar_url: None,
            url: "https://github.com/acme/platform/commit/c0ffee1".into(),
            at: 1_760_000_000,
            additions: 10,
            deletions: 2,
            files: vec![],
        },
        r#"{"repo_id":1,"sha":"c0ffee1","message":"Add sparkline","author_login":"alice","author_avatar_url":null,"url":"https://github.com/acme/platform/commit/c0ffee1","at":1760000000,"additions":10,"deletions":2,"files":[]}"#,
    );

    assert_shape(
        &RepoError { repo_id: 1, message: "network: timed out".into(), reset_at: None },
        r#"{"repo_id":1,"message":"network: timed out","reset_at":null}"#,
    );
    // Only `rate_limited` carries a `reset_at`.
    assert_shape(
        &RepoError {
            repo_id: 1,
            message: "rate_limited: API rate limit exceeded".into(),
            reset_at: Some(1_760_003_600),
        },
        r#"{"repo_id":1,"message":"rate_limited: API rate limit exceeded","reset_at":1760003600}"#,
    );

    assert_shape(
        &PollResult {
            new_events: vec![],
            errors: vec![],
            rate_limit_remaining: None,
            rate_limit_reset_at: None,
            rate_limit_limit: None,
            skipped: vec![],
        },
        r#"{"new_events":[],"errors":[],"rate_limit_remaining":null,"rate_limit_reset_at":null,"rate_limit_limit":null,"skipped":[]}"#,
    );
    // A poll that GitHub's own `X-Poll-Interval` told it to leave alone names
    // the repo in `skipped` — never in `errors`, which is for failures.
    assert_shape(
        &PollResult {
            new_events: vec![],
            errors: vec![],
            rate_limit_remaining: Some(4999),
            rate_limit_reset_at: Some(1_760_003_600),
            rate_limit_limit: Some(5000),
            skipped: vec![7, 9],
        },
        r#"{"new_events":[],"errors":[],"rate_limit_remaining":4999,"rate_limit_reset_at":1760003600,"rate_limit_limit":5000,"skipped":[7,9]}"#,
    );
    assert_shape(&PollOptions::default(), r#"{"force":false}"#);
    assert_shape(&PollOptions { force: true }, r#"{"force":true}"#);

    // One clean backfill step and one that could not run: `error` is null on
    // the first and a string on the second, and a repo with no floor reports
    // the window it never chose as 0/0.
    assert_shape(
        &RepoBackfill {
            repo_id: 1,
            from: 1_759_395_200,
            to: 1_760_000_000,
            inserted: 12,
            error: None,
        },
        r#"{"repo_id":1,"from":1759395200,"to":1760000000,"inserted":12,"error":null}"#,
    );
    assert_shape(
        &RepoBackfill {
            repo_id: 2,
            from: 0,
            to: 0,
            inserted: 0,
            error: Some("not polled yet".into()),
        },
        r#"{"repo_id":2,"from":0,"to":0,"inserted":0,"error":"not polled yet"}"#,
    );
    assert_shape(
        &BackfillResult {
            repos: vec![],
            rate_limit_remaining: None,
            rate_limit_reset_at: None,
            rate_limit_limit: None,
        },
        r#"{"repos":[],"rate_limit_remaining":null,"rate_limit_reset_at":null,"rate_limit_limit":null}"#,
    );
    assert_shape(
        &BackfillResult {
            repos: vec![RepoBackfill {
                repo_id: 1,
                from: 1_759_395_200,
                to: 1_760_000_000,
                inserted: 3,
                error: Some("window truncated".into()),
            }],
            rate_limit_remaining: Some(4931),
            rate_limit_reset_at: Some(1_760_003_600),
            rate_limit_limit: Some(5000),
        },
        r#"{"repos":[{"repo_id":1,"from":1759395200,"to":1760000000,"inserted":3,"error":"window truncated"}],"rate_limit_remaining":4931,"rate_limit_reset_at":1760003600,"rate_limit_limit":5000}"#,
    );

    assert_shape(
        &QuietHours { start_minute: 1380, end_minute: 420 },
        r#"{"start_minute":1380,"end_minute":420}"#,
    );

    // The documented defaults: 120s, "team", notifications on, every kind,
    // auto-watch on.
    assert_shape(
        &Settings::default(),
        r#"{"poll_interval_secs":120,"filter_mode":"team","notifications_enabled":true,"notify_kinds":["commit","pr_opened","pr_merged","pr_closed","pr_reviewed","pr_commented","issue_opened","issue_commented"],"quiet_hours":null,"auto_watch":true}"#,
    );

    for (source, expected) in [
        (WatchSource::Manual, r#""manual""#),
        (WatchSource::Author, r#""author""#),
        (WatchSource::Reviewer, r#""reviewer""#),
    ] {
        assert_shape(&source, expected);
    }

    assert_shape(
        &Watch {
            repo_id: 1,
            number: 101,
            kind: ThreadKind::Pull,
            title: "Split the poller out of the engine".into(),
            state: "open".into(),
            source: WatchSource::Manual,
            since: 1_760_000_000,
        },
        r#"{"repo_id":1,"number":101,"kind":"pull","title":"Split the poller out of the engine","state":"open","source":"manual","since":1760000000}"#,
    );
    // An auto-watched issue-shaped row: every other value the contract allows.
    assert_shape(
        &Watch {
            repo_id: 2,
            number: 5,
            kind: ThreadKind::Issue,
            title: "Tray icon is blurry".into(),
            state: "merged".into(),
            source: WatchSource::Reviewer,
            since: 1_760_000_300,
        },
        r#"{"repo_id":2,"number":5,"kind":"issue","title":"Tray icon is blurry","state":"merged","source":"reviewer","since":1760000300}"#,
    );

    assert_shape(
        &PersonSuggestion {
            login: "alice".into(),
            avatar_url: None,
            source: PersonSource::Contributor,
            why: "41 commits · acme/platform".into(),
        },
        r#"{"login":"alice","avatar_url":null,"source":"contributor","why":"41 commits · acme/platform"}"#,
    );
    assert_shape(&PersonSource::OrgMember, r#""org_member""#);
    assert_shape(&PersonSource::Bot, r#""bot""#);

    assert_shape(
        &RepoSuggestion {
            owner: "acme".into(),
            name: "platform".into(),
            why: "you pushed 2 days ago".into(),
        },
        r#"{"owner":"acme","name":"platform","why":"you pushed 2 days ago"}"#,
    );

    // Every kind is present in `totals` and `counts`, zeros included, in the
    // contract's declaration order — not the alphabetical order a BTreeMap
    // would produce.
    let zeroed = r#"{"commit":0,"pr_opened":0,"pr_merged":0,"pr_closed":0,"pr_reviewed":0,"pr_commented":0,"issue_opened":0,"issue_commented":0}"#;
    assert_shape(&KindCounts::default(), zeroed);

    let person = DigestPerson {
        login: "alice".into(),
        avatar_url: None,
        total: 6,
        counts: counts(&[(EventKind::Commit, 4), (EventKind::PrMerged, 2)]),
        open_prs: 2,
        review_queue: 3,
        last_at: 1_760_000_250,
    };
    assert_shape(
        &person,
        r#"{"login":"alice","avatar_url":null,"total":6,"counts":{"commit":4,"pr_opened":0,"pr_merged":2,"pr_closed":0,"pr_reviewed":0,"pr_commented":0,"issue_opened":0,"issue_commented":0},"open_prs":2,"review_queue":3,"last_at":1760000250}"#,
    );

    let thread = DigestThread {
        repo_id: 1,
        number: 101,
        kind: ThreadKind::Pull,
        title: "Split the poller out of the engine".into(),
        url: "https://github.com/acme/platform/pull/101".into(),
        events: 3,
        last_at: 1_760_000_300,
    };
    assert_shape(
        &thread,
        r#"{"repo_id":1,"number":101,"kind":"pull","title":"Split the poller out of the engine","url":"https://github.com/acme/platform/pull/101","events":3,"last_at":1760000300}"#,
    );

    assert_shape(
        &DigestRepo {
            repo_id: 1,
            total: 9,
            top_author_login: Some("alice".into()),
            top_author_share: 0.75,
        },
        r#"{"repo_id":1,"total":9,"top_author_login":"alice","top_author_share":0.75}"#,
    );
    // A repo whose window holds no commits at all: nobody to name, and a share
    // of zero rather than a made-up one.
    assert_shape(
        &DigestRepo { repo_id: 2, total: 4, top_author_login: None, top_author_share: 0.0 },
        r#"{"repo_id":2,"total":4,"top_author_login":null,"top_author_share":0.0}"#,
    );

    let day = DayCounts {
        day: 1_760_000_000,
        counts: counts(&[(EventKind::Commit, 4), (EventKind::PrMerged, 2)]),
    };
    assert_shape(
        &day,
        r#"{"day":1760000000,"counts":{"commit":4,"pr_opened":0,"pr_merged":2,"pr_closed":0,"pr_reviewed":0,"pr_commented":0,"issue_opened":0,"issue_commented":0}}"#,
    );

    assert_shape(
        &PrTiming {
            median_ttm_secs: Some(86_400),
            median_ttfr_secs: Some(3_600),
            p90_ttm_secs: Some(259_200),
            p90_ttfr_secs: Some(28_800),
            samples: 7,
        },
        r#"{"median_ttm_secs":86400,"median_ttfr_secs":3600,"p90_ttm_secs":259200,"p90_ttfr_secs":28800,"samples":7}"#,
    );
    // Nothing merged and nothing reviewed: every figure is null rather than
    // zero, which would read as "instant".
    assert_shape(
        &PrTiming::default(),
        r#"{"median_ttm_secs":null,"median_ttfr_secs":null,"p90_ttm_secs":null,"p90_ttfr_secs":null,"samples":0}"#,
    );

    // A 7x24 grid of zeros, as an empty window produces.
    let empty_hours: Vec<Vec<u64>> = vec![vec![0; 24]; 7];
    let empty_hours_json = format!(
        "[{}]",
        vec![format!("[{}]", vec!["0"; 24].join(","))
            ; 7]
        .join(",")
    );

    assert_shape(
        &Digest {
            start: 1_760_000_000,
            end: 1_760_086_400,
            team_id: Some(1),
            actor: Some("alice".into()),
            totals: counts(&[(EventKind::Commit, 4), (EventKind::PrMerged, 2)]),
            people: vec![person],
            people_total: 34,
            threads: vec![thread],
            repos: vec![DigestRepo {
                repo_id: 1,
                total: 6,
                top_author_login: Some("alice".into()),
                top_author_share: 1.0,
            }],
            series: vec![day],
            hours: empty_hours.clone(),
            pr_timing: PrTiming {
                median_ttm_secs: Some(86_400),
                median_ttfr_secs: None,
                p90_ttm_secs: Some(86_400),
                p90_ttfr_secs: None,
                samples: 1,
            },
            unreviewed_merges: 1,
        },
        &format!(
            r#"{{"start":1760000000,"end":1760086400,"team_id":1,"actor":"alice","totals":{{"commit":4,"pr_opened":0,"pr_merged":2,"pr_closed":0,"pr_reviewed":0,"pr_commented":0,"issue_opened":0,"issue_commented":0}},"people":[{{"login":"alice","avatar_url":null,"total":6,"counts":{{"commit":4,"pr_opened":0,"pr_merged":2,"pr_closed":0,"pr_reviewed":0,"pr_commented":0,"issue_opened":0,"issue_commented":0}},"open_prs":2,"review_queue":3,"last_at":1760000250}}],"people_total":34,"threads":[{{"repo_id":1,"number":101,"kind":"pull","title":"Split the poller out of the engine","url":"https://github.com/acme/platform/pull/101","events":3,"last_at":1760000300}}],"repos":[{{"repo_id":1,"total":6,"top_author_login":"alice","top_author_share":1.0}}],"series":[{{"day":1760000000,"counts":{{"commit":4,"pr_opened":0,"pr_merged":2,"pr_closed":0,"pr_reviewed":0,"pr_commented":0,"issue_opened":0,"issue_commented":0}}}}],"hours":{empty_hours_json},"pr_timing":{{"median_ttm_secs":86400,"median_ttfr_secs":null,"p90_ttm_secs":86400,"p90_ttfr_secs":null,"samples":1}},"unreviewed_merges":1}}"#
        ),
    );

    // Unscoped, and empty: both optional scopes are null and every list is [],
    // which is the shape an empty window and an unknown team both produce. The
    // heatmap is the exception — it is always 7x24, zeros and all.
    assert_shape(
        &Digest {
            start: 1_760_000_000,
            end: 1_760_086_400,
            team_id: None,
            actor: None,
            totals: KindCounts::default(),
            people: vec![],
            people_total: 0,
            threads: vec![],
            repos: vec![],
            series: vec![],
            hours: empty_hours,
            pr_timing: PrTiming::default(),
            unreviewed_merges: 0,
        },
        &format!(
            r#"{{"start":1760000000,"end":1760086400,"team_id":null,"actor":null,"totals":{zeroed},"people":[],"people_total":0,"threads":[],"repos":[],"series":[],"hours":{empty_hours_json},"pr_timing":{{"median_ttm_secs":null,"median_ttfr_secs":null,"p90_ttm_secs":null,"p90_ttfr_secs":null,"samples":0}},"unreviewed_merges":0}}"#
        ),
    );

    assert_shape(
        &OpenPull {
            repo_id: 1,
            number: 101,
            title: "Split the poller out of the engine".into(),
            url: "https://github.com/acme/platform/pull/101".into(),
            author_login: "alice".into(),
            author_avatar_url: Some("https://avatars.githubusercontent.com/u/1?v=4".into()),
            draft: false,
            created_at: 1_760_000_000,
            updated_at: 1_760_000_300,
            last_activity_at: 1_760_000_400,
            additions: Some(180),
            deletions: Some(24),
            requested_reviewers: vec!["bob".into(), "carol".into()],
            review_decision: Some("review_required".into()),
        },
        r#"{"repo_id":1,"number":101,"title":"Split the poller out of the engine","url":"https://github.com/acme/platform/pull/101","author_login":"alice","author_avatar_url":"https://avatars.githubusercontent.com/u/1?v=4","draft":false,"created_at":1760000000,"updated_at":1760000300,"last_activity_at":1760000400,"additions":180,"deletions":24,"requested_reviewers":["bob","carol"],"review_decision":"review_required"}"#,
    );
    // A draft nobody has been asked to review: every optional field null, and
    // `requested_reviewers` an empty array rather than a missing key.
    assert_shape(
        &OpenPull {
            repo_id: 2,
            number: 7,
            title: "Sketch the sharding plan".into(),
            url: "https://github.com/acme/web/pull/7".into(),
            author_login: "bob".into(),
            author_avatar_url: None,
            draft: true,
            created_at: 1_760_000_000,
            updated_at: 1_760_000_000,
            last_activity_at: 1_760_000_000,
            additions: None,
            deletions: None,
            requested_reviewers: vec![],
            review_decision: None,
        },
        r#"{"repo_id":2,"number":7,"title":"Sketch the sharding plan","url":"https://github.com/acme/web/pull/7","author_login":"bob","author_avatar_url":null,"draft":true,"created_at":1760000000,"updated_at":1760000000,"last_activity_at":1760000000,"additions":null,"deletions":null,"requested_reviewers":[],"review_decision":null}"#,
    );

    assert_shape(
        &EngineError::rate_limited("slow down", Some(1_760_000_000)),
        r#"{"kind":"rate_limited","message":"slow down","reset_at":1760000000}"#,
    );
    assert_shape(
        &EngineError::not_found("no such repo"),
        r#"{"kind":"not_found","message":"no such repo","reset_at":null}"#,
    );
    for (kind, expected) in [
        (ErrorKind::Auth, r#""auth""#),
        (ErrorKind::NotFound, r#""not_found""#),
        (ErrorKind::RateLimited, r#""rate_limited""#),
        (ErrorKind::Network, r#""network""#),
        (ErrorKind::Storage, r#""storage""#),
        (ErrorKind::Invalid, r#""invalid""#),
    ] {
        assert_shape(&kind, expected);
    }
}

/// 10b. The interval floor is enforced.
#[test]
fn a_poll_interval_below_the_floor_is_invalid() {
    let srv = server();
    let (engine, _dir) = engine_for(&srv);

    let err = engine
        .set_settings(Settings { poll_interval_secs: 10, ..Settings::default() })
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Invalid);
    // The rejected value was not stored.
    assert_eq!(engine.get_settings(), Settings::default());

    // The floor itself is accepted.
    let at_floor = Settings { poll_interval_secs: 30, ..Settings::default() };
    engine.set_settings(at_floor.clone()).unwrap();
    assert_eq!(engine.get_settings(), at_floor);

    let full = Settings {
        poll_interval_secs: 600,
        filter_mode: FilterMode::All,
        notifications_enabled: false,
        notify_kinds: vec![EventKind::PrMerged, EventKind::PrReviewed],
        quiet_hours: Some(QuietHours { start_minute: 1380, end_minute: 420 }),
        auto_watch: false,
    };
    engine.set_settings(full.clone()).unwrap();
    assert_eq!(engine.get_settings(), full, "settings must survive the store");
}

/// Settings written before schema v4 have no `auto_watch` key at all. The
/// contract's default is on, so the missing key must read back as `true` — a
/// bare `Default` on the field would give `false` and quietly turn auto-watch
/// off for every existing install.
#[test]
fn settings_without_auto_watch_deserialise_with_it_on() {
    let pre_v4 = r#"{"poll_interval_secs":300,"filter_mode":"all","notifications_enabled":false,
                     "notify_kinds":["commit"],"quiet_hours":null}"#;
    let settings: Settings = serde_json::from_str(pre_v4).expect("pre-v4 settings deserialise");
    assert!(settings.auto_watch, "a missing auto_watch key must default to true");
    // And nothing else drifted while the key was absent.
    assert_eq!(settings.poll_interval_secs, 300);
    assert_eq!(settings.filter_mode, FilterMode::All);
    assert!(!settings.notifications_enabled);
    assert_eq!(settings.notify_kinds, vec![EventKind::Commit]);

    // An explicit `false` is still honoured: the default only fills a gap.
    let explicit = r#"{"poll_interval_secs":120,"filter_mode":"team","notifications_enabled":true,
                       "notify_kinds":[],"quiet_hours":null,"auto_watch":false}"#;
    let settings: Settings = serde_json::from_str(explicit).unwrap();
    assert!(!settings.auto_watch);
}

/// The same thing through the store: a database whose settings row predates v4
/// comes back with auto-watch on, and a later write round-trips the key.
#[test]
fn a_pre_v4_settings_row_reads_back_with_auto_watch_on() {
    let srv = server();
    let (engine, _dir) = engine_for(&srv);
    assert!(engine.get_settings().auto_watch, "a fresh install defaults to on");

    let off = Settings { auto_watch: false, ..Settings::default() };
    engine.set_settings(off.clone()).unwrap();
    assert_eq!(engine.get_settings(), off, "an explicit false survives the store");
}

#[test]
fn settings_and_teams_persist_across_reopen() {
    let mut srv = server();
    let _mocks = mount(&mut srv, &RepoMock::busy("acme", "platform"));
    let dir = tempfile::tempdir().unwrap();
    let settings = Settings { poll_interval_secs: 900, ..Settings::default() };

    let platform_id = {
        let engine = gitmon::Engine::open_with_base_url(dir.path(), &srv.url()).unwrap();
        engine.set_token(Some("test-credential".into()));
        engine.set_settings(settings.clone()).unwrap();
        let platform = engine.create_team("Platform", &["Alice".to_string()]).unwrap();
        engine.create_team("Infra", &["bob".to_string()]).unwrap();
        engine.add_repo("acme/platform", None).unwrap();
        engine.poll_now();
        platform.id
    };

    let reopened = gitmon::Engine::open_with_base_url(dir.path(), &srv.url()).unwrap();
    assert_eq!(reopened.get_settings(), settings);

    let teams = reopened.list_teams();
    assert_eq!(teams.len(), 2, "both teams survived: {teams:?}");
    assert_eq!(teams[0].id, platform_id);
    assert_eq!(teams[0].name, "Platform");
    assert_eq!(teams[0].logins, vec!["alice"], "lowercased on the way in");
    assert_eq!(teams[1].name, "Infra");
    assert_eq!(reopened.list_repos().len(), 1);
    // Both actors were ingested, because each is on a team.
    assert_eq!(reopened.list_events(None, None, None, FilterMode::All, false, None, 500).len(), 12);
    // And membership still resolves after a reopen, not just in the session
    // that wrote it.
    assert_eq!(reopened.list_events(None, None, Some(platform_id), FilterMode::All, false, None, 500).len(), 7);
}

#[test]
fn team_logins_are_lowercased_deduped_and_kept_in_first_seen_order() {
    let srv = server();
    let (engine, _dir) = engine_for(&srv);
    let team = engine
        .create_team(
            "Platform",
            &[
                "Bob".to_string(),
                "alice".to_string(),
                " BOB ".to_string(),
                "@Carol".to_string(),
                "".to_string(),
                "ALICE".to_string(),
            ],
        )
        .unwrap();

    assert_eq!(team.logins, vec!["bob", "alice", "carol"]);
    assert_eq!(engine.list_teams()[0].logins, vec!["bob", "alice", "carol"]);

    // The same normalisation on the update path.
    engine
        .update_team(Team {
            id: team.id,
            name: "Platform".into(),
            logins: vec!["@Dave".into(), "DAVE".into()],
        })
        .unwrap();
    assert_eq!(engine.list_teams()[0].logins, vec!["dave"]);
}

/// A team must be named. The engine used to substitute a default for a blank
/// name; the contract now makes that `invalid`, on both create and update.
#[test]
fn a_blank_team_name_is_invalid() {
    let srv = server();
    let (engine, _dir) = engine_for(&srv);

    let err = engine.create_team("   ", &[]).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Invalid);
    assert!(engine.list_teams().is_empty(), "the rejected team must not be stored");

    let team = engine.create_team("Platform", &[]).unwrap();
    let err = engine
        .update_team(Team { id: team.id, name: "".into(), logins: vec![] })
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Invalid);
    assert_eq!(engine.list_teams()[0].name, "Platform", "the name was left alone");
}

#[test]
fn importing_an_org_team_fetches_without_storing() {
    let mut srv = server();
    let _mock = mock_get(
        &mut srv,
        "/orgs/acme/teams/platform/members",
        &Resp::ok(fixture("team_members.json")),
    );
    let (engine, _dir) = engine_for(&srv);

    let imported = engine.import_org_team("acme", "platform").unwrap();
    assert_eq!(imported.id, UNSAVED_TEAM_ID, "an imported team is not saved yet");
    assert_eq!(imported.name, "platform");
    assert_eq!(imported.logins, vec!["alice", "bob"], "lowercased and deduped");
    // Importing must not create a team; the app passes it to `create_team`.
    assert!(engine.list_teams().is_empty());
}
