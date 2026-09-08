//! In-memory mock implementation of `EngineApi`. Seeded with ~60 events
//! across 3 repos, 5 people, and all 8 `EventKind`s so the app runs
//! end-to-end with no network access; `poll_now` fabricates 0-3 new events
//! per call using a tiny in-process PRNG (no `rand` dependency — it's not
//! in the packet's approved dependency list).
//!
//! Compiled only under `--features mock`, and even then reached only when
//! `GITMON_MOCK=1` (see `lib.rs`): the shipped app has no mock in it at all.
//! The types are the engine's own, so the mock serves exactly the JSON the
//! real engine does — `team_ids`, `watched`, `body` and `sha` included.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::engine_api::EngineApi;
use gitmon::types::{
    Account, BackfillResult, CommitDetail, CommitFile, DayCounts, DeviceLogin, DeviceLoginStatus,
    Digest, DigestPerson, DigestRepo, DigestThread, Event, EventKind, FilterMode, KindCounts,
    OpenPull, PersonSource, PersonSuggestion, PollResult, PrTiming, Repo, RepoBackfill, RepoError,
    RepoSuggestion, Settings, Team, Thread, ThreadItem, ThreadItemKind, ThreadKind, Watch,
    WatchSource, DIGEST_MAX_SERIES_DAYS, DIGEST_PEOPLE_LIMIT, DIGEST_THREAD_LIMIT,
    REVIEW_APPROVED, REVIEW_CHANGES_REQUESTED, REVIEW_REQUIRED, UNSAVED_TEAM_ID,
};
use gitmon::EngineError;

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64
}

/// GitHub's real budget refills on the hour, so the mock's clean-poll reading
/// (unlike `RATE_LIMITED_POLL`'s fixed `MOCK_RATE_LIMIT_SECS`) fakes the same
/// shape: however many seconds remain until the next hour boundary.
fn seconds_until_next_hour(now: i64) -> i64 {
    3600 - now.rem_euclid(3600)
}

/// xorshift64 — deterministic-enough pseudo-randomness for demo data,
/// with no external dependency and no claim of cryptographic quality.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn range(&mut self, n: usize) -> usize {
        (self.next() as usize) % n
    }
}

struct SeedRepo {
    owner: &'static str,
    name: &'static str,
    branch: &'static str,
}
const SEED_REPOS: [SeedRepo; 3] = [
    SeedRepo { owner: "acme", name: "platform", branch: "main" },
    SeedRepo { owner: "acme", name: "web", branch: "main" },
    SeedRepo { owner: "acme", name: "infra", branch: "develop" },
];

const SEED_PEOPLE: [&str; 5] = ["priya", "marcus", "lena", "tom", "aiko"];
/// Who the mock says is signed in, once a device login or `verify_token`
/// has "resolved" one.
const MOCK_LOGIN: &str = "alice";
const BOT_LOGIN: &str = "dependabot[bot]";

/// A token `add_account` treats as GitHub having rejected it — the mock's
/// only way to exercise the auth path with no real GitHub to say no.
const MOCK_REJECTED_TOKEN: &str = "revoked";

/// The mock has no real GitHub to call `/user` on, so it derives an
/// account's login from the token itself: deterministic, so a test that
/// calls `add_account("some-token")` twice gets the same account back, and
/// legible, so `GITMON_MOCK=1`'s own device flow (which always mints the
/// fixed token below) reads as signing in as the demo user.
const MOCK_DEVICE_FLOW_TOKEN_PREFIX: &str = "gho_mock";

fn mock_login_for_token(token: &str) -> String {
    if token.starts_with(MOCK_DEVICE_FLOW_TOKEN_PREFIX) {
        MOCK_LOGIN.to_string()
    } else {
        token.to_lowercase()
    }
}

struct SeedEvent {
    repo: usize,
    actor: &'static str,
    kind: EventKind,
    title: &'static str,
    body: Option<&'static str>,
    number: Option<u64>,
    hours_ago: i64,
}

fn seed_events() -> Vec<SeedEvent> {
    use EventKind::*;
    vec![
        // acme/platform — a retry-budget PR thread (#412)
        SeedEvent { repo: 0, actor: "priya", kind: PrOpened, title: "Add retry budget to webhook dispatcher", body: None, number: Some(412), hours_ago: 4 },
        SeedEvent { repo: 0, actor: "marcus", kind: PrReviewed, title: "Review: approved", body: Some("Approved. One nit: the backoff cap should be configurable per tenant, but that can land separately."), number: Some(412), hours_ago: 4 },
        SeedEvent { repo: 0, actor: "tom", kind: PrCommented, title: "Comment on #412 Add retry budget", body: Some("Can we gate this behind the feature flag first? Two customers are mid-migration and a retry storm would hurt."), number: Some(412), hours_ago: 5 },
        SeedEvent { repo: 0, actor: "priya", kind: PrCommented, title: "Comment on #412 Add retry budget", body: Some("Good call. Flag added in 3f1c2e0, default off."), number: Some(412), hours_ago: 5 },
        SeedEvent { repo: 0, actor: "priya", kind: Commit, title: "Add feature flag for retry budget, default off", body: None, number: None, hours_ago: 5 },
        SeedEvent { repo: 0, actor: "priya", kind: Commit, title: "Refactor event cursor to use composite key", body: None, number: None, hours_ago: 6 },
        SeedEvent { repo: 0, actor: "priya", kind: Commit, title: "Add tenant_id to retry_budget table", body: None, number: None, hours_ago: 7 },
        SeedEvent { repo: 0, actor: "priya", kind: PrMerged, title: "Add retry budget to webhook dispatcher", body: None, number: Some(412), hours_ago: 8 },
        SeedEvent { repo: 0, actor: "aiko", kind: IssueOpened, title: "Webhook retries can exceed rate limit under burst load", body: None, number: Some(415), hours_ago: 20 },
        SeedEvent { repo: 0, actor: "marcus", kind: IssueCommented, title: "Comment on #415 Webhook retries can exceed rate limit", body: Some("Reproduced locally with 200 concurrent webhooks. Filing a fix."), number: Some(415), hours_ago: 19 },
        SeedEvent { repo: 0, actor: "lena", kind: PrOpened, title: "Cap webhook retry concurrency per tenant", body: None, number: Some(416), hours_ago: 18 },
        SeedEvent { repo: 0, actor: "tom", kind: PrReviewed, title: "Review: changes requested", body: Some("The semaphore should be per-tenant, not global — otherwise one noisy tenant starves the rest."), number: Some(416), hours_ago: 17 },
        SeedEvent { repo: 0, actor: "lena", kind: Commit, title: "Make retry semaphore per-tenant", body: None, number: None, hours_ago: 16 },
        SeedEvent { repo: 0, actor: "lena", kind: PrMerged, title: "Cap webhook retry concurrency per tenant", body: None, number: Some(416), hours_ago: 15 },
        SeedEvent { repo: 0, actor: "aiko", kind: Commit, title: "Bump serde to 1.0.210", body: None, number: None, hours_ago: 30 },
        SeedEvent { repo: 0, actor: "priya", kind: IssueOpened, title: "Investigate p99 latency regression on /webhooks", body: None, number: Some(420), hours_ago: 44 },
        SeedEvent { repo: 0, actor: "marcus", kind: IssueCommented, title: "Comment on #420 Investigate p99 latency regression", body: Some("Bisected to the retry-budget change. Looking into a fix."), number: Some(420), hours_ago: 40 },
        SeedEvent { repo: 0, actor: "priya", kind: PrOpened, title: "Cache tenant config lookups on the hot path", body: None, number: Some(421), hours_ago: 39 },
        SeedEvent { repo: 0, actor: "tom", kind: PrReviewed, title: "Review: approved", body: Some("Nice speedup — p99 back under 40ms in staging."), number: Some(421), hours_ago: 38 },
        SeedEvent { repo: 0, actor: "priya", kind: PrMerged, title: "Cache tenant config lookups on the hot path", body: None, number: Some(421), hours_ago: 37 },

        // acme/web — dashboard latency + rollup chunking (#233/#418)
        SeedEvent { repo: 1, actor: "marcus", kind: IssueOpened, title: "Dashboard latency spikes after 14:00 UTC", body: None, number: Some(233), hours_ago: 6 },
        SeedEvent { repo: 1, actor: "marcus", kind: IssueCommented, title: "Comment on #233 Dashboard latency spikes", body: Some("Traced it to the hourly rollup job holding a lock on metrics_daily. Proposal: move it to 03:00 and chunk by tenant."), number: Some(233), hours_ago: 5 },
        SeedEvent { repo: 1, actor: "marcus", kind: PrOpened, title: "Chunk the hourly rollup by tenant", body: None, number: Some(418), hours_ago: 28 },
        SeedEvent { repo: 1, actor: "priya", kind: PrReviewed, title: "Review: changes requested", body: Some("The migration drops the old index before the new one is built; swap the order."), number: Some(418), hours_ago: 26 },
        SeedEvent { repo: 1, actor: "marcus", kind: Commit, title: "Chunk rollup by tenant (wip)", body: None, number: None, hours_ago: 25 },
        SeedEvent { repo: 1, actor: "marcus", kind: Commit, title: "Fix index migration order", body: None, number: None, hours_ago: 24 },
        SeedEvent { repo: 1, actor: "priya", kind: PrReviewed, title: "Review: approved", body: Some("Order looks right now. Ship it."), number: Some(418), hours_ago: 23 },
        SeedEvent { repo: 1, actor: "marcus", kind: PrMerged, title: "Chunk the hourly rollup by tenant", body: None, number: Some(418), hours_ago: 22 },
        SeedEvent { repo: 1, actor: "tom", kind: Commit, title: "Bump vite to 6.3.1", body: None, number: None, hours_ago: 23 },
        SeedEvent { repo: 1, actor: "aiko", kind: IssueOpened, title: "Chart legend overlaps on narrow viewports", body: None, number: Some(240), hours_ago: 50 },
        SeedEvent { repo: 1, actor: "tom", kind: PrOpened, title: "Reflow chart legend below 480px", body: None, number: Some(241), hours_ago: 49 },
        SeedEvent { repo: 1, actor: "lena", kind: PrCommented, title: "Comment on #241 Reflow chart legend", body: Some("Looks good on iPhone SE width too, nice."), number: Some(241), hours_ago: 48 },
        SeedEvent { repo: 1, actor: "tom", kind: PrMerged, title: "Reflow chart legend below 480px", body: None, number: Some(241), hours_ago: 47 },
        SeedEvent { repo: 1, actor: "aiko", kind: PrOpened, title: "Add dark-mode toggle to the settings page", body: None, number: Some(244), hours_ago: 60 },
        SeedEvent { repo: 1, actor: "priya", kind: PrClosed, title: "Add dark-mode toggle to the settings page", body: None, number: Some(244), hours_ago: 55 },
        SeedEvent { repo: 1, actor: "priya", kind: IssueCommented, title: "Comment on #244 Add dark-mode toggle", body: Some("Closing — we're standardizing on a single fixed theme for now, see docs/design."), number: Some(244), hours_ago: 55 },
        SeedEvent { repo: 1, actor: "tom", kind: Commit, title: "Upgrade eslint config to flat config format", body: None, number: None, hours_ago: 70 },
        SeedEvent { repo: 1, actor: "lena", kind: Commit, title: "Fix off-by-one in pagination cursor", body: None, number: None, hours_ago: 80 },

        // acme/infra — flaky test + Postgres migration
        SeedEvent { repo: 2, actor: "lena", kind: Commit, title: "Fix flaky test in scheduler_test.go", body: None, number: None, hours_ago: 1 },
        SeedEvent { repo: 2, actor: "aiko", kind: Commit, title: "Remove dead retry path", body: None, number: None, hours_ago: 3 },
        SeedEvent { repo: 2, actor: "aiko", kind: PrOpened, title: "Migrate alerts service to Postgres 16", body: None, number: Some(88), hours_ago: 26 },
        // The migration is a one-woman branch: seven of aiko's commits in a
        // row, which is what pushes acme/infra past 0.8 author concentration
        // in the digest and gives the Summary view a repo that really is one
        // person's.
        SeedEvent { repo: 2, actor: "aiko", kind: Commit, title: "Bump the alerts service to the pg16 client library", body: None, number: None, hours_ago: 25 },
        SeedEvent { repo: 2, actor: "aiko", kind: Commit, title: "Rewrite the alert_state upsert for MERGE", body: None, number: None, hours_ago: 24 },
        SeedEvent { repo: 2, actor: "aiko", kind: Commit, title: "Drop the pg13 compatibility shim", body: None, number: None, hours_ago: 23 },
        SeedEvent { repo: 2, actor: "aiko", kind: Commit, title: "Regenerate the alerts schema dump under pg16", body: None, number: None, hours_ago: 22 },
        SeedEvent { repo: 2, actor: "aiko", kind: Commit, title: "Point the staging compose file at postgres:16", body: None, number: None, hours_ago: 21 },
        SeedEvent { repo: 2, actor: "aiko", kind: Commit, title: "Backfill alert_state.tenant_id before the not-null", body: None, number: None, hours_ago: 20 },
        SeedEvent { repo: 2, actor: "aiko", kind: Commit, title: "Add the pg16 upgrade runbook", body: None, number: None, hours_ago: 19 },
        SeedEvent { repo: 2, actor: "lena", kind: PrReviewed, title: "Review: approved", body: Some("Ran the migration against a staging snapshot, all good."), number: Some(88), hours_ago: 20 },
        SeedEvent { repo: 2, actor: "aiko", kind: PrMerged, title: "Migrate alerts service to Postgres 16", body: None, number: Some(88), hours_ago: 18 },
        SeedEvent { repo: 2, actor: "marcus", kind: IssueOpened, title: "Terraform apply intermittently times out on the alerts module", body: None, number: Some(90), hours_ago: 33 },
        SeedEvent { repo: 2, actor: "lena", kind: IssueCommented, title: "Comment on #90 Terraform apply intermittently times out", body: Some("Looks like the provider's default timeout is too short for this module — bumping it."), number: Some(90), hours_ago: 30 },
        SeedEvent { repo: 2, actor: "lena", kind: PrOpened, title: "Increase terraform provider timeout for alerts module", body: None, number: Some(91), hours_ago: 29 },
        SeedEvent { repo: 2, actor: "marcus", kind: PrReviewed, title: "Review: approved", body: Some("Confirmed fixes the timeout in three consecutive applies."), number: Some(91), hours_ago: 28 },
        SeedEvent { repo: 2, actor: "lena", kind: PrMerged, title: "Increase terraform provider timeout for alerts module", body: None, number: Some(91), hours_ago: 27 },
        SeedEvent { repo: 2, actor: "tom", kind: Commit, title: "Pin base image digest in Dockerfile", body: None, number: None, hours_ago: 42 },
        SeedEvent { repo: 2, actor: "aiko", kind: IssueOpened, title: "Nightly backup job silently failing since Tuesday", body: None, number: Some(95), hours_ago: 14 },
        SeedEvent { repo: 2, actor: "lena", kind: IssueCommented, title: "Comment on #95 Nightly backup job silently failing", body: Some("Disk quota on the backup volume was hit; bumped it and re-ran manually."), number: Some(95), hours_ago: 12 },
        SeedEvent { repo: 2, actor: "aiko", kind: PrOpened, title: "Alert when backup volume crosses 80% usage", body: None, number: Some(96), hours_ago: 11 },
        SeedEvent { repo: 2, actor: "marcus", kind: PrCommented, title: "Comment on #96 Alert when backup volume crosses 80%", body: Some("Can we also page on-call, not just Slack, for this one?"), number: Some(96), hours_ago: 10 },
        SeedEvent { repo: 2, actor: "aiko", kind: Commit, title: "Page on-call in addition to Slack for volume alerts", body: None, number: None, hours_ago: 9 },
        SeedEvent { repo: 2, actor: "aiko", kind: PrMerged, title: "Alert when backup volume crosses 80% usage", body: None, number: Some(96), hours_ago: 8 },
        SeedEvent { repo: 2, actor: BOT_LOGIN, kind: PrOpened, title: "Bump tokio from 1.38.0 to 1.40.0", body: None, number: Some(97), hours_ago: 36 },
        SeedEvent { repo: 2, actor: "tom", kind: PrMerged, title: "Bump tokio from 1.38.0 to 1.40.0", body: None, number: Some(97), hours_ago: 34 },
        SeedEvent { repo: 0, actor: BOT_LOGIN, kind: PrOpened, title: "Bump serde_json from 1.0.120 to 1.0.128", body: None, number: Some(422), hours_ago: 58 },
        SeedEvent { repo: 0, actor: "marcus", kind: PrMerged, title: "Bump serde_json from 1.0.120 to 1.0.128", body: None, number: Some(422), hours_ago: 56 },

        // acme/platform — a still-open PR authored by the signed-in mock
        // login, watched automatically (source "author") so the Watched
        // view and the feed's eye glyph have a real open thread to show.
        SeedEvent { repo: 0, actor: MOCK_LOGIN, kind: PrOpened, title: "Add per-tenant rate limit override", body: Some("Lets a tenant with a contractual higher limit opt out of the shared default. Off unless explicitly configured."), number: Some(425), hours_ago: 5 },
        SeedEvent { repo: 0, actor: "lena", kind: PrCommented, title: "Comment on #425 Add per-tenant rate limit override", body: Some("Should the override live in the tenant config table or a separate one? Slight preference for separate, easier to audit."), number: Some(425), hours_ago: 4 },
    ]
}

/// One seeded open pull request, before it is given a repo id and a clock.
struct SeedPull {
    repo: usize,
    number: u64,
    author: &'static str,
    title: &'static str,
    draft: bool,
    /// How long ago it was opened. Spread from an hour to nine days so the
    /// Summary view's "oldest open PR" and its age tints have a real range.
    hours_ago: i64,
    /// How long ago it was last touched; always at most `hours_ago`.
    activity_hours_ago: i64,
    reviewers: &'static [&'static str],
    /// `None` where nobody has been asked and nobody has reviewed — exactly
    /// what the real engine's `review_decision` leaves null.
    decision: Option<&'static str>,
    additions: Option<u32>,
    deletions: Option<u32>,
}

/// Eight open PRs across five authors and all three repos: two drafts, two
/// approved, one with changes requested, three still waiting on a reviewer,
/// and ages from one hour to nine days.
const SEED_PULLS: [SeedPull; 8] = [
    SeedPull { repo: 0, number: 425, author: MOCK_LOGIN, title: "Add per-tenant rate limit override", draft: false, hours_ago: 5, activity_hours_ago: 4, reviewers: &["lena"], decision: Some(REVIEW_REQUIRED), additions: Some(184), deletions: Some(22) },
    SeedPull { repo: 0, number: 430, author: "priya", title: "Extract the retry budget into its own crate", draft: false, hours_ago: 26, activity_hours_ago: 6, reviewers: &["marcus", "lena"], decision: Some(REVIEW_APPROVED), additions: Some(612), deletions: Some(431) },
    SeedPull { repo: 0, number: 431, author: "priya", title: "Sketch the tenant sharding plan", draft: true, hours_ago: 3, activity_hours_ago: 2, reviewers: &[], decision: None, additions: Some(96), deletions: Some(4) },
    SeedPull { repo: 1, number: 250, author: "marcus", title: "Virtualise the events feed", draft: false, hours_ago: 50, activity_hours_ago: 9, reviewers: &["priya"], decision: Some(REVIEW_CHANGES_REQUESTED), additions: Some(305), deletions: Some(118) },
    SeedPull { repo: 1, number: 251, author: "lena", title: "Add keyboard shortcuts to the detail pane", draft: false, hours_ago: 1, activity_hours_ago: 1, reviewers: &["tom"], decision: Some(REVIEW_REQUIRED), additions: Some(77), deletions: Some(9) },
    SeedPull { repo: 2, number: 101, author: "lena", title: "Move alerts onto the new Terraform module", draft: false, hours_ago: 216, activity_hours_ago: 31, reviewers: &["aiko", "marcus"], decision: Some(REVIEW_REQUIRED), additions: Some(1204), deletions: Some(977) },
    SeedPull { repo: 2, number: 102, author: "aiko", title: "Nightly restore drill, first pass", draft: true, hours_ago: 100, activity_hours_ago: 40, reviewers: &[], decision: None, additions: Some(58), deletions: Some(0) },
    SeedPull { repo: 2, number: 103, author: "aiko", title: "Retry backup uploads with jitter", draft: false, hours_ago: 30, activity_hours_ago: 7, reviewers: &["lena"], decision: Some(REVIEW_APPROVED), additions: Some(143), deletions: Some(37) },
];

fn seed_pulls(repos: &[Repo], base: i64) -> Vec<OpenPull> {
    SEED_PULLS
        .iter()
        .map(|p| OpenPull {
            repo_id: repos[p.repo].id,
            number: p.number,
            title: p.title.to_string(),
            url: format!(
                "https://github.com/{}/{}/pull/{}",
                repos[p.repo].owner, repos[p.repo].name, p.number
            ),
            author_login: p.author.to_string(),
            author_avatar_url: None,
            draft: p.draft,
            created_at: base - p.hours_ago * 3600,
            updated_at: base - p.activity_hours_ago * 3600,
            last_activity_at: base - p.activity_hours_ago * 3600,
            additions: p.additions,
            deletions: p.deletions,
            requested_reviewers: p.reviewers.iter().map(|r| r.to_string()).collect(),
            review_decision: p.decision.map(|d| d.to_string()),
        })
        .collect()
}

/// Titles fabricated for `poll_now`, grouped by kind so a fresh event still
/// reads as plausible for the kind it was assigned.
fn fabricated_titles(kind: EventKind) -> &'static [&'static str] {
    use EventKind::*;
    match kind {
        Commit => &["Tidy up error messages in the poller", "Add integration test for pagination", "Fix typo in README", "Extract retry helper into its own module"],
        PrOpened => &["Add exponential backoff to the sync job", "Speed up cold start by lazy-loading config", "Support custom User-Agent header"],
        PrMerged => &["Add exponential backoff to the sync job", "Speed up cold start by lazy-loading config"],
        PrClosed => &["Try caching responses at the edge", "Prototype a GraphQL layer"],
        PrReviewed => &["Review: approved", "Review: changes requested", "Review: commented"],
        PrCommented => &["Comment on the open pull request", "Comment on the open pull request"],
        IssueOpened => &["Occasional timeout when the API is under load", "Docs are out of date for the new config format"],
        IssueCommented => &["Comment on the open issue", "Comment on the open issue"],
    }
}

struct MockState {
    // Set by `set_token` (device-flow success, or `load_token` at startup)
    // for parity with the real engine's in-memory token; the mock has no
    // auth-gated calls left that need to read it back.
    #[allow(dead_code)]
    token: Option<String>,
    /// The signed-in login, remembered exactly as the real engine does:
    /// resolved by a device login, forgotten by `set_token(None)`.
    login: Option<String>,
    /// Signed-in accounts, in the order added (docs/CONTRACT.md "Accounts").
    /// Empty on a fresh mock — the mock starts at onboarding exactly like a
    /// fresh real install.
    accounts: Vec<Account>,
    /// What `set_account_token` was handed, by lowercased login — the mock
    /// never gates a repo's polling on this (see `poll_now`), so this exists
    /// only so a test can confirm `lib.rs`'s startup actually called it.
    account_tokens: HashMap<String, String>,
    /// Teams in display order, like `Engine::list_teams`.
    teams: Vec<Team>,
    repos: Vec<Repo>,
    events: Vec<Event>,
    /// Watched threads (docs/CONTRACT.md "Watched threads"). `Event.watched`
    /// is resolved from this at every read, exactly like the real engine —
    /// see `resolve_watched`.
    watches: Vec<Watch>,
    /// Open pull requests (docs/CONTRACT.md "Pulls"). The real engine keeps
    /// closed and merged rows too; the mock keeps only the open ones, because
    /// `open_pulls` is the only thing that reads the table and the digest's
    /// per-person counts are drawn from the open ones alone.
    pulls: Vec<OpenPull>,
    settings: Settings,
    rng: Rng,
    /// Outstanding device-flow logins, device_code -> polls-so-far. See
    /// `poll_device_login`: pending twice, then `ok` on the third poll.
    device_logins: HashMap<String, u32>,
    /// How many times `poll_now` has been called this process. Only the first
    /// poll reports the demo rate limit; see `RATE_LIMITED_POLL`.
    polls: u32,
}

/// Which `poll_now` call the mock reports a rate limit on, so the app's
/// banner can be seen under `GITMON_MOCK=1` without waiting on a real
/// GitHub budget. It fires once and then stops, because a limit that never
/// lifted would pin the banner up forever and hide the "a clean poll takes
/// it back down" half of the behaviour.
const RATE_LIMITED_POLL: u32 = 1;

/// How far ahead of the failing poll the mock's rate limit says it refills.
const MOCK_RATE_LIMIT_SECS: i64 = 15 * 60;

pub struct MockEngine {
    state: Mutex<MockState>,
    next_event_id: AtomicU64,
    next_repo_id: AtomicU64,
    next_device_code: AtomicU64,
}

impl MockEngine {
    pub fn new() -> Self {
        let repos: Vec<Repo> = SEED_REPOS
            .iter()
            .enumerate()
            .map(|(i, r)| Repo {
                id: (i + 1) as u64,
                owner: r.owner.to_string(),
                name: r.name.to_string(),
                url: format!("https://github.com/{}/{}", r.owner, r.name),
                account_login: String::new(),
                default_branch: r.branch.to_string(),
                last_polled_at: Some(now() - 60),
                // As if their first poll had reached back the usual 24 hours,
                // so `backfill` has a floor to step down from in the mock too.
                backfilled_to: Some(now() - 60 - gitmon::FIRST_POLL_LOOKBACK_SECS),
                last_error: None,
                hidden: false,
            })
            .collect();

        let next_repo_id = AtomicU64::new(repos.len() as u64 + 1);
        let next_event_id = AtomicU64::new(1);

        let base = now();
        let mut events: Vec<Event> = seed_events()
            .into_iter()
            .map(|s| {
                let id = next_event_id.fetch_add(1, Ordering::Relaxed);
                Event {
                    id,
                    repo_id: repos[s.repo].id,
                    kind: s.kind,
                    actor_login: s.actor.to_string(),
                    actor_avatar_url: None,
                    title: s.title.to_string(),
                    body_preview: s.body.map(|b| b.to_string()),
                    body: s.body.map(|b| b.to_string()),
                    url: build_event_url(&repos[s.repo].owner, &repos[s.repo].name, s.kind, s.number, id),
                    number: s.number,
                    occurred_at: base - s.hours_ago * 3600,
                    seen: s.hours_ago > 36, // older events start already-seen; recent ones are unseen
                    // Both are resolved at read time by the real engine;
                    // `list_events` below fills them in the same way.
                    team_ids: Vec::new(),
                    watched: false,
                }
            })
            .collect();
        events.sort_by(|a, b| b.occurred_at.cmp(&a.occurred_at).then(b.id.cmp(&a.id)));

        // Give one repo a transient error so the Repos view's "last error"
        // column has something real to demo without waiting on poll_now
        // to fabricate one.
        let mut repos = repos;
        if let Some(r) = repos.get_mut(1) {
            r.last_error = Some("Rate limited by GitHub; retrying automatically.".to_string());
        }

        let seed = (base as u64) ^ 0x9E3779B97F4A7C15;
        // Two watches (packet's mock seed): one open PR the mock login
        // authored (source `author`, as auto-watch would have set it), one
        // merged PR watched by hand (source `manual`) — see #425 and #96 in
        // `seed_events`.
        let watches = vec![
            Watch {
                repo_id: repos[0].id,
                number: 425,
                kind: ThreadKind::Pull,
                title: "Add per-tenant rate limit override".to_string(),
                state: "open".to_string(),
                source: WatchSource::Author,
                since: base - 5 * 3600,
            },
            Watch {
                repo_id: repos[2].id,
                number: 96,
                kind: ThreadKind::Pull,
                title: "Alert when backup volume crosses 80% usage".to_string(),
                state: "merged".to_string(),
                source: WatchSource::Manual,
                since: base - 6 * 3600,
            },
        ];
        let pulls = seed_pulls(&repos, base);
        MockEngine {
            state: Mutex::new(MockState {
                token: None,
                login: None,
                accounts: Vec::new(),
                account_tokens: HashMap::new(),
                // Two teams overlapping by exactly one login ("lena"), so the
                // switcher, People grouping and suggest_people exclusion all
                // have a real overlap case to demo without a real GitHub org.
                teams: vec![
                    Team {
                        id: 1,
                        name: "Platform".to_string(),
                        logins: vec!["priya".to_string(), "marcus".to_string(), "lena".to_string()],
                    },
                    Team {
                        id: 2,
                        name: "Infra".to_string(),
                        logins: vec!["lena".to_string(), "aiko".to_string(), "tom".to_string()],
                    },
                ],
                repos,
                events,
                watches,
                pulls,
                settings: Settings::default(),
                rng: Rng(if seed == 0 { 0xdead_beef } else { seed }),
                device_logins: HashMap::new(),
                polls: 0,
            }),
            next_event_id,
            next_repo_id,
            next_device_code: AtomicU64::new(1),
        }
    }
}

impl Default for MockEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
impl MockEngine {
    /// What `set_account_token` was last handed for this login, or `None` —
    /// used by `lib.rs`'s startup tests to confirm every account's token was
    /// installed, since the mock otherwise never reads it back.
    pub(crate) fn installed_account_token(&self, login: &str) -> Option<String> {
        self.state.lock().unwrap().account_tokens.get(&login.to_lowercase()).cloned()
    }
}

/// Builds a plausible per-thing URL (commit/pull/issue), matching the
/// contract's "url is the html_url of the specific thing" — used for
/// fabricated events in `add_repo`'s seed and `poll_now`, mirroring what
/// `seed_events` already does by hand for the initial history.
fn build_event_url(owner: &str, name: &str, kind: EventKind, number: Option<u64>, fake_id: u64) -> String {
    let base = format!("https://github.com/{owner}/{name}");
    match kind {
        EventKind::Commit => format!("{base}/commit/{:07x}", fake_id),
        EventKind::IssueOpened | EventKind::IssueCommented => {
            format!("{base}/issues/{}", number.unwrap_or(fake_id))
        }
        _ => format!("{base}/pull/{}", number.unwrap_or(fake_id)),
    }
}

fn parse_repo_spec(spec: &str) -> Result<(String, String), EngineError> {
    let spec = spec.trim().trim_end_matches('/');
    let after_host = spec
        .strip_prefix("https://github.com/")
        .or_else(|| spec.strip_prefix("http://github.com/"))
        .or_else(|| spec.strip_prefix("github.com/"))
        .unwrap_or(spec);
    let parts: Vec<&str> = after_host.trim_end_matches(".git").splitn(3, '/').collect();
    if parts.len() < 2 || parts[0].is_empty() || parts[1].is_empty() {
        return Err(EngineError::invalid("Expected \"owner/name\" or a github.com URL."));
    }
    Ok((parts[0].to_string(), parts[1].to_string()))
}

impl MockEngine {
    /// Shared body of `suggest_people` and `suggest_active_people`: ranked
    /// contributors, then bots, from stored events. `keep_team` is the only
    /// thing that differs between the two callers — it decides whether a
    /// (lowercase) login passes the caller's team constraint.
    fn suggest_people_filtered(
        &self,
        query: &str,
        limit: u32,
        keep_team: impl Fn(&str) -> bool,
    ) -> Vec<PersonSuggestion> {
        let state = self.state.lock().unwrap();
        let q = query.trim().to_lowercase();

        // (total events, per-kind counts, per-repo counts) for one actor.
        type ActorTally = (u32, std::collections::HashMap<EventKind, u32>, std::collections::HashMap<u64, u32>);
        let mut counts: std::collections::HashMap<&str, ActorTally> = std::collections::HashMap::new();
        let visible = visible_repo_ids(&state.repos);
        for e in state.events.iter().filter(|e| visible.contains(&e.repo_id)) {
            let entry = counts.entry(e.actor_login.as_str()).or_default();
            entry.0 += 1;
            *entry.1.entry(e.kind).or_insert(0) += 1;
            *entry.2.entry(e.repo_id).or_insert(0) += 1;
        }

        let mut contributors: Vec<PersonSuggestion> = counts
            .into_iter()
            .filter(|(login, _)| keep_team(login) && !login.ends_with("[bot]"))
            .filter(|(login, _)| q.is_empty() || login.starts_with(&q) || login.contains(&q))
            .map(|(login, (total, kinds, repos))| {
                let top_kind = kinds.into_iter().max_by_key(|(_, c)| *c).map(|(k, _)| k);
                let top_repo = repos.into_iter().max_by_key(|(_, c)| *c).map(|(id, _)| id);
                let repo_label = top_repo
                    .and_then(|id| state.repos.iter().find(|r| r.id == id))
                    .map(|r| format!("{}/{}", r.owner, r.name))
                    .unwrap_or_default();
                let kind_label = top_kind.map(kind_plural).unwrap_or("event");
                PersonSuggestion {
                    login: login.to_string(),
                    avatar_url: None,
                    source: PersonSource::Contributor,
                    why: format!("{total} {kind_label} · {repo_label}"),
                }
            })
            .collect();
        contributors.sort_by(|a, b| a.login.cmp(&b.login));

        let mut bots: Vec<PersonSuggestion> = state
            .events
            .iter()
            .map(|e| e.actor_login.as_str())
            .filter(|l| l.ends_with("[bot]") && keep_team(l))
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .filter(|login| q.is_empty() || login.starts_with(&q) || login.contains(&q))
            .map(|login| PersonSuggestion {
                login: login.to_string(),
                avatar_url: None,
                source: PersonSource::Bot,
                why: "dependabot".to_string(),
            })
            .collect();
        bots.sort_by(|a, b| a.login.cmp(&b.login));

        contributors.extend(bots);
        contributors.truncate(limit as usize);
        contributors
    }
}

impl EngineApi for MockEngine {
    fn set_token(&self, token: Option<String>) {
        let mut state = self.state.lock().unwrap();
        if token.is_none() {
            state.login = None;
        }
        state.token = token;
    }

    fn verify_token(&self) -> Result<String, EngineError> {
        let mut state = self.state.lock().unwrap();
        if state.token.is_none() {
            return Err(EngineError::auth("no GitHub credential is set"));
        }
        state.login = Some(MOCK_LOGIN.to_string());
        Ok(MOCK_LOGIN.to_string())
    }

    fn list_accounts(&self) -> Vec<Account> {
        self.state.lock().unwrap().accounts.clone()
    }

    fn add_account(&self, token: &str) -> Result<Account, EngineError> {
        let token = token.trim();
        if token.is_empty() {
            return Err(EngineError::invalid("a token is required"));
        }
        // The mock has no real GitHub to verify against, so a fixed sentinel
        // (case-insensitive) stands in for "GitHub rejected this token" —
        // tests use it to exercise the auth path without a network call.
        if token.eq_ignore_ascii_case(MOCK_REJECTED_TOKEN) {
            return Err(EngineError::auth("mock: GitHub rejected this token"));
        }
        let login = mock_login_for_token(token);
        let mut state = self.state.lock().unwrap();
        if let Some(existing) = state.accounts.iter().find(|a| a.login.eq_ignore_ascii_case(&login)) {
            // Re-authenticating replaces the token, not the row — same as
            // the real engine, `added_at` stays put.
            return Ok(existing.clone());
        }
        let account = Account { login: login.clone(), avatar_url: None, added_at: now() };
        state.accounts.push(account.clone());
        // Claims every repo no account has claimed yet — the pre-v5 upgrade
        // path the real engine's `add_account` describes.
        for r in state.repos.iter_mut() {
            if r.account_login.is_empty() {
                r.account_login = login.clone();
            }
        }
        Ok(account)
    }

    fn remove_account(&self, login: &str) -> Result<(), EngineError> {
        let login = login.trim();
        let mut state = self.state.lock().unwrap();
        let before = state.accounts.len();
        state.accounts.retain(|a| !a.login.eq_ignore_ascii_case(login));
        if state.accounts.len() == before {
            return Err(EngineError::not_found(format!("no account {login:?}")));
        }
        if state.login.as_deref().is_some_and(|l| l.eq_ignore_ascii_case(login)) {
            state.login = None;
        }
        Ok(())
    }

    fn set_account_token(&self, login: &str, token: &str) {
        // The mock never gates a repo's polling on a per-account token (see
        // `poll_now`) — this just records what it was handed, so a test can
        // confirm `lib.rs`'s startup called it for every account.
        self.state.lock().unwrap().account_tokens.insert(login.to_lowercase(), token.to_string());
    }

    fn forget_account_token(&self, login: &str) {
        self.state.lock().unwrap().account_tokens.remove(&login.to_lowercase());
    }

    fn current_login(&self) -> Option<String> {
        self.state.lock().unwrap().login.clone()
    }

    fn start_device_login(&self, _client_id: &str) -> Result<DeviceLogin, EngineError> {
        let mut state = self.state.lock().unwrap();
        let device_code = format!("mock-device-code-{}", self.next_device_code.fetch_add(1, Ordering::Relaxed));
        state.device_logins.insert(device_code.clone(), 0);
        Ok(DeviceLogin {
            device_code,
            user_code: "ABCD-1234".to_string(),
            verification_uri: "https://github.com/login/device".to_string(),
            verification_uri_complete: Some("https://github.com/login/device?user_code=ABCD-1234".to_string()),
            expires_in: 900,
            interval: 1, // fast on purpose: this is the mock, not a real GitHub poll
        })
    }

    fn poll_device_login(&self, _client_id: &str, device_code: &str) -> Result<DeviceLoginStatus, EngineError> {
        let mut state = self.state.lock().unwrap();
        let polls = state
            .device_logins
            .get_mut(device_code)
            .ok_or_else(|| EngineError::not_found("Unknown device code."))?;
        *polls += 1;
        // Packet spec: pending twice, then ok with a fake token and login
        // "alice" on the third poll.
        if *polls < 3 {
            Ok(DeviceLoginStatus::Pending)
        } else {
            state.device_logins.remove(device_code);
            let token = "gho_mock0000000000000000deadbeef".to_string();
            // The real engine installs the credential and resolves the
            // login itself before returning; the app relies on that.
            state.token = Some(token.clone());
            state.login = Some(MOCK_LOGIN.to_string());
            Ok(DeviceLoginStatus::Ok { token, login: MOCK_LOGIN.to_string() })
        }
    }

    fn list_teams(&self) -> Vec<Team> {
        self.state.lock().unwrap().teams.clone()
    }

    fn create_team(&self, name: &str, logins: &[String]) -> Result<Team, EngineError> {
        let mut state = self.state.lock().unwrap();
        let id = state.teams.iter().map(|t| t.id).max().unwrap_or(0) + 1;
        let team = Team { id, name: name.to_string(), logins: normalize_logins(logins) };
        state.teams.push(team.clone());
        Ok(team)
    }

    fn update_team(&self, team: Team) -> Result<(), EngineError> {
        let mut state = self.state.lock().unwrap();
        let slot = state
            .teams
            .iter_mut()
            .find(|t| t.id == team.id)
            .ok_or_else(|| EngineError::not_found(format!("no team {}", team.id)))?;
        slot.name = team.name;
        slot.logins = normalize_logins(&team.logins);
        Ok(())
    }

    fn delete_team(&self, team_id: u64) -> Result<(), EngineError> {
        let mut state = self.state.lock().unwrap();
        let before = state.teams.len();
        state.teams.retain(|t| t.id != team_id);
        if state.teams.len() == before {
            return Err(EngineError::not_found(format!("no team {team_id}")));
        }
        Ok(())
    }

    fn reorder_teams(&self, ids: &[u64]) -> Result<(), EngineError> {
        let mut state = self.state.lock().unwrap();
        let current: std::collections::HashSet<u64> = state.teams.iter().map(|t| t.id).collect();
        let requested: std::collections::HashSet<u64> = ids.iter().copied().collect();
        if current != requested || ids.len() != state.teams.len() {
            return Err(EngineError::invalid("ids must be a permutation of every team id"));
        }
        let mut by_id: HashMap<u64, Team> = state.teams.drain(..).map(|t| (t.id, t)).collect();
        state.teams = ids.iter().map(|id| by_id.remove(id).unwrap()).collect();
        Ok(())
    }

    fn import_org_team(&self, org: &str, team_slug: &str) -> Result<Team, EngineError> {
        if org.trim().is_empty() || team_slug.trim().is_empty() {
            return Err(EngineError::invalid("Org and team slug are required."));
        }
        // Mock: fabricate a plausible membership list from the seeded people.
        // `import_org_team` fetches but does not save, so the id is 0.
        Ok(Team {
            id: UNSAVED_TEAM_ID,
            name: format!("{org}/{team_slug}"),
            logins: SEED_PEOPLE.iter().map(|s| s.to_string()).collect(),
        })
    }

    fn suggest_people(
        &self,
        query: &str,
        team_id: Option<u64>,
        limit: u32,
    ) -> Result<Vec<PersonSuggestion>, EngineError> {
        // Per the contract: with a `team_id`, exclude logins already in that
        // team; without one, exclude nobody.
        let team_set: std::collections::HashSet<String> = match team_id {
            Some(id) => self
                .state
                .lock()
                .unwrap()
                .teams
                .iter()
                .find(|t| t.id == id)
                .map(|t| t.logins.iter().cloned().collect())
                .unwrap_or_default(),
            None => std::collections::HashSet::new(),
        };
        Ok(self.suggest_people_filtered(query, limit, |login| !team_set.contains(login)))
    }

    fn suggest_active_people(
        &self,
        query: &str,
        team_id: Option<u64>,
        limit: u32,
    ) -> Result<Vec<PersonSuggestion>, EngineError> {
        // Mirror image of `suggest_people`: with a `team_id`, keep only
        // logins already in that team; an id matching no team keeps nobody.
        let team_set: Option<std::collections::HashSet<String>> = team_id.map(|id| {
            self.state
                .lock()
                .unwrap()
                .teams
                .iter()
                .find(|t| t.id == id)
                .map(|t| t.logins.iter().cloned().collect())
                .unwrap_or_default()
        });
        Ok(self.suggest_people_filtered(query, limit, |login| {
            team_set.as_ref().is_none_or(|t| t.contains(login))
        }))
    }

    fn suggest_repos(&self) -> Result<Vec<RepoSuggestion>, EngineError> {
        let state = self.state.lock().unwrap();
        let watched: std::collections::HashSet<(String, String)> =
            state.repos.iter().map(|r| (r.owner.clone(), r.name.clone())).collect();
        let candidates = [
            ("acme", "mobile", "you pushed 2 days ago"),
            ("acme", "docs", "12 open PRs"),
            ("acme", "billing", "member of acme"),
        ];
        Ok(candidates
            .into_iter()
            .filter(|(o, n, _)| !watched.contains(&(o.to_string(), n.to_string())))
            .map(|(owner, name, why)| RepoSuggestion { owner: owner.to_string(), name: name.to_string(), why: why.to_string() })
            .collect())
    }

    fn add_repo(&self, spec: &str, account_login: Option<&str>) -> Result<Repo, EngineError> {
        let (owner, name) = parse_repo_spec(spec)?;
        let mut state = self.state.lock().unwrap();
        if state.repos.iter().any(|r| r.owner.eq_ignore_ascii_case(&owner) && r.name.eq_ignore_ascii_case(&name)) {
            return Err(EngineError::invalid(format!("{owner}/{name} is already watched.")));
        }
        // Mirrors the real engine's `resolve_add_account`: a named login must
        // exist, an unnamed repo goes to the sole account (or stays unclaimed
        // with none signed in), and several accounts with none named is
        // `invalid` rather than a guess.
        let resolved_login = match account_login.map(str::trim).filter(|l| !l.is_empty()) {
            Some(login) => {
                if !state.accounts.iter().any(|a| a.login.eq_ignore_ascii_case(login)) {
                    return Err(EngineError::invalid(format!("no account {login:?} is signed in")));
                }
                login.to_lowercase()
            }
            None => match state.accounts.len() {
                0 => String::new(),
                1 => state.accounts[0].login.clone(),
                n => {
                    return Err(EngineError::invalid(format!(
                        "add_repo needs an account_login: {n} accounts are signed in"
                    )))
                }
            },
        };
        let id = self.next_repo_id.fetch_add(1, Ordering::Relaxed);
        let repo = Repo {
            id,
            owner: owner.clone(),
            name: name.clone(),
            url: format!("https://github.com/{owner}/{name}"),
            account_login: resolved_login,
            default_branch: "main".to_string(),
            last_polled_at: None,
            backfilled_to: None,
            last_error: None,
            hidden: false,
        };
        state.repos.push(repo.clone());

        // Simulate the engine's "first poll uses since = now-24h": seed a
        // few recent events so a newly added repo isn't empty until the
        // next background poll happens to touch it.
        let people = SEED_PEOPLE;
        let base = now();
        for i in 0..3u64 {
            let event_id = self.next_event_id.fetch_add(1, Ordering::Relaxed);
            let actor = people[(i as usize + name.len()) % people.len()];
            let kind = EventKind::ALL[(i as usize * 3 + owner.len()) % EventKind::ALL.len()];
            let titles = fabricated_titles(kind);
            let title = titles[(i as usize) % titles.len()];
            let number = if matches!(kind, EventKind::Commit) { None } else { Some(100 + event_id) };
            state.events.push(Event {
                id: event_id,
                repo_id: id,
                kind,
                actor_login: actor.to_string(),
                actor_avatar_url: None,
                title: title.to_string(),
                body_preview: None,
                body: None,
                url: build_event_url(&owner, &name, kind, number, event_id),
                number,
                occurred_at: base - (i as i64) * 1800,
                seen: false,
                team_ids: Vec::new(),
                watched: false,
            });
        }
        let added = state.repos.iter_mut().find(|r| r.id == id).unwrap();
        added.last_polled_at = Some(base);
        added.backfilled_to = Some(base - gitmon::FIRST_POLL_LOOKBACK_SECS);
        state.events.sort_by(|a, b| b.occurred_at.cmp(&a.occurred_at).then(b.id.cmp(&a.id)));

        Ok(repo)
    }

    fn remove_repo(&self, repo_id: u64) -> Result<(), EngineError> {
        let mut state = self.state.lock().unwrap();
        let before = state.repos.len();
        state.repos.retain(|r| r.id != repo_id);
        if state.repos.len() == before {
            return Err(EngineError::not_found("That repo isn't being watched."));
        }
        state.events.retain(|e| e.repo_id != repo_id); // cascades events
        state.pulls.retain(|p| p.repo_id != repo_id); // ... and its pull rows
        Ok(())
    }

    fn list_repos(&self) -> Vec<Repo> {
        self.state.lock().unwrap().repos.iter().filter(|r| !r.hidden).cloned().collect()
    }

    fn list_hidden_repos(&self) -> Vec<Repo> {
        self.state.lock().unwrap().repos.iter().filter(|r| r.hidden).cloned().collect()
    }

    /// The flag and nothing else — no events, pulls or watches are touched,
    /// exactly as the real store's `set_repo_hidden` leaves them. The mock
    /// keeps no watermark worth ageing, so it has no stale-watermark reset to
    /// mirror; what it does mirror is that everything collected comes back the
    /// instant the repo is unhidden.
    fn set_repo_hidden(&self, repo_id: u64, hidden: bool) -> Result<(), EngineError> {
        let mut state = self.state.lock().unwrap();
        let Some(repo) = state.repos.iter_mut().find(|r| r.id == repo_id) else {
            return Err(EngineError::not_found("That repo isn't being watched."));
        };
        repo.hidden = hidden;
        Ok(())
    }

    fn poll_now(&self) -> PollResult {
        let mut state = self.state.lock().unwrap();
        state.polls += 1;
        // One rate-limited repo on the first poll, with a real refill time, so
        // the banner ("resumes at HH:MM") is reachable in the mock. Every
        // later poll comes back clean and the banner drops.
        let errors = if state.polls == RATE_LIMITED_POLL {
            let repo_id = state.repos.first().map(|r| r.id).unwrap_or(1);
            vec![RepoError {
                repo_id,
                // The engine puts `EngineError`'s `Display` here, which is
                // "<kind>: <message>" — the prefix the app matches on.
                message: "rate_limited: API rate limit exceeded".to_string(),
                reset_at: Some(now() + MOCK_RATE_LIMIT_SECS),
            }]
        } else {
            Vec::new()
        };
        if state.repos.is_empty() {
            let n = now();
            return PollResult {
                new_events: vec![],
                errors,
                rate_limit_remaining: Some(5000),
                rate_limit_reset_at: Some(n + seconds_until_next_hour(n)),
                rate_limit_limit: Some(5000),
                // The mock never implements the `X-Poll-Interval` skip logic
                // `PollResult::skipped` describes — every repo it "polls" is
                // actually fetched, so this is always empty.
                skipped: Vec::new(),
            };
        }

        // Hidden repos are not polled and cost nothing — the same rule the
        // real engine gets from `list_repos` being visible-only.
        let pollable: Vec<Repo> = state.repos.iter().filter(|r| !r.hidden).cloned().collect();
        if pollable.is_empty() {
            let n = now();
            return PollResult {
                new_events: vec![],
                errors,
                rate_limit_remaining: Some(5000),
                rate_limit_reset_at: Some(n + seconds_until_next_hour(n)),
                rate_limit_limit: Some(5000),
                skipped: Vec::new(),
            };
        }
        let count = state.rng.range(4); // 0..=3
        let mut new_events = Vec::with_capacity(count);
        let repo_count = pollable.len();
        for _ in 0..count {
            let repo_idx = state.rng.range(repo_count);
            let repo = pollable[repo_idx].clone();
            let kind_idx = state.rng.range(EventKind::ALL.len());
            let kind = EventKind::ALL[kind_idx];
            let actor_idx = state.rng.range(SEED_PEOPLE.len());
            let actor = SEED_PEOPLE[actor_idx];
            let titles = fabricated_titles(kind);
            let title_idx = state.rng.range(titles.len());
            let title = titles[title_idx];
            let number = if matches!(kind, EventKind::Commit) {
                None
            } else {
                Some(100 + state.rng.range(900) as u64)
            };

            let id = self.next_event_id.fetch_add(1, Ordering::Relaxed);
            let event = Event {
                id,
                repo_id: repo.id,
                kind,
                actor_login: actor.to_string(),
                actor_avatar_url: None,
                title: title.to_string(),
                body_preview: None,
                body: None,
                url: build_event_url(&repo.owner, &repo.name, kind, number, id),
                number,
                occurred_at: now(),
                seen: false,
                team_ids: Vec::new(),
                watched: false,
            };
            new_events.push(event.clone());
            state.events.push(event);
        }
        if count > 0 {
            state.events.sort_by(|a, b| b.occurred_at.cmp(&a.occurred_at).then(b.id.cmp(&a.id)));
        }

        let poll_time = now();
        // A hidden repo's watermark simply stops advancing: nothing polled it.
        for r in state.repos.iter_mut().filter(|r| !r.hidden) {
            r.last_polled_at = Some(poll_time);
            // `COALESCE`, as the real store does it: a later poll never raises
            // a floor a backfill has already lowered.
            r.backfilled_to =
                r.backfilled_to.or(Some(poll_time - gitmon::FIRST_POLL_LOOKBACK_SECS));
        }

        let rate_limit_remaining = Some(4000 + state.rng.range(1000) as u32);
        let rate_limit_reset_at = Some(poll_time + seconds_until_next_hour(poll_time));
        // Neither field is stored; the real engine fills both in on the way
        // out of a poll, and so does this.
        resolve_team_ids(&state.teams, &mut new_events);
        resolve_watched(&state.watches, &mut new_events);
        // See the other `PollResult` literal above: the mock always fetches
        // every repo, so `skipped` is always empty.
        PollResult {
            new_events,
            errors,
            rate_limit_remaining,
            rate_limit_reset_at,
            rate_limit_limit: Some(5000),
            skipped: Vec::new(),
        }
    }

    /// Fabricates one older window per repo, mirroring the real engine's rules
    /// exactly: the events land already seen, the poll watermark is untouched,
    /// and each repo's floor steps down by the (clamped) span so calling it
    /// repeatedly walks steadily backwards. A repo with no floor — one added
    /// and never polled — reports `not polled yet` rather than inventing one.
    fn backfill(
        &self,
        repo_id: Option<u64>,
        span_secs: i64,
    ) -> Result<BackfillResult, EngineError> {
        let span =
            span_secs.clamp(gitmon::BACKFILL_MIN_SPAN_SECS, gitmon::BACKFILL_MAX_SPAN_SECS);
        let mut state = self.state.lock().unwrap();
        if let Some(id) = repo_id {
            if !state.repos.iter().any(|r| r.id == id) {
                return Err(EngineError::not_found(format!("no repo with id {id}")));
            }
        }

        let hidden: std::collections::HashSet<u64> =
            state.repos.iter().filter(|r| r.hidden).map(|r| r.id).collect();
        let targets: Vec<(u64, Option<i64>)> = state
            .repos
            .iter()
            .filter(|r| repo_id.is_none_or(|id| r.id == id))
            // The sweep (`repo_id: None`) never reaches a hidden repo at all,
            // exactly as the real engine's does not: `list_repos` is
            // visible-only there. Only an explicit id gets the row below.
            .filter(|r| repo_id.is_some() || !r.hidden)
            .map(|r| (r.id, r.backfilled_to))
            .collect();

        let mut rows = Vec::with_capacity(targets.len());
        let mut fabricated: Vec<Event> = Vec::new();
        for (id, floor) in targets {
            // Reaching further back is a request like any other, and a hidden
            // repo spends none. Only reachable through an explicit `repo_id`:
            // the sweep below never sees one.
            if hidden.contains(&id) {
                rows.push(RepoBackfill {
                    repo_id: id,
                    from: 0,
                    to: 0,
                    inserted: 0,
                    error: Some("hidden".to_string()),
                });
                continue;
            }
            let Some(to) = floor else {
                rows.push(RepoBackfill {
                    repo_id: id,
                    from: 0,
                    to: 0,
                    inserted: 0,
                    error: Some("not polled yet".to_string()),
                });
                continue;
            };
            let from = to - span;
            let repo = state.repos.iter().find(|r| r.id == id).expect("target repo").clone();
            let count = 2 + state.rng.range(3); // 2..=4 per repo per step
            for i in 0..count {
                let actor = SEED_PEOPLE[state.rng.range(SEED_PEOPLE.len())];
                let kind = EventKind::ALL[state.rng.range(EventKind::ALL.len())];
                let titles = fabricated_titles(kind);
                let title = titles[state.rng.range(titles.len())];
                let event_id = self.next_event_id.fetch_add(1, Ordering::Relaxed);
                let number = if matches!(kind, EventKind::Commit) {
                    None
                } else {
                    Some(100 + state.rng.range(900) as u64)
                };
                // Spread across the window, newest first, always inside
                // `[from, to)` so the mock's own history stays consistent.
                let occurred_at = to - 1 - (span / (count as i64 + 1)) * (i as i64 + 1);
                fabricated.push(Event {
                    id: event_id,
                    repo_id: id,
                    kind,
                    actor_login: actor.to_string(),
                    actor_avatar_url: None,
                    title: title.to_string(),
                    body_preview: None,
                    body: None,
                    url: build_event_url(&repo.owner, &repo.name, kind, number, event_id),
                    number,
                    occurred_at,
                    // Backfilled history is never news.
                    seen: true,
                    team_ids: Vec::new(),
                    watched: false,
                });
            }
            state.repos.iter_mut().find(|r| r.id == id).expect("target repo").backfilled_to =
                Some(from);
            rows.push(RepoBackfill {
                repo_id: id,
                from,
                to,
                inserted: count as u64,
                error: None,
            });
        }

        if !fabricated.is_empty() {
            state.events.extend(fabricated);
            state.events.sort_by(|a, b| b.occurred_at.cmp(&a.occurred_at).then(b.id.cmp(&a.id)));
        }
        let rate_limit_remaining = Some(4000 + state.rng.range(1000) as u32);
        let n = now();
        Ok(BackfillResult {
            repos: rows,
            rate_limit_remaining,
            rate_limit_reset_at: Some(n + seconds_until_next_hour(n)),
            rate_limit_limit: Some(5000),
        })
    }

    fn list_events(
        &self,
        repo_id: Option<u64>,
        actor: Option<&str>,
        team_id: Option<u64>,
        mode: FilterMode,
        watched_only: bool,
        before_id: Option<u64>,
        limit: u32,
    ) -> Vec<Event> {
        let state = self.state.lock().unwrap();
        let limit = limit.min(gitmon::MAX_EVENT_LIMIT) as usize;
        let watched_keys: std::collections::HashSet<(u64, u64)> =
            state.watches.iter().map(|w| (w.repo_id, w.number)).collect();
        let team_members: Option<std::collections::HashSet<&str>> = team_id.map(|id| {
            state
                .teams
                .iter()
                .find(|t| t.id == id)
                .map(|t| t.logins.iter().map(|s| s.as_str()).collect())
                .unwrap_or_default()
        });
        // "My team / Everyone" (docs/CONTRACT.md "Filter"): the union of every
        // team's members plus every signed-in account, mirroring the real
        // engine's `list_events(mode)` — including its fresh-install guard,
        // an install with no team member yet reads as "not configured", not
        // "configured to exclude everybody".
        let all_members: std::collections::HashSet<String> = state
            .teams
            .iter()
            .flat_map(|t| t.logins.iter())
            .map(|s| s.to_lowercase())
            .collect();
        let restrict_to: Option<std::collections::HashSet<String>> = match mode {
            FilterMode::All => None,
            FilterMode::Team if all_members.is_empty() => None,
            FilterMode::Team => {
                let mut set = all_members;
                set.extend(state.accounts.iter().map(|a| a.login.to_lowercase()));
                Some(set)
            }
        };
        let visible = visible_repo_ids(&state.repos);
        let mut page: Vec<Event> = state
            .events
            .iter()
            .filter(|e| visible.contains(&e.repo_id))
            .filter(|e| repo_id.map(|id| e.repo_id == id).unwrap_or(true))
            .filter(|e| actor.map(|a| e.actor_login.eq_ignore_ascii_case(a)).unwrap_or(true))
            .filter(|e| match &team_members {
                Some(members) => members.contains(e.actor_login.to_lowercase().as_str()),
                None => true,
            })
            .filter(|e| match &restrict_to {
                Some(set) => set.contains(&e.actor_login.to_lowercase()),
                None => true,
            })
            .filter(|e| {
                !watched_only || e.number.is_some_and(|n| watched_keys.contains(&(e.repo_id, n)))
            })
            .filter(|e| before_id.map(|id| e.id < id).unwrap_or(true))
            .take(limit)
            .cloned()
            .collect();
        resolve_team_ids(&state.teams, &mut page);
        resolve_watched(&state.watches, &mut page);
        page
    }

    fn mark_seen(&self, ids: &[u64]) -> Result<(), EngineError> {
        let mut state = self.state.lock().unwrap();
        let id_set: std::collections::HashSet<u64> = ids.iter().copied().collect();
        for e in state.events.iter_mut() {
            if id_set.contains(&e.id) {
                e.seen = true;
            }
        }
        Ok(())
    }

    fn unseen_count(&self) -> u64 {
        let state = self.state.lock().unwrap();
        let visible = visible_repo_ids(&state.repos);
        state.events.iter().filter(|e| !e.seen && visible.contains(&e.repo_id)).count() as u64
    }

    fn get_thread(&self, repo_id: u64, number: u64) -> Result<Thread, EngineError> {
        let state = self.state.lock().unwrap();
        let mut members: Vec<&Event> =
            state.events.iter().filter(|e| e.repo_id == repo_id && e.number == Some(number)).collect();
        if members.is_empty() {
            return Err(EngineError::not_found(format!("No thread #{number} in this repo.")));
        }
        members.sort_by_key(|e| e.occurred_at);

        let is_issue = members.iter().all(|e| matches!(e.kind, EventKind::IssueOpened | EventKind::IssueCommented));
        let kind = if is_issue { ThreadKind::Issue } else { ThreadKind::Pull };

        let opener = members
            .iter()
            .find(|e| matches!(e.kind, EventKind::PrOpened | EventKind::IssueOpened))
            .or_else(|| members.first())
            .unwrap();
        let title = thread_title(&members);
        let state_str = if members.iter().any(|e| e.kind == EventKind::PrMerged) {
            "merged"
        } else if members.iter().any(|e| e.kind == EventKind::PrClosed) {
            "closed"
        } else {
            "open"
        };

        let repo = state.repos.iter().find(|r| r.id == repo_id);
        let seed = repo_id ^ (number << 20);

        let mut items: Vec<ThreadItem> = members
            .iter()
            .filter_map(|e| match e.kind {
                EventKind::PrReviewed => Some(ThreadItem {
                    kind: ThreadItemKind::Review,
                    actor_login: e.actor_login.clone(),
                    actor_avatar_url: e.actor_avatar_url.clone(),
                    body: e.body_preview.clone(),
                    state: Some(review_state_from_title(&e.title)),
                    path: None,
                    line: None,
                    url: e.url.clone(),
                    at: e.occurred_at,
                    sha: None,
                }),
                EventKind::PrCommented | EventKind::IssueCommented => Some(ThreadItem {
                    kind: ThreadItemKind::Comment,
                    actor_login: e.actor_login.clone(),
                    actor_avatar_url: e.actor_avatar_url.clone(),
                    body: e.body_preview.clone(),
                    state: None,
                    path: None,
                    line: None,
                    url: e.url.clone(),
                    at: e.occurred_at,
                    sha: None,
                }),
                _ => None,
            })
            .collect();

        // The contract's inline `review_comment` (path + line) has no
        // matching stored EventKind — it only ever surfaces via a live
        // get_thread fetch. Fabricate one plausible example per pull thread
        // so the detail pane's file/line rendering has something real to
        // show, deterministic on (repo_id, number) so repeat opens agree.
        if kind == ThreadKind::Pull {
            let reviewer = members.iter().find(|e| e.kind == EventKind::PrReviewed).unwrap_or(opener);
            items.push(ThreadItem {
                kind: ThreadItemKind::ReviewComment,
                actor_login: reviewer.actor_login.clone(),
                actor_avatar_url: reviewer.actor_avatar_url.clone(),
                body: Some(inline_comment_body(seed)),
                state: None,
                path: Some(inline_comment_path(seed)),
                line: Some(20 + (seed % 180) as u32),
                url: opener.url.clone(),
                at: opener.occurred_at + 60,
                sha: None,
            });

            // Three synthetic commits for the Commits tab (packet's mock
            // seed): the mock has no live GitHub commit list to draw a PR's
            // commits from, so this fabricates one deterministically —
            // `get_commit` reconstructs the same sha/title/actor from the
            // sha alone, so clicking through lands on matching content.
            let repo_slug = repo.map(|r| format!("https://github.com/{}/{}", r.owner, r.name)).unwrap_or_default();
            for i in 0..3u64 {
                let bits = seeded(seed, 100 + i) & PR_COMMIT_SHA_MASK;
                let (sha, title) = synthetic_pr_commit(bits);
                let actor = SEED_PEOPLE[(bits as usize) % SEED_PEOPLE.len()];
                items.push(ThreadItem {
                    kind: ThreadItemKind::Commit,
                    actor_login: actor.to_string(),
                    actor_avatar_url: None,
                    body: Some(title.to_string()),
                    state: None,
                    path: None,
                    line: None,
                    url: format!("{repo_slug}/commit/{sha}"),
                    at: opener.occurred_at + 120 * (i as i64 + 1),
                    sha: Some(sha),
                });
            }
        }
        items.sort_by_key(|i| i.at);

        Ok(Thread {
            repo_id,
            number,
            kind,
            title: title.clone(),
            state: state_str.to_string(),
            author_login: opener.actor_login.clone(),
            author_avatar_url: opener.actor_avatar_url.clone(),
            body: Some(fabricated_body(&title, seed)),
            url: opener.url.clone(),
            created_at: opener.occurred_at,
            head_ref: (kind == ThreadKind::Pull).then(|| format!("{}/{}", opener.actor_login, slugify(&title))),
            base_ref: (kind == ThreadKind::Pull).then(|| repo.map(|r| r.default_branch.clone()).unwrap_or_else(|| "main".to_string())),
            additions: (kind == ThreadKind::Pull).then(|| 12 + (seed % 220) as u32),
            deletions: (kind == ThreadKind::Pull).then(|| 3 + (seed % 60) as u32),
            changed_files: (kind == ThreadKind::Pull).then(|| 1 + (seed % 6) as u32),
            items,
        })
    }

    fn get_commit(&self, repo_id: u64, sha: &str) -> Result<CommitDetail, EngineError> {
        let state = self.state.lock().unwrap();
        let sha_bits = u64::from_str_radix(sha, 16).unwrap_or(0x1234);
        let seed = repo_id ^ sha_bits;
        let files = fabricate_commit_files(seed);
        let additions = files.iter().map(|f| f.additions).sum();
        let deletions = files.iter().map(|f| f.deletions).sum();

        if let Some(event) = state
            .events
            .iter()
            .find(|e| e.repo_id == repo_id && e.kind == EventKind::Commit && e.url.ends_with(&format!("/commit/{sha}")))
        {
            return Ok(CommitDetail {
                repo_id,
                sha: sha.to_string(),
                message: format!("{}\n\n{}", event.title, fabricated_body(&event.title, seed)),
                author_login: event.actor_login.clone(),
                author_avatar_url: event.actor_avatar_url.clone(),
                url: event.url.clone(),
                at: event.occurred_at,
                additions,
                deletions,
                files,
            });
        }

        // Falls back to a PR-thread synthetic commit (see get_thread's
        // Commits-tab fabrication): the mock has no repo-wide `Event` for
        // these, only a `ThreadItem`. Reconstructed deterministically from
        // the sha alone — the same bits `synthetic_pr_commit` started from —
        // so a click-through shows the same title/actor the Commits tab did.
        let bits = sha_bits & PR_COMMIT_SHA_MASK;
        let (canonical_sha, title) = synthetic_pr_commit(bits);
        if canonical_sha != sha {
            return Err(EngineError::not_found(format!("No commit {sha} in this repo.")));
        }
        let actor = SEED_PEOPLE[(bits as usize) % SEED_PEOPLE.len()];
        let repo_slug = state
            .repos
            .iter()
            .find(|r| r.id == repo_id)
            .map(|r| format!("https://github.com/{}/{}", r.owner, r.name))
            .unwrap_or_default();
        Ok(CommitDetail {
            repo_id,
            sha: sha.to_string(),
            message: format!("{title}\n\n{}", fabricated_body(title, seed)),
            author_login: actor.to_string(),
            author_avatar_url: None,
            url: format!("{repo_slug}/commit/{sha}"),
            at: now() - 3600,
            additions,
            deletions,
            files,
        })
    }

    fn get_pull_files(&self, repo_id: u64, number: u64) -> Result<Vec<CommitFile>, EngineError> {
        let state = self.state.lock().unwrap();
        let exists = state.events.iter().any(|e| e.repo_id == repo_id && e.number == Some(number))
            || state.watches.iter().any(|w| w.repo_id == repo_id && w.number == number);
        if !exists {
            return Err(EngineError::not_found(format!("No thread #{number} in this repo.")));
        }
        let seed = repo_id ^ (number << 20);
        Ok(fabricate_pull_files(seed))
    }

    fn list_watches(&self) -> Vec<Watch> {
        let state = self.state.lock().unwrap();
        let visible = visible_repo_ids(&state.repos);
        let mut watches: Vec<Watch> =
            state.watches.iter().filter(|w| visible.contains(&w.repo_id)).cloned().collect();
        watches.sort_by_key(|w| std::cmp::Reverse(w.since));
        watches
    }

    fn watch_thread(&self, repo_id: u64, number: u64) -> Result<Watch, EngineError> {
        let mut state = self.state.lock().unwrap();
        if let Some(w) = state.watches.iter().find(|w| w.repo_id == repo_id && w.number == number) {
            return Ok(w.clone());
        }
        // Derives kind/title/state from stored events for this thread, like
        // the real engine does; falls back to a generic placeholder when
        // nothing is stored yet (the mock has no live GitHub to fetch from).
        let members: Vec<&Event> =
            state.events.iter().filter(|e| e.repo_id == repo_id && e.number == Some(number)).collect();
        let (kind, title, state_str) = if !members.is_empty() {
            let is_issue =
                members.iter().all(|e| matches!(e.kind, EventKind::IssueOpened | EventKind::IssueCommented));
            let kind = if is_issue { ThreadKind::Issue } else { ThreadKind::Pull };
            let title = thread_title(&members);
            let state_str = if members.iter().any(|e| e.kind == EventKind::PrMerged) {
                "merged"
            } else if members.iter().any(|e| e.kind == EventKind::PrClosed) {
                "closed"
            } else {
                "open"
            };
            (kind, title, state_str.to_string())
        } else {
            (ThreadKind::Pull, format!("#{number}"), "open".to_string())
        };
        let watch = Watch { repo_id, number, kind, title, state: state_str, source: WatchSource::Manual, since: now() };
        state.watches.push(watch.clone());
        Ok(watch)
    }

    fn unwatch_thread(&self, repo_id: u64, number: u64) -> Result<(), EngineError> {
        let mut state = self.state.lock().unwrap();
        let before = state.watches.len();
        state.watches.retain(|w| !(w.repo_id == repo_id && w.number == number));
        if state.watches.len() == before {
            return Err(EngineError::not_found(format!("{repo_id}#{number} is not watched")));
        }
        Ok(())
    }

    fn clear_closed_watches(&self) -> u64 {
        let mut state = self.state.lock().unwrap();
        let before = state.watches.len();
        state.watches.retain(|w| w.state == "open");
        (before - state.watches.len()) as u64
    }

    /// Mirrors `Engine::digest` (crates/gitmon/src/engine.rs) against the
    /// mock's own in-memory events instead of a SQL store: same half-open
    /// window, same team/actor scoping, same ranking and truncation, so the
    /// Summary view gets real numbers under `GITMON_MOCK=1`.
    fn digest(
        &self,
        start: i64,
        end: i64,
        team_id: Option<u64>,
        actor: Option<&str>,
        tz_offset_secs: i32,
    ) -> Result<Digest, EngineError> {
        if end <= start {
            return Err(EngineError::invalid(format!(
                "digest needs end after start (start {start}, end {end})"
            )));
        }

        let state = self.state.lock().unwrap();

        // A `team_id` matching no team yields an empty digest (see the real
        // engine's doc on `digest`), not an error — `unwrap_or_default()`
        // gives an empty set, which matches nobody below.
        let team_logins: Option<std::collections::HashSet<String>> = team_id.map(|id| {
            state
                .teams
                .iter()
                .find(|t| t.id == id)
                .map(|t| t.logins.iter().cloned().collect())
                .unwrap_or_default()
        });

        // Split out of `in_window` because the PR timings scope by the PR's
        // *author* — the actor on its `pr_opened` — while the window itself
        // bounds the merge or the first review, not the opening.
        let scope_ok = |login: &str| -> bool {
            if let Some(a) = actor {
                if !login.eq_ignore_ascii_case(a) {
                    return false;
                }
            }
            if let Some(logins) = &team_logins {
                if !logins.iter().any(|l| l.eq_ignore_ascii_case(login)) {
                    return false;
                }
            }
            true
        };
        let visible = visible_repo_ids(&state.repos);
        let in_window = |e: &Event| -> bool {
            e.occurred_at >= start
                && e.occurred_at < end
                && visible.contains(&e.repo_id)
                && scope_ok(&e.actor_login)
        };

        // Open-PR counts are current state, not window state — the same rule
        // the real engine's `open_pull_person_counts` follows.
        let mut authored: HashMap<String, u64> = HashMap::new();
        let mut review_queue: HashMap<String, u64> = HashMap::new();
        for pull in state.pulls.iter().filter(|p| visible.contains(&p.repo_id)) {
            *authored.entry(pull.author_login.to_lowercase()).or_insert(0) += 1;
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            for reviewer in &pull.requested_reviewers {
                if seen.insert(reviewer.to_lowercase()) {
                    *review_queue.entry(reviewer.to_lowercase()).or_insert(0) += 1;
                }
            }
        }

        let mut totals = KindCounts::default();
        let mut by_login: HashMap<String, DigestPerson> = HashMap::new();
        for e in state.events.iter().filter(|e| in_window(e)) {
            totals.add(e.kind, 1);
            let key = e.actor_login.to_lowercase();
            let entry = by_login.entry(key.clone()).or_insert_with(|| DigestPerson {
                login: e.actor_login.clone(),
                avatar_url: e.actor_avatar_url.clone(),
                total: 0,
                counts: KindCounts::default(),
                open_prs: authored.get(&key).copied().unwrap_or(0),
                review_queue: review_queue.get(&key).copied().unwrap_or(0),
                last_at: i64::MIN,
            });
            entry.total += 1;
            entry.counts.add(e.kind, 1);
            entry.last_at = entry.last_at.max(e.occurred_at);
            if entry.avatar_url.is_none() {
                entry.avatar_url = e.actor_avatar_url.clone();
            }
        }
        // Counted before the top-20 cut, like `totals`.
        let people_total = by_login.len() as u64;
        let mut people: Vec<DigestPerson> = by_login.into_values().collect();
        people.sort_by(|a, b| {
            b.total.cmp(&a.total).then_with(|| a.login.to_lowercase().cmp(&b.login.to_lowercase()))
        });
        people.truncate(DIGEST_PEOPLE_LIMIT);

        // Per thread: events, and the newest member's kind/title/url. `state.events`
        // is kept sorted newest-first by every insertion path (seeding, poll_now,
        // add_repo), so the first matching event seen per (repo_id, number) here
        // is the thread's newest — the same pick the real store's
        // `ROW_NUMBER() ... ORDER BY occurred_at DESC, id DESC` makes.
        struct ThreadAgg {
            events: u64,
            last_at: i64,
            last_kind: EventKind,
            title: String,
            url: String,
        }
        let mut by_thread: HashMap<(u64, u64), ThreadAgg> = HashMap::new();
        let mut repo_totals: HashMap<u64, u64> = HashMap::new();
        for e in state.events.iter().filter(|e| in_window(e)) {
            *repo_totals.entry(e.repo_id).or_insert(0) += 1;
            if let Some(number) = e.number {
                let agg = by_thread.entry((e.repo_id, number)).or_insert_with(|| ThreadAgg {
                    events: 0,
                    last_at: e.occurred_at,
                    last_kind: e.kind,
                    title: e.title.clone(),
                    url: e.url.clone(),
                });
                agg.events += 1;
            }
        }
        let mut threads: Vec<DigestThread> = by_thread
            .into_iter()
            .filter_map(|((repo_id, number), agg)| {
                Some(DigestThread {
                    repo_id,
                    number,
                    kind: agg.last_kind.thread_kind()?,
                    title: agg.title,
                    url: agg.url,
                    events: agg.events,
                    last_at: agg.last_at,
                })
            })
            .collect();
        threads.sort_by(|a, b| {
            b.events
                .cmp(&a.events)
                .then_with(|| b.last_at.cmp(&a.last_at))
                .then_with(|| (a.repo_id, a.number).cmp(&(b.repo_id, b.number)))
        });
        threads.truncate(DIGEST_THREAD_LIMIT);

        // Per repo per commit author, for the concentration — commits only,
        // exactly the rows the real store's `GROUP BY repo_id, actor_login`
        // read returns.
        let mut commit_authors: HashMap<u64, HashMap<String, (String, u64)>> = HashMap::new();
        for e in state.events.iter().filter(|e| in_window(e) && e.kind == EventKind::Commit) {
            let entry = commit_authors
                .entry(e.repo_id)
                .or_default()
                .entry(e.actor_login.to_lowercase())
                .or_insert_with(|| (e.actor_login.clone(), 0));
            entry.1 += 1;
            // The canonical casing is the lowest one, as `MIN(actor_login)`
            // picks in the real store.
            if e.actor_login < entry.0 {
                entry.0 = e.actor_login.clone();
            }
        }

        let mut repos: Vec<DigestRepo> = repo_totals
            .into_iter()
            .filter(|(_, total)| *total > 0)
            .map(|(repo_id, total)| {
                let authors: Vec<(String, u64)> = commit_authors
                    .get(&repo_id)
                    .map(|by_login| by_login.values().cloned().collect())
                    .unwrap_or_default();
                let (top_author_login, top_author_share) = mock_top_commit_author(&authors);
                DigestRepo { repo_id, total, top_author_login, top_author_share }
            })
            .collect();
        repos.sort_by(|a, b| b.total.cmp(&a.total).then_with(|| a.repo_id.cmp(&b.repo_id)));

        // Series and heatmap, folded out of local-hour buckets exactly as the
        // real engine's `fold_buckets` does, so the mock cannot put an event
        // on a different day from the engine it stands in for.
        let mut buckets: HashMap<(i64, EventKind), u64> = HashMap::new();
        for e in state.events.iter().filter(|e| in_window(e)) {
            let bucket = (e.occurred_at + tz_offset_secs as i64).div_euclid(3600);
            *buckets.entry((bucket, e.kind)).or_insert(0) += 1;
        }
        let buckets: Vec<(i64, EventKind, u64)> =
            buckets.into_iter().map(|((b, k), n)| (b, k, n)).collect();
        let (series, hours) = fold_buckets(start, end, tz_offset_secs, &buckets);

        let pr_timing = mock_pr_timing(&state.events, start, end, &scope_ok);
        let unreviewed_merges = mock_unreviewed_merges(
            &state.events,
            start,
            end,
            actor.is_some() || team_logins.is_some(),
            &scope_ok,
        );

        Ok(Digest {
            start,
            end,
            team_id,
            actor: actor.map(str::to_string),
            totals,
            people,
            people_total,
            threads,
            repos,
            series,
            hours,
            pr_timing,
            unreviewed_merges,
        })
    }

    /// Mirrors `Engine::open_pulls`: open only, oldest first, scoped by the
    /// author's team membership, by one author, or by who is being asked to
    /// review — the three intersecting.
    fn open_pulls(
        &self,
        team_id: Option<u64>,
        actor: Option<&str>,
        reviewer: Option<&str>,
    ) -> Result<Vec<OpenPull>, EngineError> {
        let state = self.state.lock().unwrap();
        // An unknown team matches nobody, exactly as the digest's does.
        let team_logins: Option<Vec<String>> = team_id.map(|id| {
            state
                .teams
                .iter()
                .find(|t| t.id == id)
                .map(|t| t.logins.clone())
                .unwrap_or_default()
        });
        let visible = visible_repo_ids(&state.repos);
        let mut pulls: Vec<OpenPull> = state
            .pulls
            .iter()
            .filter(|p| visible.contains(&p.repo_id))
            .filter(|p| {
                if let Some(actor) = actor {
                    if !p.author_login.eq_ignore_ascii_case(actor) {
                        return false;
                    }
                }
                if let Some(logins) = &team_logins {
                    if !logins.iter().any(|l| l.eq_ignore_ascii_case(&p.author_login)) {
                        return false;
                    }
                }
                if let Some(reviewer) = reviewer {
                    if !p.requested_reviewers.iter().any(|r| r.eq_ignore_ascii_case(reviewer)) {
                        return false;
                    }
                }
                true
            })
            .cloned()
            .collect();
        pulls.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| (a.repo_id, a.number).cmp(&(b.repo_id, b.number)))
        });
        Ok(pulls)
    }

    fn get_settings(&self) -> Settings {
        self.state.lock().unwrap().settings.clone()
    }

    fn set_settings(&self, s: Settings) -> Result<(), EngineError> {
        if s.poll_interval_secs < 30 {
            return Err(EngineError::invalid("Poll interval must be at least 30 seconds."));
        }
        self.state.lock().unwrap().settings = s;
        Ok(())
    }
}

/// Derives a thread's display title from its member events: prefer the
/// opening event's title (it *is* the PR/issue title in this contract),
/// else strip the "Comment on #N " prefix seed data uses for bare comments,
/// else fall back to whatever title the earliest event carries.
fn thread_title(members: &[&Event]) -> String {
    if let Some(opener) = members.iter().find(|e| matches!(e.kind, EventKind::PrOpened | EventKind::IssueOpened)) {
        return opener.title.clone();
    }
    let earliest = members[0];
    if let Some(rest) = earliest.title.split_once("Comment on #") {
        // rest.1 looks like "412 Add retry budget" — drop the leading number.
        if let Some((_, after_num)) = rest.1.split_once(' ') {
            return after_num.to_string();
        }
    }
    earliest.title.clone()
}

/// Maps a `pr_reviewed` event's `title` ("Review: approved" / "Review:
/// changes requested" / "Review: commented") to the ThreadItem `state`
/// string a real GitHub review carries.
fn review_state_from_title(title: &str) -> String {
    if title.contains("approved") {
        "approved".to_string()
    } else if title.contains("changes requested") {
        "changes_requested".to_string()
    } else {
        "commented".to_string()
    }
}

fn slugify(title: &str) -> String {
    title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .take(4)
        .collect::<Vec<_>>()
        .join("-")
}

/// A short deterministic "random" stream seeded once, for fabricating
/// plausible detail-pane content without a `rand` dependency.
fn seeded(seed: u64, salt: u64) -> u64 {
    let mut x = seed ^ salt ^ 0x9E3779B97F4A7C15;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    x
}

fn fabricated_body(title: &str, seed: u64) -> String {
    const OPENERS: [&str; 4] = [
        "This picks up where the linked discussion left off.",
        "Small, focused change — see the diff for the exact scope.",
        "Follows the same pattern used elsewhere in this repo.",
        "Split out of a larger change to keep the review manageable.",
    ];
    let opener = OPENERS[(seeded(seed, 1) as usize) % OPENERS.len()];
    format!("{opener}\n\n**What changed:** {title}.\n\nNo user-facing behavior change outside what's described above.")
}

fn inline_comment_path(seed: u64) -> String {
    const PATHS: [&str; 6] =
        ["src/lib.rs", "src/handlers/webhook.rs", "src/poller.rs", "src/db/events.rs", "src/client/github.rs", "tests/integration.rs"];
    PATHS[(seeded(seed, 2) as usize) % PATHS.len()].to_string()
}

fn inline_comment_body(seed: u64) -> String {
    const BODIES: [&str; 4] = [
        "Nit: this could use the existing helper instead of re-implementing it here.",
        "Worth a short comment explaining why this branch is needed.",
        "Double-checked this against the fixture data — looks right.",
        "Should this be behind the same feature flag as the rest of the change?",
    ];
    BODIES[(seeded(seed, 3) as usize) % BODIES.len()].to_string()
}

fn fabricate_commit_files(seed: u64) -> Vec<CommitFile> {
    const CANDIDATES: [(&str, &str); 6] = [
        ("src/lib.rs", "modified"),
        ("src/poller.rs", "modified"),
        ("src/db/events.rs", "modified"),
        ("tests/integration.rs", "modified"),
        ("CHANGELOG.md", "modified"),
        ("src/client/github.rs", "added"),
    ];
    let count = 2 + (seeded(seed, 10) % 3) as usize; // 2..=4 files
    (0..count)
        .map(|i| {
            let (path, status) = CANDIDATES[(seeded(seed, 20 + i as u64) as usize) % CANDIDATES.len()];
            let additions = 3 + (seeded(seed, 30 + i as u64) % 40) as u32;
            let deletions = (seeded(seed, 40 + i as u64) % 15) as u32;
            let patch = format!(
                "@@ -1,{ctx} +1,{ctx_new} @@\n-old line in {path}\n+new line in {path}\n+another added line",
                ctx = deletions.max(1),
                ctx_new = additions.max(1),
            );
            CommitFile { path: path.to_string(), status: status.to_string(), additions, deletions, patch: Some(patch) }
        })
        .collect()
}

/// Masks a seed down to 28 bits — exactly what `{:07x}` prints back out —
/// so `synthetic_pr_commit` round-trips: the sha `get_thread` fabricates and
/// the sha `get_commit` is later asked to look up carry the same bits.
const PR_COMMIT_SHA_MASK: u64 = 0xFFF_FFFF;

/// One synthetic commit for a PR's Commits tab (packet's mock seed: "a PR
/// thread with three commit items carrying sha"). `bits` must already be
/// masked to [`PR_COMMIT_SHA_MASK`] — both call sites do this, so the same
/// bits always regenerate the same (sha, title) pair, letting `get_commit`
/// reconstruct a synthetic commit from its sha alone.
fn synthetic_pr_commit(bits: u64) -> (String, &'static str) {
    const TITLES: [&str; 6] = [
        "Add feature flag for the new behavior, default off",
        "Refactor to use a composite key",
        "Add a regression test",
        "Fix flaky assertion under load",
        "Tidy up error handling",
        "Address review feedback",
    ];
    let sha = format!("{bits:07x}");
    let title = TITLES[(bits as usize) % TITLES.len()];
    (sha, title)
}

/// Five fabricated files for a PR's Files tab (packet's mock seed: "five
/// files, one with `patch: null`") — deterministic on `seed` so repeat opens
/// of the same PR agree. The last file is always the oversized one, matching
/// the mockup's "vendor/generated_stub.go … too large to show here".
fn fabricate_pull_files(seed: u64) -> Vec<CommitFile> {
    const CANDIDATES: [(&str, &str); 5] = [
        ("webhook/dispatcher.go", "modified"),
        ("webhook/retry_budget_test.go", "added"),
        ("webhook/config.go", "modified"),
        ("CHANGELOG.md", "modified"),
        ("vendor/generated_stub.go", "modified"),
    ];
    CANDIDATES
        .iter()
        .enumerate()
        .map(|(i, (path, status))| {
            let additions = 3 + (seeded(seed, 60 + i as u64) % 40) as u32;
            let deletions = (seeded(seed, 70 + i as u64) % 15) as u32;
            let too_large = i == CANDIDATES.len() - 1;
            let patch = (!too_large).then(|| {
                format!(
                    "@@ -1,{ctx} +1,{ctx_new} @@\n-old line in {path}\n+new line in {path}\n+another added line",
                    ctx = deletions.max(1),
                    ctx_new = additions.max(1),
                )
            });
            CommitFile { path: path.to_string(), status: status.to_string(), additions, deletions, patch }
        })
        .collect()
}

fn kind_plural(k: EventKind) -> &'static str {
    use EventKind::*;
    match k {
        Commit => "commits",
        PrOpened | PrMerged | PrClosed => "PRs",
        PrReviewed => "reviews",
        PrCommented => "PR comments",
        IssueOpened => "issues",
        IssueCommented => "issue comments",
    }
}

/// The contract's login normalisation for a saved team: trimmed, lowercased,
/// empties dropped, order preserved, deduped — the same rules
/// `Engine::create_team` applies.
fn normalize_logins(logins: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    logins
        .iter()
        .map(|l| l.trim().to_lowercase())
        .filter(|l| !l.is_empty())
        .filter(|l| seen.insert(l.clone()))
        .collect()
}

/// Fills in `Event.watched` from the watches as they stand right now — the
/// real engine resolves this at every read too (docs/CONTRACT.md "Watched
/// threads": "Read time"), never storing it on the event. A commit has no
/// `number`, so it is never watched, matching the contract.
/// The ids of the repos that are not hidden.
///
/// Every read below is filtered through this, mirroring the real store's
/// `not_hidden` clause: a hidden repo contributes no events, no pulls, no
/// watches and no unseen count, while all of it stays in place waiting to come
/// back the moment the repo is unhidden.
fn visible_repo_ids(repos: &[Repo]) -> std::collections::HashSet<u64> {
    repos.iter().filter(|r| !r.hidden).map(|r| r.id).collect()
}

fn resolve_watched(watches: &[Watch], events: &mut [Event]) {
    let keys: std::collections::HashSet<(u64, u64)> =
        watches.iter().map(|w| (w.repo_id, w.number)).collect();
    for event in events {
        event.watched = event.number.is_some_and(|n| keys.contains(&(event.repo_id, n)));
    }
}

/// Fills in `Event.team_ids` from the teams as they stand right now, which
/// is what the real engine does on every read: membership is never stored on
/// an event, so editing a team re-categorises the feed at once.
fn resolve_team_ids(teams: &[Team], events: &mut [Event]) {
    for event in events {
        let actor = event.actor_login.to_lowercase();
        event.team_ids = teams
            .iter()
            .filter(|t| t.logins.contains(&actor))
            .map(|t| t.id)
            .collect();
    }
}

/// Mirrors the real engine's `fold_buckets`: one gapless entry per local day
/// in the window, and the 7x24 weekday-by-hour grid, both folded out of the
/// same local-hour buckets so they cannot disagree. Monday is weekday 0, and
/// the unix epoch was a Thursday — hence the `+ 3`.
fn fold_buckets(
    start: i64,
    end: i64,
    tz_offset_secs: i32,
    buckets: &[(i64, EventKind, u64)],
) -> (Vec<DayCounts>, Vec<Vec<u64>>) {
    let tz = tz_offset_secs as i64;
    // Half-open: a window ending exactly at local midnight covers the day
    // before it, not the one about to start.
    let first_day = (start + tz).div_euclid(86_400);
    let last_day = (end - 1 + tz).div_euclid(86_400);
    let span = (last_day - first_day + 1).clamp(0, DIGEST_MAX_SERIES_DAYS as i64) as usize;

    let mut series: Vec<DayCounts> = (0..span as i64)
        .map(|offset| DayCounts {
            day: (first_day + offset) * 86_400 - tz,
            counts: KindCounts::default(),
        })
        .collect();
    let mut hours: Vec<Vec<u64>> = vec![vec![0; 24]; 7];

    for (bucket, kind, count) in buckets {
        let day = bucket.div_euclid(24);
        let hour = bucket.rem_euclid(24) as usize;
        let weekday = (day + 3).rem_euclid(7) as usize;
        hours[weekday][hour] += count;
        if let Ok(index) = usize::try_from(day - first_day) {
            if let Some(entry) = series.get_mut(index) {
                entry.counts.add(*kind, *count);
            }
        }
    }
    (series, hours)
}

/// Mirrors the real engine's `Store::pr_timing_samples` plus its median: time
/// to merge from `pr_opened` to `pr_merged`, time to first review from
/// `pr_opened` to the earliest `pr_reviewed`, both joined on
/// `(repo_id, number)`, both scoped by the PR's author, and both counted only
/// when the *closing* event falls inside the window.
fn mock_pr_timing(
    events: &[Event],
    start: i64,
    end: i64,
    scope_ok: &dyn Fn(&str) -> bool,
) -> PrTiming {
    let key = |e: &Event| e.number.map(|n| (e.repo_id, n));
    let mut opened: HashMap<(u64, u64), &Event> = HashMap::new();
    let mut merged: HashMap<(u64, u64), i64> = HashMap::new();
    let mut first_review: HashMap<(u64, u64), i64> = HashMap::new();
    for e in events {
        let Some(k) = key(e) else { continue };
        match e.kind {
            EventKind::PrOpened => {
                opened.entry(k).or_insert(e);
            }
            EventKind::PrMerged => {
                let at = merged.entry(k).or_insert(e.occurred_at);
                *at = (*at).min(e.occurred_at);
            }
            EventKind::PrReviewed => {
                let at = first_review.entry(k).or_insert(e.occurred_at);
                *at = (*at).min(e.occurred_at);
            }
            _ => {}
        }
    }

    let mut ttm: Vec<i64> = Vec::new();
    let mut ttfr: Vec<i64> = Vec::new();
    let mut measured: std::collections::HashSet<(u64, u64)> = std::collections::HashSet::new();
    for (k, open_event) in &opened {
        if !scope_ok(&open_event.actor_login) {
            continue;
        }
        if let Some(at) = merged.get(k) {
            if *at >= start && *at < end && *at >= open_event.occurred_at {
                ttm.push(at - open_event.occurred_at);
                measured.insert(*k);
            }
        }
        if let Some(at) = first_review.get(k) {
            if *at >= start && *at < end && *at >= open_event.occurred_at {
                ttfr.push(at - open_event.occurred_at);
                measured.insert(*k);
            }
        }
    }

    ttm.sort_unstable();
    ttfr.sort_unstable();
    PrTiming {
        median_ttm_secs: median(&ttm),
        median_ttfr_secs: median(&ttfr),
        p90_ttm_secs: percentile_90(&ttm),
        p90_ttfr_secs: percentile_90(&ttfr),
        samples: measured.len() as u64,
    }
}

/// Mirrors the real engine's `Store::unreviewed_merges`: pull requests merged
/// inside the window with no `pr_reviewed` event at any time up to the merge,
/// scoped by the PR's author. A merge whose `pr_opened` was never seeded has no
/// known author, so it counts unscoped and is dropped once a scope is asked
/// for — the same rule the SQL's conditional join encodes.
fn mock_unreviewed_merges(
    events: &[Event],
    start: i64,
    end: i64,
    scoped: bool,
    scope_ok: &dyn Fn(&str) -> bool,
) -> u64 {
    let mut opened: HashMap<(u64, u64), &Event> = HashMap::new();
    let mut reviews: HashMap<(u64, u64), Vec<i64>> = HashMap::new();
    for e in events {
        let Some(number) = e.number else { continue };
        match e.kind {
            EventKind::PrOpened => {
                opened.entry((e.repo_id, number)).or_insert(e);
            }
            EventKind::PrReviewed => {
                reviews.entry((e.repo_id, number)).or_default().push(e.occurred_at);
            }
            _ => {}
        }
    }

    let mut count = 0;
    for e in events {
        let Some(number) = e.number else { continue };
        if e.kind != EventKind::PrMerged || e.occurred_at < start || e.occurred_at >= end {
            continue;
        }
        let key = (e.repo_id, number);
        match opened.get(&key) {
            Some(open_event) => {
                if !scope_ok(&open_event.actor_login) {
                    continue;
                }
            }
            // No stored opening: nothing names the author, so a scoped digest
            // cannot claim it.
            None if scoped => continue,
            None => {}
        }
        let reviewed = reviews
            .get(&key)
            .is_some_and(|at| at.iter().any(|reviewed_at| *reviewed_at <= e.occurred_at));
        if !reviewed {
            count += 1;
        }
    }
    count
}

/// The busiest commit author in one repo's window and their share, mirroring
/// the real engine's `top_commit_author`: most commits wins, a tie goes to the
/// lowest login compared case-insensitively, and no commits means
/// `(None, 0.0)`.
fn mock_top_commit_author(authors: &[(String, u64)]) -> (Option<String>, f32) {
    let total: u64 = authors.iter().map(|(_, commits)| *commits).sum();
    if total == 0 {
        return (None, 0.0);
    }
    let top = authors
        .iter()
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.to_lowercase().cmp(&a.0.to_lowercase())));
    match top {
        Some((login, commits)) => (Some(login.clone()), *commits as f32 / total as f32),
        None => (None, 0.0),
    }
}

/// The median of an ascending sample set, `None` when empty; an even count
/// averages the two middle values, rounding toward zero — the real engine's
/// rule.
fn median(samples: &[i64]) -> Option<i64> {
    if samples.is_empty() {
        return None;
    }
    let mid = samples.len() / 2;
    if samples.len() % 2 == 1 {
        Some(samples[mid])
    } else {
        Some(((samples[mid - 1] as i128 + samples[mid] as i128) / 2) as i64)
    }
}

/// The 90th percentile of an ascending sample set — R-7 linear interpolation
/// at rank `0.9 * (n - 1)`, held in tenths as integer arithmetic and rounded
/// up, which is the real engine's `percentile_90` rule verbatim. See
/// `gitmon::PrTiming` for why it interpolates rather than taking the nearest
/// rank.
fn percentile_90(samples: &[i64]) -> Option<i64> {
    if samples.is_empty() {
        return None;
    }
    let position = 9 * (samples.len() as i128 - 1);
    let index = (position / 10) as usize;
    let remainder = position % 10;
    let lower = samples[index] as i128;
    if remainder == 0 {
        return Some(lower as i64);
    }
    let gap = samples[index + 1] as i128 - lower;
    Some((lower + (remainder * gap + 9) / 10) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `add_account` upserts by login, claims every unclaimed repo, and
    /// leaves an already-claimed repo alone — the packet's product decision
    /// ("a repo is bound to the account that added it").
    #[test]
    fn add_account_claims_only_the_unclaimed_repos() {
        let engine = MockEngine::new();
        assert!(engine.list_accounts().is_empty(), "a fresh mock has no accounts");
        assert!(
            engine.list_repos().iter().all(|r| r.account_login.is_empty()),
            "the seeded repos start unclaimed"
        );
        // Repo 1 (index 1) is already spoken for, as if an earlier account
        // had claimed it.
        engine.state.lock().unwrap().repos[1].account_login = "someone-else".to_string();

        let account = engine.add_account("alice").unwrap();
        assert_eq!(account.login, "alice");
        assert_eq!(engine.list_accounts(), vec![account.clone()]);

        let repos = engine.list_repos();
        assert_eq!(repos[0].account_login, "alice", "unclaimed repo goes to the new account");
        assert_eq!(repos[1].account_login, "someone-else", "an already-claimed repo is untouched");
        assert_eq!(repos[2].account_login, "alice", "unclaimed repo goes to the new account");

        // Re-adding the same login replaces nothing observable (no second
        // row, same `added_at`) rather than erroring.
        let again = engine.add_account("ALICE").unwrap();
        assert_eq!(again, account, "re-authenticating returns the existing row");
        assert_eq!(engine.list_accounts().len(), 1);
    }

    /// The mock's stand-in for "GitHub rejected this token" — no account is
    /// created and nothing is claimed.
    #[test]
    fn add_account_rejects_the_sentinel_dead_token() {
        let engine = MockEngine::new();
        let err = engine.add_account("revoked").unwrap_err();
        assert_eq!(err.kind, gitmon::ErrorKind::Auth);
        assert!(engine.list_accounts().is_empty());
    }

    #[test]
    fn remove_account_is_not_found_for_an_unknown_login() {
        let engine = MockEngine::new();
        engine.add_account("alice").unwrap();
        assert!(engine.remove_account("alice").is_ok());
        assert!(engine.list_accounts().is_empty());
        assert_eq!(engine.remove_account("alice").unwrap_err().kind, gitmon::ErrorKind::NotFound);
    }

    /// The packet's required coverage: `add_repo` with an explicit login
    /// files the repo under that account regardless of how many others are
    /// signed in, and rejects a login nobody is signed in as.
    #[test]
    fn add_repo_with_an_explicit_login_files_it_under_that_account() {
        let engine = MockEngine::new();
        engine.add_account("alice").unwrap();
        engine.add_account("bob").unwrap();

        // With two accounts signed in, no login at all is ambiguous.
        let ambiguous = engine.add_repo("acme/one", None).unwrap_err();
        assert_eq!(ambiguous.kind, gitmon::ErrorKind::Invalid);

        let repo = engine.add_repo("acme/one", Some("bob")).unwrap();
        assert_eq!(repo.account_login, "bob");

        // A login nobody is signed in as is rejected, not silently unclaimed.
        let unknown = engine.add_repo("acme/two", Some("carol")).unwrap_err();
        assert_eq!(unknown.kind, gitmon::ErrorKind::Invalid);
    }

    /// The demo rate limit: the first poll reports one `rate_limited` repo
    /// error carrying a real `reset_at`, so the app's banner can be seen (and
    /// its "resumes at HH:MM" wording exercised) under `GITMON_MOCK=1`. Every
    /// later poll is clean, which is what takes the banner back down.
    #[test]
    fn the_first_mock_poll_reports_a_rate_limit_with_a_reset_time() {
        let engine = MockEngine::new();
        let before = now();

        let first = engine.poll_now();
        assert_eq!(first.errors.len(), 1, "errors: {:?}", first.errors);
        let error = &first.errors[0];
        // The prefix `poller::rate_limit_notice` matches on to raise the banner.
        assert!(
            error.message.starts_with("rate_limited: "),
            "the banner only reacts to a rate_limited message, got {:?}",
            error.message
        );
        let reset_at = error.reset_at.expect("a reset time the banner can name");
        assert!(
            reset_at >= before + MOCK_RATE_LIMIT_SECS,
            "reset_at must be in the future: {reset_at} vs {before}"
        );

        // The banner comes down again: no later poll repeats the limit.
        for _ in 0..3 {
            assert!(engine.poll_now().errors.is_empty(), "only the first poll is limited");
        }
    }

    /// The status bar's meter needs a reset time and a limit on the *clean*
    /// path too, not only on the rate-limited repo's own `RepoError` — see
    /// the test above for that one. A poll with no error still owes
    /// `PollResult.rate_limit_reset_at`/`rate_limit_limit` a reading, or the
    /// mock would show a meter the real engine never would.
    #[test]
    fn a_clean_poll_still_reports_a_reset_time_and_limit() {
        let engine = MockEngine::new();
        let before = now();
        engine.poll_now(); // consumes RATE_LIMITED_POLL's one scripted error
        let clean = engine.poll_now();
        assert!(clean.errors.is_empty(), "the second poll is the clean one: {:?}", clean.errors);
        assert!(clean.rate_limit_remaining.is_some());
        assert!(clean.rate_limit_limit.is_some());
        let reset_at = clean.rate_limit_reset_at.expect("a reset time for the status bar meter");
        assert!(reset_at > before, "reset_at must be in the future: {reset_at} vs {before}");
        assert!(
            reset_at <= before + 3600,
            "GitHub refills hourly, so the mock's reset should be within the hour: {reset_at} vs {before}"
        );
    }

    /// The mock has to obey the same three rules the real engine does, or the
    /// feed built against `GITMON_MOCK=1` would behave differently from the
    /// shipped one: each step lands strictly inside `[from, to)`, its events
    /// arrive already seen, and the poll watermark does not move.
    #[test]
    fn a_mock_backfill_steps_down_seen_and_leaves_the_watermark_alone() {
        let engine = MockEngine::new();
        let repo = engine.list_repos()[0].clone();
        let existing: std::collections::HashSet<u64> = engine
            .list_events(Some(repo.id), None, None, FilterMode::All, false, None, 500)
            .iter()
            .map(|e| e.id)
            .collect();
        let before = existing.len();
        let floor = repo.backfilled_to.expect("a seeded repo has a floor");

        let result = engine.backfill(Some(repo.id), 7 * 24 * 3600).unwrap();

        assert_eq!(result.repos.len(), 1, "only the named repo: {result:?}");
        let row = &result.repos[0];
        assert_eq!((row.repo_id, row.to), (repo.id, floor));
        assert_eq!(row.from, floor - 7 * 24 * 3600);
        assert!(row.inserted > 0 && row.error.is_none(), "row: {row:?}");

        let after = engine.list_events(Some(repo.id), None, None, FilterMode::All, false, None, 500);
        assert_eq!(after.len(), before + row.inserted as usize);
        // Only the rows this call added — the seed set already reaches below
        // the floor, and those were never claimed to be backfilled.
        for event in after.iter().filter(|e| !existing.contains(&e.id)) {
            assert!(event.seen, "backfilled history must arrive seen: {event:?}");
            assert!(
                event.occurred_at >= row.from && event.occurred_at < row.to,
                "event outside the window it was fabricated for: {event:?}"
            );
        }

        let refreshed = engine.list_repos().into_iter().find(|r| r.id == repo.id).unwrap();
        assert_eq!(refreshed.backfilled_to, Some(row.from), "the floor did not step down");
        assert_eq!(refreshed.last_polled_at, repo.last_polled_at, "the watermark moved");

        // A second step continues from where the first stopped, with no gap.
        let second = engine.backfill(Some(repo.id), 7 * 24 * 3600).unwrap();
        assert_eq!(second.repos[0].to, row.from, "the two steps left a gap");
    }

    /// A repo added but never polled has no floor, and the mock refuses it the
    /// same way the real engine does rather than inventing one.
    #[test]
    fn a_mock_backfill_refuses_a_repo_with_no_floor() {
        let engine = MockEngine::new();
        let repo = engine.add_repo("acme/fresh", None).unwrap();
        // `add_repo` simulates a first poll, so take the floor away again to
        // reach the state a repo added mid-session between polls would be in.
        engine.state.lock().unwrap().repos.iter_mut().find(|r| r.id == repo.id).unwrap()
            .backfilled_to = None;

        let result = engine.backfill(Some(repo.id), 7 * 24 * 3600).unwrap();

        let row = &result.repos[0];
        assert_eq!(row.error.as_deref(), Some("not polled yet"));
        assert_eq!((row.from, row.to, row.inserted), (0, 0, 0));

        let unknown = engine.backfill(Some(9_999), 3600).unwrap_err();
        assert_eq!(unknown.kind, gitmon::ErrorKind::NotFound);
    }

    /// The packet's seed requirement: two teams whose members overlap by
    /// exactly one login, so the switcher, People grouping and
    /// suggest_people exclusion all have a real overlap case to demo.
    #[test]
    fn seeded_teams_overlap_by_exactly_one_login() {
        let engine = MockEngine::new();
        let teams = engine.list_teams();
        assert_eq!(teams.len(), 2, "expected two seeded teams, got {teams:?}");
        let a: std::collections::HashSet<&String> = teams[0].logins.iter().collect();
        let b: std::collections::HashSet<&String> = teams[1].logins.iter().collect();
        let overlap: Vec<_> = a.intersection(&b).collect();
        assert_eq!(overlap.len(), 1, "expected exactly one shared login, got {overlap:?}");
    }

    /// `list_events(team_id)` keeps only events whose actor is *currently* in
    /// that team, and `Event.team_ids` is resolved from current membership —
    /// this is the engine-level half of "switch the chips and confirm the
    /// feed changes" from the packet's manual acceptance step.
    #[test]
    fn list_events_team_id_filters_by_current_membership() {
        let engine = MockEngine::new();
        let teams = engine.list_teams();
        let (platform, infra) = (&teams[0], &teams[1]);
        let shared_login = platform
            .logins
            .iter()
            .find(|l| infra.logins.contains(l))
            .expect("seed teams overlap by one login")
            .clone();

        let platform_events = engine.list_events(None, None, Some(platform.id), FilterMode::All, false, None, 500);
        assert!(!platform_events.is_empty(), "the platform team should have some seeded events");
        for e in &platform_events {
            assert!(
                platform.logins.contains(&e.actor_login),
                "list_events(team_id) returned an event from {} who is not on the team",
                e.actor_login
            );
            assert!(e.team_ids.contains(&platform.id));
        }

        // The shared login's events carry both team ids, in both filtered
        // views — membership, not team_id, decides what's returned.
        let infra_events = engine.list_events(None, None, Some(infra.id), FilterMode::All, false, None, 500);
        for e in platform_events.iter().chain(infra_events.iter()) {
            if e.actor_login == shared_login {
                assert!(e.team_ids.contains(&platform.id) && e.team_ids.contains(&infra.id));
            }
        }
    }

    /// The other half of the same acceptance step: deleting a team drops its
    /// (non-shared) members' events out of that team's filtered feed
    /// immediately, since membership is resolved at read time, not stored.
    #[test]
    fn deleting_a_team_drops_its_members_from_the_team_scoped_feed() {
        let engine = MockEngine::new();
        let teams = engine.list_teams();
        let infra_id = teams[1].id;
        let infra_only_login = teams[1]
            .logins
            .iter()
            .find(|l| !teams[0].logins.contains(l))
            .expect("infra has a non-shared member")
            .clone();

        engine.delete_team(infra_id).expect("delete the team");
        assert!(engine.list_teams().iter().all(|t| t.id != infra_id));

        // The deleted team's id no longer exists, so filtering by it now
        // matches nothing at all — exactly "its members drop out of the feed
        // in team mode".
        let events = engine.list_events(None, None, Some(infra_id), FilterMode::All, false, None, 500);
        assert!(events.is_empty(), "a deleted team's id should filter out everything");

        // And that member's own events no longer carry the deleted team's id.
        let all = engine.list_events(None, Some(&infra_only_login), None, FilterMode::All, false, None, 500);
        assert!(!all.is_empty(), "the login should still have events on the record");
        assert!(all.iter().all(|e| !e.team_ids.contains(&infra_id)));
    }

    /// `reorder_teams` accepts a permutation of every team id and rejects
    /// anything else; `delete_team` on an unknown id is `not_found`.
    #[test]
    fn reorder_teams_validates_the_permutation_and_delete_team_validates_the_id() {
        let engine = MockEngine::new();
        let ids: Vec<u64> = engine.list_teams().iter().map(|t| t.id).collect();
        let reversed: Vec<u64> = ids.iter().rev().copied().collect();

        engine.reorder_teams(&reversed).expect("a full permutation is valid");
        assert_eq!(engine.list_teams().iter().map(|t| t.id).collect::<Vec<_>>(), reversed);

        assert!(engine.reorder_teams(&[ids[0]]).is_err(), "a partial list must be rejected");
        assert!(engine.delete_team(999).is_err(), "an unknown team id must be not_found");
    }

    /// `end <= start` is `invalid` per CONTRACT.md's "Digests", whether
    /// `end` equals or precedes `start`.
    #[test]
    fn digest_rejects_a_window_that_is_not_after_start() {
        let engine = MockEngine::new();
        let t = now();
        assert!(engine.digest(t, t, None, None, 0).is_err());
        assert!(engine.digest(t, t - 10, None, None, 0).is_err());
    }

    /// A window covering every seeded event's totals must match a tally
    /// computed independently from `seed_events()`, per kind, in
    /// `EventKind::ALL` order — the same computation the app reads
    /// `KindCounts` in.
    #[test]
    fn digest_totals_match_every_seeded_event_over_a_wide_window() {
        let engine = MockEngine::new();
        let start = now() - 200 * 3600;
        let end = now() + 3600;
        let digest = engine.digest(start, end, None, None, 0).expect("a valid window");
        assert_eq!(digest.start, start);
        assert_eq!(digest.end, end);
        assert_eq!(digest.team_id, None);
        assert_eq!(digest.actor, None);

        let mut expected: HashMap<EventKind, u64> = HashMap::new();
        for s in seed_events() {
            *expected.entry(s.kind).or_insert(0) += 1;
        }
        for kind in EventKind::ALL {
            assert_eq!(
                digest.totals.get(kind),
                *expected.get(&kind).unwrap_or(&0),
                "{kind:?} total mismatch"
            );
        }
    }

    /// The half-open window `[start, end)`: a range that only spans the
    /// single most-recent seed event (`hours_ago: 1`, on acme/infra) must
    /// count exactly that one event, nothing older and nothing at `end`
    /// itself.
    #[test]
    fn digest_window_is_half_open() {
        let engine = MockEngine::new();
        let base = now();
        let digest = engine.digest(base - 2 * 3600, base, None, None, 0).expect("valid window");
        let total: u64 = EventKind::ALL.iter().map(|k| digest.totals.get(*k)).sum();
        assert_eq!(total, 1, "expected exactly one event in a 2h window ending now, got {digest:?}");
    }

    /// A window entirely before the oldest seeded event yields a zeroed,
    /// empty digest rather than an error — the "Nothing recorded" case the
    /// Summary view renders.
    #[test]
    fn digest_is_empty_for_a_window_with_no_seeded_events() {
        let engine = MockEngine::new();
        let base = now();
        let digest = engine.digest(base - 1000 * 3600, base - 500 * 3600, None, None, 0).expect("valid window");
        assert!(EventKind::ALL.iter().all(|k| digest.totals.get(*k) == 0));
        assert!(digest.people.is_empty());
        assert!(digest.threads.is_empty());
        assert!(digest.repos.is_empty());
    }

    /// `team_id` keeps only events whose actor is currently on that team;
    /// an unknown `team_id` matches nobody (a quiet empty digest, per the
    /// real engine's doc on `digest`), not an error.
    #[test]
    fn digest_team_id_scopes_to_current_membership() {
        let engine = MockEngine::new();
        let platform = engine.list_teams().into_iter().find(|t| t.name == "Platform").unwrap();
        let start = now() - 200 * 3600;
        let end = now() + 3600;

        let scoped = engine.digest(start, end, Some(platform.id), None, 0).expect("valid window");
        assert!(!scoped.people.is_empty(), "the platform team has seeded activity");
        for p in &scoped.people {
            assert!(
                platform.logins.iter().any(|l| l.eq_ignore_ascii_case(&p.login)),
                "{} is not on the Platform team",
                p.login
            );
        }

        let unknown = engine.digest(start, end, Some(999_999), None, 0).expect("valid window");
        assert!(EventKind::ALL.iter().all(|k| unknown.totals.get(*k) == 0));
        assert!(unknown.people.is_empty());
    }

    /// `actor` keeps one login, matched case-insensitively (the store's
    /// `COLLATE NOCASE` in the real engine).
    #[test]
    fn digest_actor_scopes_to_one_login_case_insensitively() {
        let engine = MockEngine::new();
        let start = now() - 200 * 3600;
        let end = now() + 3600;
        let digest = engine.digest(start, end, None, Some("PRIYA"), 0).expect("valid window");
        assert_eq!(digest.actor.as_deref(), Some("PRIYA"), "actor is echoed as given, not normalised");
        assert_eq!(digest.people.len(), 1, "exactly one person's activity: {:?}", digest.people);
        assert_eq!(digest.people[0].login, "priya");

        let expected_total = seed_events().into_iter().filter(|s| s.actor == "priya").count() as u64;
        assert_eq!(digest.people[0].total, expected_total);
    }

    /// `people` is ordered by `total` descending, then login ascending —
    /// checked against a tally computed independently of the mock's own
    /// aggregation, over every seeded actor.
    #[test]
    fn digest_people_are_ranked_by_total_desc_then_login_asc() {
        let engine = MockEngine::new();
        let start = now() - 200 * 3600;
        let end = now() + 3600;
        let digest = engine.digest(start, end, None, None, 0).expect("valid window");

        let mut expected: HashMap<&str, u64> = HashMap::new();
        for s in seed_events() {
            *expected.entry(s.actor).or_insert(0) += 1;
        }
        let mut expected_order: Vec<(&str, u64)> = expected.into_iter().collect();
        expected_order.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));

        let actual_order: Vec<(String, u64)> =
            digest.people.iter().map(|p| (p.login.clone(), p.total)).collect();
        assert_eq!(
            actual_order,
            expected_order.into_iter().map(|(l, t)| (l.to_string(), t)).collect::<Vec<_>>()
        );
    }

    /// `threads` is ordered by `events` desc then `last_at` desc, and a
    /// thread whose members are all issue-kind events carries
    /// `kind: "issue"` while one whose members are pull-kind events carries
    /// `kind: "pull"`; a commit (no `number`) never becomes a thread.
    #[test]
    fn digest_threads_rank_and_classify_kind() {
        let engine = MockEngine::new();
        let start = now() - 200 * 3600;
        let end = now() + 3600;
        let digest = engine.digest(start, end, None, None, 0).expect("valid window");

        // Sorted descending by events, ties broken by last_at descending.
        for pair in digest.threads.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            assert!(
                a.events > b.events || (a.events == b.events && a.last_at >= b.last_at),
                "threads not ranked correctly: {a:?} before {b:?}"
            );
        }

        // #415 (acme/platform) is issue_opened + issue_commented only.
        let issue_415 = digest.threads.iter().find(|t| t.number == 415);
        if let Some(t) = issue_415 {
            assert_eq!(t.kind, ThreadKind::Issue);
        }
        // #412 (acme/platform) mixes pr_opened/reviewed/commented/merged.
        let pull_412 = digest.threads.iter().find(|t| t.number == 412);
        if let Some(t) = pull_412 {
            assert_eq!(t.kind, ThreadKind::Pull);
        }
    }

    /// Every event belongs to exactly one repo, so `repos` totals over a
    /// window must sum to that window's grand total across every kind, and
    /// `repos` is ordered by `total` descending.
    #[test]
    fn digest_repos_sum_to_the_grand_total_and_rank_desc() {
        let engine = MockEngine::new();
        let start = now() - 200 * 3600;
        let end = now() + 3600;
        let digest = engine.digest(start, end, None, None, 0).expect("valid window");

        let grand_total: u64 = EventKind::ALL.iter().map(|k| digest.totals.get(*k)).sum();
        let repos_total: u64 = digest.repos.iter().map(|r| r.total).sum();
        assert_eq!(repos_total, grand_total);

        for pair in digest.repos.windows(2) {
            assert!(pair[0].total >= pair[1].total, "repos not ranked by total descending");
        }
    }

    /// The packet's seed requirement: two watches, one an open PR the mock
    /// login authored, one a merged PR watched by hand.
    #[test]
    fn two_watches_are_seeded_one_open_authored_one_merged_manual() {
        let engine = MockEngine::new();
        let watches = engine.list_watches();
        assert_eq!(watches.len(), 2, "expected two seeded watches, got {watches:?}");
        assert!(
            watches.iter().any(|w| w.state == "open" && w.source == WatchSource::Author),
            "expected an open, author-watched PR: {watches:?}"
        );
        assert!(
            watches.iter().any(|w| w.state == "merged" && w.source == WatchSource::Manual),
            "expected a merged, manually-watched PR: {watches:?}"
        );
    }

    /// `watch_thread` is idempotent (CONTRACT.md "Watched threads"): watching
    /// an already-watched thread returns the existing row unchanged, not a
    /// second one.
    #[test]
    fn watch_thread_is_idempotent() {
        let engine = MockEngine::new();
        let before = engine.list_watches().len();
        let first = engine.watch_thread(1, 416).expect("watch a new thread");
        assert_eq!(engine.list_watches().len(), before + 1);
        let second = engine.watch_thread(1, 416).expect("watch again");
        assert_eq!(first, second, "re-watching must return the same row unchanged");
        assert_eq!(engine.list_watches().len(), before + 1, "must not duplicate the row");
    }

    /// `unwatch_thread` removes the row and un-marks `Event.watched` at read
    /// time (CONTRACT.md "Watched threads": "Read time"); unwatching again is
    /// `not_found`.
    #[test]
    fn unwatch_thread_removes_the_row_and_unwatch_again_is_not_found() {
        let engine = MockEngine::new();
        engine.watch_thread(1, 416).expect("watch");
        let watched = engine.list_events(Some(1), None, None, FilterMode::All, false, None, 500);
        assert!(
            watched.iter().any(|e| e.number == Some(416) && e.watched),
            "events on a freshly watched thread must read back as watched"
        );

        engine.unwatch_thread(1, 416).expect("unwatch");
        assert!(engine.list_watches().iter().all(|w| w.number != 416));
        let after = engine.list_events(Some(1), None, None, FilterMode::All, false, None, 500);
        assert!(
            after.iter().all(|e| e.number != Some(416) || !e.watched),
            "unwatching must un-mark the thread's events immediately"
        );

        assert!(engine.unwatch_thread(1, 416).is_err(), "unwatching an unwatched thread is not_found");
    }

    /// `list_events(watched_only = true)` keeps only events on currently
    /// watched threads — the seeded #425 (open, author-watched) and #96
    /// (merged, manually watched), nothing else.
    #[test]
    fn list_events_watched_only_keeps_only_watched_threads() {
        let engine = MockEngine::new();
        let events = engine.list_events(None, None, None, FilterMode::All, true, None, 500);
        assert!(!events.is_empty(), "the seeded watches should surface some events");
        for e in &events {
            assert!(e.watched, "watched_only returned an unwatched event: {e:?}");
            assert!(
                matches!(e.number, Some(425) | Some(96)),
                "watched_only returned an event on an unwatched thread: {e:?}"
            );
        }
    }

    /// `clear_closed_watches` drops every non-open watch (the seeded merged
    /// #96) and leaves the open one (#425) alone, returning the count removed.
    #[test]
    fn clear_closed_watches_drops_non_open_watches_only() {
        let engine = MockEngine::new();
        let removed = engine.clear_closed_watches();
        assert_eq!(removed, 1, "exactly the one seeded merged watch should be cleared");
        let remaining = engine.list_watches();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].state, "open");
    }

    /// `get_pull_files` (CONTRACT.md "Reading in-app"): the packet's mock
    /// seed calls for five files with exactly one `patch: null`.
    #[test]
    fn get_pull_files_returns_five_files_one_too_large_to_show() {
        let engine = MockEngine::new();
        let files = engine.get_pull_files(1, 425).expect("a watched, seeded PR");
        assert_eq!(files.len(), 5, "expected five files, got {files:?}");
        assert_eq!(
            files.iter().filter(|f| f.patch.is_none()).count(),
            1,
            "expected exactly one file too large to show: {files:?}"
        );
        assert!(engine.get_pull_files(1, 999_999).is_err(), "an unknown thread is not_found");
    }

    /// `get_thread` on the seeded PR carries three synthetic commit items
    /// with a `sha`, and each one's sha resolves back through `get_commit`
    /// to the same commit — the packet's "a PR thread with three commit
    /// items carrying sha" wired end to end.
    #[test]
    fn pr_thread_commits_round_trip_through_get_commit() {
        let engine = MockEngine::new();
        let thread = engine.get_thread(1, 425).expect("the seeded PR");
        let commit_items: Vec<_> = thread.items.iter().filter(|i| i.kind == ThreadItemKind::Commit).collect();
        assert_eq!(commit_items.len(), 3, "expected three commit items: {commit_items:?}");
        for item in commit_items {
            let sha = item.sha.as_deref().expect("a commit item must carry a sha");
            let detail = engine.get_commit(1, sha).expect("get_commit must resolve the synthetic sha");
            assert_eq!(detail.sha, sha);
            assert_eq!(Some(detail.message.split("\n\n").next().unwrap().to_string()), item.body);
        }
    }

    /// The packet's seed requirement for Summary v2: at least six open PRs,
    /// spread over at least three authors, covering draft, approved, changes
    /// requested and review-required, and aged between an hour and nine days —
    /// so the app has a real spread to render under `GITMON_MOCK=1`.
    #[test]
    fn the_seeded_open_pulls_cover_every_state_the_summary_renders() {
        let engine = MockEngine::new();
        let pulls = engine.open_pulls(None, None, None).expect("open pulls");
        assert!(pulls.len() >= 6, "expected at least six open PRs, got {}", pulls.len());

        let authors: std::collections::HashSet<&str> =
            pulls.iter().map(|p| p.author_login.as_str()).collect();
        assert!(authors.len() >= 3, "expected at least three authors: {authors:?}");

        assert!(pulls.iter().any(|p| p.draft), "no draft PR");
        for decision in [REVIEW_APPROVED, REVIEW_CHANGES_REQUESTED, REVIEW_REQUIRED] {
            assert!(
                pulls.iter().any(|p| p.review_decision.as_deref() == Some(decision)),
                "no open PR is {decision}"
            );
        }
        assert!(
            pulls.iter().any(|p| p.review_decision.is_none()),
            "no open PR has an undecided review"
        );

        let now = now();
        let ages: Vec<i64> = pulls.iter().map(|p| now - p.created_at).collect();
        let youngest = *ages.iter().min().expect("at least one PR");
        let oldest = *ages.iter().max().expect("at least one PR");
        assert!(youngest <= 2 * 3600, "the youngest PR should be about an hour old: {youngest}s");
        assert!(oldest >= 8 * 24 * 3600, "the oldest PR should be over a week old: {oldest}s");

        // Oldest first, and every reviewer request is a real login.
        for pair in pulls.windows(2) {
            assert!(pair[0].created_at <= pair[1].created_at, "not oldest first: {pulls:?}");
        }
    }

    /// `open_pulls` narrows by the author's team, by one author and by who is
    /// being asked to review, the three intersecting — the same rule the real
    /// engine follows.
    #[test]
    fn mock_open_pulls_scope_by_team_author_and_reviewer() {
        let engine = MockEngine::new();
        let all = engine.open_pulls(None, None, None).unwrap();

        let priyas = engine.open_pulls(None, Some("PRIYA"), None).unwrap();
        assert!(!priyas.is_empty(), "priya has open PRs in the seed");
        assert!(priyas.iter().all(|p| p.author_login == "priya"));
        assert!(priyas.len() < all.len(), "the author filter narrowed nothing");

        // Team 2 is Infra: lena, aiko, tom.
        let infra = engine.open_pulls(Some(2), None, None).unwrap();
        assert!(!infra.is_empty());
        assert!(infra
            .iter()
            .all(|p| ["lena", "aiko", "tom"].contains(&p.author_login.as_str())));

        let for_lena = engine.open_pulls(None, None, Some("LENA")).unwrap();
        assert!(!for_lena.is_empty(), "lena is asked to review in the seed");
        assert!(for_lena
            .iter()
            .all(|p| p.requested_reviewers.iter().any(|r| r.eq_ignore_ascii_case("lena"))));

        // An unknown team matches nobody, quietly.
        assert!(engine.open_pulls(Some(999_999), None, None).unwrap().is_empty());
    }

    /// The digest's Summary-v2 halves: a gapless local-day series that adds up
    /// to the window, an always-7x24 heatmap that does too, `people_total`
    /// counted before the top-20 cut, and the per-person PR load read off the
    /// open pulls rather than off the window.
    #[test]
    fn mock_digest_carries_the_series_heatmap_and_pull_load() {
        let engine = MockEngine::new();
        let start = now() - 200 * 3600;
        let end = now() + 3600;
        let digest = engine.digest(start, end, None, None, 0).expect("valid window");

        // One entry per local day the window touches, in order and 86 400 apart.
        assert!(!digest.series.is_empty());
        for pair in digest.series.windows(2) {
            assert_eq!(pair[1].day - pair[0].day, 86_400, "the series has a gap in it");
        }
        assert_eq!(
            digest.series.iter().map(|d| d.counts.total()).sum::<u64>(),
            digest.totals.total(),
            "the series must add up to the window"
        );

        assert_eq!(digest.hours.len(), 7);
        assert!(digest.hours.iter().all(|row| row.len() == 24));
        assert_eq!(
            digest.hours.iter().flatten().sum::<u64>(),
            digest.totals.total(),
            "the heatmap must add up to the window"
        );

        assert_eq!(
            digest.people_total,
            digest.people.len() as u64,
            "the seed has fewer than twenty people, so the two agree"
        );
        for person in &digest.people {
            assert!(person.last_at >= start && person.last_at < end);
        }

        // The load matches `open_pulls` exactly, because both read the same rows.
        let open = engine.open_pulls(None, None, None).unwrap();
        for person in &digest.people {
            let authored = open
                .iter()
                .filter(|p| p.author_login.eq_ignore_ascii_case(&person.login))
                .count() as u64;
            let queued = open
                .iter()
                .filter(|p| {
                    p.requested_reviewers.iter().any(|r| r.eq_ignore_ascii_case(&person.login))
                })
                .count() as u64;
            assert_eq!(person.open_prs, authored, "{} open_prs", person.login);
            assert_eq!(person.review_queue, queued, "{} review_queue", person.login);
        }

        // The offset moves which day an event lands on, and nothing else.
        let shifted = engine.digest(start, end, None, None, 6 * 3600).expect("valid window");
        assert_eq!(shifted.totals, digest.totals);
        assert_eq!(
            shifted.series.iter().map(|d| d.counts.total()).sum::<u64>(),
            shifted.totals.total()
        );
        assert_ne!(shifted.series[0].day, digest.series[0].day);
    }

    /// The mock's PR timings are real medians over the seeded threads, and a
    /// window in which nothing merged or was reviewed reports nulls rather
    /// than zeros.
    #[test]
    fn mock_digest_pr_timings_are_medians_and_null_when_empty() {
        let engine = MockEngine::new();
        let wide = engine
            .digest(now() - 200 * 3600, now() + 3600, None, None, 0)
            .expect("valid window");
        assert!(wide.pr_timing.samples > 0, "the seed merges and reviews plenty of PRs");
        let ttm = wide.pr_timing.median_ttm_secs.expect("something merged");
        assert!(ttm > 0, "a PR cannot merge before it is opened");
        assert!(
            wide.pr_timing.median_ttfr_secs.is_some(),
            "the seed reviews PRs it also opened"
        );

        // Far enough in the past that the seed has nothing there at all.
        let quiet = engine
            .digest(now() - 1000 * 3600, now() - 900 * 3600, None, None, 0)
            .expect("valid window");
        assert_eq!(quiet.pr_timing.median_ttm_secs, None);
        assert_eq!(quiet.pr_timing.median_ttfr_secs, None);
        assert_eq!(quiet.pr_timing.p90_ttm_secs, None);
        assert_eq!(quiet.pr_timing.p90_ttfr_secs, None);
        assert_eq!(quiet.pr_timing.samples, 0);
    }

    /// The Summary-v3 trio the app renders under `GITMON_MOCK=1`: a p90 that
    /// is visibly slower than the median rather than equal to it, a non-zero
    /// count of merges nobody reviewed, and a repo one person plainly owns.
    ///
    /// The seed has to *earn* all three, so this test is what keeps a later
    /// edit to `seed_events` from quietly flattening the demo.
    #[test]
    fn mock_digest_carries_the_summary_v3_signals() {
        let engine = MockEngine::new();
        let wide = engine
            .digest(now() - 200 * 3600, now() + 3600, None, None, 0)
            .expect("valid window");

        let median = wide.pr_timing.median_ttm_secs.expect("something merged");
        let p90 = wide.pr_timing.p90_ttm_secs.expect("the same samples have a p90");
        assert!(
            p90 > median,
            "the seed's slow tail should show: p90 {p90} vs median {median}"
        );
        assert!(
            wide.pr_timing.p90_ttfr_secs.expect("reviews too") >= wide
                .pr_timing
                .median_ttfr_secs
                .expect("reviews too"),
            "a p90 can never sit below its own median"
        );

        assert!(
            wide.unreviewed_merges > 0,
            "the seed merges dependency bumps nobody reviewed"
        );
        assert!(
            wide.unreviewed_merges < wide.totals.get(EventKind::PrMerged),
            "not every merge is unreviewed — the seed reviews most of them"
        );

        let concentrated = wide
            .repos
            .iter()
            .max_by(|a, b| a.top_author_share.partial_cmp(&b.top_author_share).unwrap())
            .expect("the seed has repos");
        assert!(
            concentrated.top_author_share > 0.8,
            "one seeded repo should read as one person's: {:?} at {}",
            concentrated.top_author_login,
            concentrated.top_author_share
        );
        assert!(concentrated.top_author_login.is_some(), "a share needs an author");
        // Every repo's share is a fraction, and a repo with no commits in the
        // window says so rather than naming a reviewer.
        for repo in &wide.repos {
            assert!((0.0..=1.0).contains(&repo.top_author_share), "{repo:?}");
            assert_eq!(
                repo.top_author_login.is_none(),
                repo.top_author_share == 0.0,
                "{repo:?} disagrees with itself about having commits"
            );
        }
    }

    /// Hiding takes a repo out of every read the mock serves, and unhiding
    /// puts every one of its rows back — the same claim the real engine's
    /// `tests/hidden.rs` makes, so the mock-driven UI behaves like the app.
    #[test]
    fn hiding_a_repo_empties_every_read_and_unhiding_refills_them() {
        let engine = MockEngine::new();
        let before_events = engine.list_events(None, None, None, FilterMode::All, false, None, 500);
        let before_pulls = engine.open_pulls(None, None, None).unwrap();
        let before_unseen = engine.unseen_count();
        let hidden = engine.list_repos()[0].id;
        assert!(before_events.iter().any(|e| e.repo_id == hidden), "the seed fills every repo");

        engine.set_repo_hidden(hidden, true).unwrap();

        assert!(engine.list_repos().iter().all(|r| r.id != hidden));
        assert_eq!(engine.list_hidden_repos().len(), 1);
        let during = engine.list_events(None, None, None, FilterMode::All, false, None, 500);
        assert!(during.iter().all(|e| e.repo_id != hidden), "a hidden repo is still in the feed");
        assert!(during.len() < before_events.len());
        assert!(engine.open_pulls(None, None, None).unwrap().iter().all(|p| p.repo_id != hidden));
        assert!(engine.list_watches().iter().all(|w| w.repo_id != hidden));
        assert!(engine.unseen_count() <= before_unseen);
        // Nor is it polled, nor backfilled.
        let polled = engine.poll_now();
        assert!(polled.new_events.iter().all(|e| e.repo_id != hidden));
        let step = engine.backfill(Some(hidden), gitmon::BACKFILL_DEFAULT_SPAN_SECS).unwrap();
        assert_eq!(step.repos[0].error.as_deref(), Some("hidden"));

        engine.set_repo_hidden(hidden, false).unwrap();

        // Every row it had is back. The poll above added events to the *other*
        // repos, so this checks the hidden repo's own history rather than the
        // whole feed being byte-identical.
        let after = engine.list_events(None, None, None, FilterMode::All, false, None, 500);
        let mine = |events: &[Event]| -> Vec<u64> {
            events.iter().filter(|e| e.repo_id == hidden).map(|e| e.id).collect()
        };
        assert_eq!(mine(&after), mine(&before_events), "unhiding lost stored events");
        assert_eq!(
            engine.open_pulls(None, None, None).unwrap().len(),
            before_pulls.len(),
            "unhiding lost stored pulls"
        );
    }

    /// Removing a repo forgets its open PRs, exactly as it forgets its events.
    #[test]
    fn removing_a_repo_forgets_its_open_pulls() {
        let engine = MockEngine::new();
        let before = engine.open_pulls(None, None, None).unwrap();
        let doomed = before[0].repo_id;
        let survivors = before.iter().filter(|p| p.repo_id != doomed).count();
        assert!(survivors > 0, "the seed spreads open PRs over several repos");

        engine.remove_repo(doomed).unwrap();

        let after = engine.open_pulls(None, None, None).unwrap();
        assert_eq!(after.len(), survivors);
        assert!(after.iter().all(|p| p.repo_id != doomed));
    }
}
