//! Serde types from `docs/CONTRACT.md`.
//!
//! Key names are snake_case and every optional field is serialized as `null`
//! rather than omitted, so the JSON the app sees always has the full shape.

use serde::de::{MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A named, ordered list of GitHub logins. An install has any number of teams
/// and a login may belong to several; teams categorise people, and repos are
/// shared by all of them.
///
/// `id` is 0 only for a team that has not been saved yet — what
/// `import_org_team` hands back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Team {
    pub id: u64,
    pub name: String,
    pub logins: Vec<String>,
}

/// The id given to a team that exists only in memory, never stored.
pub const UNSAVED_TEAM_ID: u64 = 0;

/// A signed-in GitHub login with its own token.
///
/// An install may have several — personal and work, say. The token itself is
/// never here and never in the database: it lives in the app's keychain and, for
/// the life of the process, in the engine's in-memory map keyed by this `login`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub login: String,
    pub avatar_url: Option<String>,
    pub added_at: i64,
}

/// A GitHub repository the app polls, owned by one account.
///
/// `account_login` is the account whose token polls it. An empty string means
/// "not yet claimed": a pre-v5 install's repos start that way and the first
/// `add_account` claims them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repo {
    pub id: u64,
    pub owner: String,
    pub name: String,
    pub url: String,
    pub account_login: String,
    pub default_branch: String,
    pub last_polled_at: Option<i64>,
    /// The oldest instant this repo's stored history is known to cover, as unix
    /// seconds — the bottom of the contiguous run of windows fetched down from
    /// the watermark. `null` until the first successful poll seeds it (and again
    /// after a rescan clears it), which is also the state in which `backfill`
    /// declines to reach further back: there is no floor to step down from yet.
    pub backfilled_to: Option<i64>,
    pub last_error: Option<String>,
    /// True when the user has hidden this repo: it is left out of every read
    /// (`list_events`, the digest, `open_pulls`, the watch list, the unseen
    /// count and the repo filter's own options) and out of every poll, so it
    /// costs no GitHub requests at all while it is hidden.
    ///
    /// Hiding is emphatically *not* removing: the events, pulls and watches
    /// already collected stay in the database untouched, and unhiding brings
    /// every one of them straight back with no refetch. `last_polled_at`
    /// simply stops advancing while hidden — nothing polls the repo, so there
    /// is nothing to advance it — which is why unhiding a long-hidden repo
    /// resets the watermark rather than reaching back across the whole gap;
    /// see `Store::set_repo_hidden`.
    pub hidden: bool,
}

/// The feed's "My team / Everyone" view filter, passed to `list_events` as
/// `mode`.
///
/// `Team` means "in at least one team" — with several teams it is their
/// union, plus a signed-in account's own login regardless of its team — and
/// with no team having a single member yet it behaves like `All`, since an
/// install that has not been configured is not one configured to exclude
/// everybody. `Settings.filter_mode` carries this same type as the value a
/// fresh window's view filter starts from; before packet gm-feed-r1 it was
/// also applied at ingestion (dropping a non-matching actor's event before it
/// was ever stored, permanently — changing the setting did not backfill the
/// dropped history), which is why an install upgrading from that version can
/// have gaps in it that switching this can no longer recover.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterMode {
    All,
    #[default]
    Team,
}

impl FilterMode {
    pub fn as_str(self) -> &'static str {
        match self {
            FilterMode::All => "all",
            FilterMode::Team => "team",
        }
    }
}

/// One kind of thing a person did in a repo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Commit,
    PrOpened,
    PrMerged,
    PrClosed,
    PrReviewed,
    PrCommented,
    IssueOpened,
    IssueCommented,
}

impl EventKind {
    /// Every kind, in contract order. Used as the default for `notify_kinds`.
    pub const ALL: [EventKind; 8] = [
        EventKind::Commit,
        EventKind::PrOpened,
        EventKind::PrMerged,
        EventKind::PrClosed,
        EventKind::PrReviewed,
        EventKind::PrCommented,
        EventKind::IssueOpened,
        EventKind::IssueCommented,
    ];

    /// The wire name, which is also how the kind is stored in SQLite.
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::Commit => "commit",
            EventKind::PrOpened => "pr_opened",
            EventKind::PrMerged => "pr_merged",
            EventKind::PrClosed => "pr_closed",
            EventKind::PrReviewed => "pr_reviewed",
            EventKind::PrCommented => "pr_commented",
            EventKind::IssueOpened => "issue_opened",
            EventKind::IssueCommented => "issue_commented",
        }
    }

    /// This kind's position in [`ALL`](Self::ALL) — the index a [`KindCounts`]
    /// slot lives at, and the order the contract fixes for `totals`/`counts`.
    pub fn index(self) -> usize {
        // Small enough that a linear scan beats a match arm per variant, and it
        // cannot drift out of step with `ALL` the way a hand-written one could.
        EventKind::ALL
            .iter()
            .position(|kind| *kind == self)
            .expect("every kind is in EventKind::ALL")
    }

    /// Which kind of thread this event belongs to: `pull` for the `pr_*` kinds,
    /// `issue` for the `issue_*` kinds. A commit belongs to no thread — it has
    /// no `number` — so it has none, and never appears in a digest's `threads`.
    pub fn thread_kind(self) -> Option<ThreadKind> {
        match self {
            EventKind::Commit => None,
            EventKind::PrOpened
            | EventKind::PrMerged
            | EventKind::PrClosed
            | EventKind::PrReviewed
            | EventKind::PrCommented => Some(ThreadKind::Pull),
            EventKind::IssueOpened | EventKind::IssueCommented => Some(ThreadKind::Issue),
        }
    }
}

impl std::str::FromStr for EventKind {
    type Err = crate::error::EngineError;

    fn from_str(s: &str) -> std::result::Result<EventKind, Self::Err> {
        EventKind::ALL
            .into_iter()
            .find(|kind| kind.as_str() == s)
            .ok_or_else(|| crate::error::EngineError::invalid(format!("unknown event kind {s:?}")))
    }
}

/// One stored thing a person did. Never mutated after insert except for `seen`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub id: u64,
    pub repo_id: u64,
    pub kind: EventKind,
    pub actor_login: String,
    pub actor_avatar_url: Option<String>,
    pub title: String,
    pub body_preview: Option<String>,
    /// The full markdown body, untruncated; the app renders it in place.
    pub body: Option<String>,
    pub url: String,
    pub number: Option<u64>,
    pub occurred_at: i64,
    pub seen: bool,
    /// Which teams the actor currently belongs to, in team display order.
    /// Resolved on every read and never stored, so editing a team
    /// re-categorises the feed immediately and never backfills.
    pub team_ids: Vec<u64>,
    /// Whether `(repo_id, number)` is a currently watched thread. Resolved on
    /// every read for the same reason `team_ids` is: unwatching a thread
    /// un-marks the feed at once rather than at the next poll. Always false for
    /// a commit, which has no `number` and so belongs to no thread.
    pub watched: bool,
}

/// A single repo's failure during a poll. Other repos still poll.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoError {
    pub repo_id: u64,
    pub message: String,
    /// When the budget refills, as unix seconds — the `reset_at` the
    /// underlying [`EngineError`](crate::EngineError) carried, so a
    /// poller-side rate limit can say *when* it lifts instead of only that it
    /// happened. `None` for every other error kind, exactly as `EngineError`
    /// itself is: only `rate_limited` ever sets it.
    pub reset_at: Option<i64>,
}

/// The outcome of one `poll_now` across every watched repo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollResult {
    pub new_events: Vec<Event>,
    pub errors: Vec<RepoError>,
    pub rate_limit_remaining: Option<u32>,
    /// When `rate_limit_remaining`'s budget refills, as unix seconds — the
    /// same account's `x-ratelimit-reset`, captured off the same response
    /// (see `github::RateLimitReading`). `None` when that account's most
    /// recent response carried no reset header, or when no account was used.
    pub rate_limit_reset_at: Option<i64>,
    /// The size of that budget — the same account's `x-ratelimit-limit`.
    /// `None` when the response carried no limit header; callers should
    /// treat that as GitHub's documented default (5,000/hour), not as
    /// "unknown", since GitHub omits `-limit` far less reliably than it omits
    /// `-reset`.
    pub rate_limit_limit: Option<u32>,
    /// Repo ids this poll deliberately did not fetch because GitHub's own
    /// `X-Poll-Interval` hint for them had not elapsed yet. Not an error and
    /// not a failure: the repo's watermark and `last_error` are both left
    /// exactly as they were, and the next poll past the interval fetches it.
    /// Always empty for a forced poll.
    pub skipped: Vec<u64>,
}

/// How one `poll_now` behaves. Defaults to the background poller's behaviour,
/// so `PollOptions::default()` is what the timer wants.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PollOptions {
    /// Fetch every repo whether or not its `X-Poll-Interval` has elapsed. What
    /// a hand-driven "Poll now" passes: the user asked, so the answer should
    /// not be "not yet". The background poller leaves it false and honours the
    /// hint.
    #[serde(default)]
    pub force: bool,
}

/// One repo's share of a `backfill`: the window it reached for and what came of
/// it.
///
/// A repo that could not be backfilled still gets a row — with `inserted` 0 and
/// `error` set — so the caller can tell "nothing was there" from "we never
/// looked". `from` and `to` are both 0 for a repo that was never polled, the one
/// case where no window was chosen at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoBackfill {
    pub repo_id: u64,
    /// The window's inclusive lower bound, and the repo's new `backfilled_to`
    /// once the step succeeds.
    pub from: i64,
    /// The window's exclusive upper bound — the `backfilled_to` this step
    /// started from.
    pub to: i64,
    /// Rows this step actually added. Re-running an overlapping window inserts
    /// nothing: the dedupe index turns every already-stored event into a no-op.
    pub inserted: u64,
    /// `null` on a clean step. `"not polled yet"` when the repo has no floor to
    /// step down from, `"signed out"` when no token can authenticate it,
    /// `"window truncated"` when the page cap cut the fetch short (the cursor
    /// still advances), and otherwise the `EngineError` the fetch failed with.
    pub error: Option<String>,
}

/// The outcome of one `backfill` across the repos it was aimed at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackfillResult {
    pub repos: Vec<RepoBackfill>,
    pub rate_limit_remaining: Option<u32>,
    /// Same meaning as `PollResult.rate_limit_reset_at`.
    pub rate_limit_reset_at: Option<i64>,
    /// Same meaning as `PollResult.rate_limit_limit`.
    pub rate_limit_limit: Option<u32>,
}

/// Minutes after local midnight; may wrap past midnight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuietHours {
    pub start_minute: u16,
    pub end_minute: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    pub poll_interval_secs: u32,
    pub filter_mode: FilterMode,
    pub notifications_enabled: bool,
    pub notify_kinds: Vec<EventKind>,
    pub quiet_hours: Option<QuietHours>,
    /// Whether a poll watches the signed-in user's own pull requests and the
    /// ones they were asked to review.
    ///
    /// `#[serde(default)]` with an explicit `true`: settings written before
    /// schema v4 have no `auto_watch` key at all, and the contract's default is
    /// on, so a missing key must read back as `true` rather than as `false`,
    /// which is what a bare `Default` would give.
    #[serde(default = "auto_watch_default")]
    pub auto_watch: bool,
}

/// The contract's default for [`Settings::auto_watch`], named so `serde` can
/// reach it and so the default lives in exactly one place.
fn auto_watch_default() -> bool {
    true
}

/// The contract's floor for `poll_interval_secs`; anything lower is `invalid`.
pub const MIN_POLL_INTERVAL_SECS: u32 = 30;

impl Default for Settings {
    fn default() -> Self {
        Settings {
            poll_interval_secs: 120,
            filter_mode: FilterMode::Team,
            notifications_enabled: true,
            notify_kinds: EventKind::ALL.to_vec(),
            quiet_hours: None,
            auto_watch: auto_watch_default(),
        }
    }
}

/// Where a suggested person came from. Bots sort last.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonSource {
    Contributor,
    OrgMember,
    Bot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonSuggestion {
    pub login: String,
    pub avatar_url: Option<String>,
    pub source: PersonSource,
    pub why: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoSuggestion {
    pub owner: String,
    pub name: String,
    pub why: String,
}

/// The device-flow handshake GitHub hands back from `POST /login/device/code`.
/// The app shows `user_code` and `verification_uri` to the user, then polls with
/// `device_code` every `interval` seconds until `expires_in` runs out.
///
/// `verification_uri_complete` is the same page with the code already filled in;
/// GitHub returns it alongside `verification_uri`, and it is what the app opens
/// in its own window. It is `None` when the response omits it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceLogin {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub expires_in: u32,
    pub interval: u32,
}

/// The result of one poll of a device login.
///
/// Serialises as `{"status":"pending"}` or
/// `{"status":"ok","token":"…","login":"…"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DeviceLoginStatus {
    /// The user has not finished authorising yet. The caller waits and retries.
    Pending,
    Ok { token: String, login: String },
}

/// Whether a thread is a pull request or a plain issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadKind {
    Pull,
    Issue,
}

impl ThreadKind {
    /// The wire name, which is also how the kind is stored in SQLite.
    pub fn as_str(self) -> &'static str {
        match self {
            ThreadKind::Pull => "pull",
            ThreadKind::Issue => "issue",
        }
    }
}

impl std::str::FromStr for ThreadKind {
    type Err = crate::error::EngineError;

    fn from_str(s: &str) -> std::result::Result<ThreadKind, Self::Err> {
        match s {
            "pull" => Ok(ThreadKind::Pull),
            "issue" => Ok(ThreadKind::Issue),
            other => {
                Err(crate::error::EngineError::invalid(format!("unknown thread kind {other:?}")))
            }
        }
    }
}

/// How a thread came to be watched.
///
/// `Manual` is `watch_thread`; the other two are auto-watch during a poll, and
/// they are the only reason the engine needs to remember the signed-in login.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatchSource {
    Manual,
    Author,
    Reviewer,
}

impl WatchSource {
    /// Every source, used by the tests that enumerate the wire names.
    pub const ALL: [WatchSource; 3] =
        [WatchSource::Manual, WatchSource::Author, WatchSource::Reviewer];

    /// The wire name, which is also how the source is stored in SQLite.
    pub fn as_str(self) -> &'static str {
        match self {
            WatchSource::Manual => "manual",
            WatchSource::Author => "author",
            WatchSource::Reviewer => "reviewer",
        }
    }
}

impl std::str::FromStr for WatchSource {
    type Err = crate::error::EngineError;

    fn from_str(s: &str) -> std::result::Result<WatchSource, Self::Err> {
        WatchSource::ALL.into_iter().find(|source| source.as_str() == s).ok_or_else(|| {
            crate::error::EngineError::invalid(format!("unknown watch source {s:?}"))
        })
    }
}

/// A PR or issue the user follows.
///
/// Every event on a watched thread is stored regardless of the ingestion
/// filter, and always notifies. `title` and `state` are refreshed by each poll
/// that sees the thread in a listing, so they are "as last seen" rather than
/// live: `state` is `open`, `closed` or `merged`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Watch {
    pub repo_id: u64,
    pub number: u64,
    pub kind: ThreadKind,
    pub title: String,
    pub state: String,
    pub source: WatchSource,
    pub since: i64,
}

/// The `state` a watch carries while its thread is still open. `clear_closed`
/// unwatches every row that is not this.
pub const WATCH_STATE_OPEN: &str = "open";

/// One PR or issue with its whole conversation, fetched live for in-app reading.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thread {
    pub repo_id: u64,
    pub number: u64,
    pub kind: ThreadKind,
    pub title: String,
    pub state: String,
    pub author_login: String,
    pub author_avatar_url: Option<String>,
    pub body: Option<String>,
    pub url: String,
    pub created_at: i64,
    /// Pull requests only; an issue leaves all five null.
    pub head_ref: Option<String>,
    pub base_ref: Option<String>,
    pub additions: Option<u32>,
    pub deletions: Option<u32>,
    pub changed_files: Option<u32>,
    /// Chronological, oldest first.
    pub items: Vec<ThreadItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadItemKind {
    Comment,
    Review,
    ReviewComment,
    Commit,
}

/// One entry in a thread's conversation. `state` is set on reviews only;
/// `path` and `line` on review comments only; `sha` on commits only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadItem {
    pub kind: ThreadItemKind,
    pub actor_login: String,
    pub actor_avatar_url: Option<String>,
    pub body: Option<String>,
    pub state: Option<String>,
    pub path: Option<String>,
    pub line: Option<u32>,
    pub url: String,
    pub at: i64,
    /// The commit sha, on `commit` items only; null on every other kind. It is
    /// what the app hands to `get_commit` when the reader clicks a commit.
    pub sha: Option<String>,
}

/// One commit with its per-file diff, fetched live for in-app reading.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitDetail {
    pub repo_id: u64,
    pub sha: String,
    pub message: String,
    pub author_login: String,
    pub author_avatar_url: Option<String>,
    pub url: String,
    pub at: i64,
    pub additions: u32,
    pub deletions: u32,
    pub files: Vec<CommitFile>,
}

/// One changed file. `patch` is null for binary and very large files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitFile {
    pub path: String,
    pub status: String,
    pub additions: u32,
    pub deletions: u32,
    pub patch: Option<String>,
}

/// Per-kind counts, serialised as a JSON object keyed by the `EventKind` wire
/// name with **every** kind present — zeros included — in `EventKind::ALL`
/// order, which is the order `docs/CONTRACT.md` lists them in.
///
/// Neither standard map would do: a `BTreeMap` sorts its keys alphabetically
/// (`commit`, `issue_commented`, `issue_opened`, …) and a `HashMap` does not
/// order them at all, while the contract fixes the order. So the counts live in
/// a fixed array indexed by [`EventKind::index`] and the JSON object is written
/// out by hand, one entry per kind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KindCounts([u64; EventKind::ALL.len()]);

impl KindCounts {
    /// How many events of `kind`. Zero when there were none.
    pub fn get(&self, kind: EventKind) -> u64 {
        self.0[kind.index()]
    }

    pub fn set(&mut self, kind: EventKind, count: u64) {
        self.0[kind.index()] = count;
    }

    pub fn add(&mut self, kind: EventKind, count: u64) {
        self.0[kind.index()] += count;
    }

    /// Every kind summed — the `total` beside a `counts` object.
    pub fn total(&self) -> u64 {
        self.0.iter().sum()
    }
}

impl Serialize for KindCounts {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(EventKind::ALL.len()))?;
        for kind in EventKind::ALL {
            map.serialize_entry(kind.as_str(), &self.get(kind))?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for KindCounts {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct KindCountsVisitor;

        impl<'de> Visitor<'de> for KindCountsVisitor {
            type Value = KindCounts;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a map of event kind to count")
            }

            fn visit_map<M: MapAccess<'de>>(
                self,
                mut map: M,
            ) -> std::result::Result<KindCounts, M::Error> {
                let mut counts = KindCounts::default();
                while let Some((key, count)) = map.next_entry::<String, u64>()? {
                    // Strict, like `EventKind::from_str`: a key this build does
                    // not know is a contract mismatch, not something to drop on
                    // the floor and silently under-count.
                    let kind: EventKind = key
                        .parse()
                        .map_err(|_| serde::de::Error::unknown_field(&key, &[]))?;
                    counts.set(kind, count);
                }
                Ok(counts)
            }
        }

        deserializer.deserialize_map(KindCountsVisitor)
    }
}

/// The contract's cap on a digest's `people` list.
pub const DIGEST_PEOPLE_LIMIT: usize = 20;
/// The contract's cap on a digest's `threads` list.
pub const DIGEST_THREAD_LIMIT: usize = 10;
/// The cap on a digest's `series`, in local days — ten years, which no period
/// the app offers comes close to. It exists so an absurd `[start, end)` cannot
/// ask the engine to allocate one entry per day for the age of the universe; a
/// longer window is summarised by its first `DIGEST_MAX_SERIES_DAYS` days.
pub const DIGEST_MAX_SERIES_DAYS: usize = 3660;

/// A summary of the events already stored for the half-open window
/// `[start, end)`, optionally narrowed to one team, one person, or both.
///
/// Computed from the store alone, never from GitHub, so it covers exactly what
/// Vigie has watched: nothing from before a repo was added, and nothing the
/// ingestion filter dropped.
///
/// `PartialEq` but not `Eq`: `repos[].top_author_share` is a float.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Digest {
    pub start: i64,
    pub end: i64,
    /// Echoes the request's scoping, so the app can label the summary without
    /// keeping the arguments it called with.
    pub team_id: Option<u64>,
    pub actor: Option<String>,
    /// Every event in the window, by kind. Counts the whole window, not just
    /// the people who survived the top-20 cut.
    pub totals: KindCounts,
    /// By `total` descending, then login ascending; at most
    /// [`DIGEST_PEOPLE_LIMIT`].
    pub people: Vec<DigestPerson>,
    /// How many distinct people were active in the window, before the top-20
    /// cut — so the app can say "20 of 34" rather than implying there were 20.
    pub people_total: u64,
    /// By `events` descending, then `last_at` descending; at most
    /// [`DIGEST_THREAD_LIMIT`].
    pub threads: Vec<DigestThread>,
    /// Every watched repo with a non-zero total, by `total` descending.
    pub repos: Vec<DigestRepo>,
    /// One entry per **local** day the window touches, oldest first, with the
    /// quiet days present and zeroed so the app can plot a gapless series.
    pub series: Vec<DayCounts>,
    /// A 7 x 24 grid of event counts: `hours[weekday][hour]`, weekday Monday =
    /// 0, hour in local time. Always exactly seven rows of twenty-four.
    pub hours: Vec<Vec<u64>>,
    /// How long pull requests took, over the same window.
    pub pr_timing: PrTiming,
    /// How many pull requests merged inside the window carry no stored
    /// `pr_reviewed` event at all, at any time up to their merge — the
    /// "shipped without a review" count.
    ///
    /// Scoped by the PR's **author** (the actor on its `pr_opened`), exactly as
    /// [`PrTiming`] is, so a team's number is about that team's pull requests
    /// however they were merged. An unscoped digest counts every merge in the
    /// window; a scoped one can only count merges whose `pr_opened` is stored,
    /// because nothing else names the author.
    ///
    /// **It sees only what the store holds.** A repo added after a PR was
    /// reviewed has no `pr_reviewed` event for it, so that PR's later merge
    /// reads as unreviewed — the same caveat class as
    /// [`OpenPull::review_decision`], and the reason this is a prompt to look
    /// rather than an audit.
    pub unreviewed_merges: u64,
}

/// One local day's events. `day` is the unix second at which that local day
/// began — midnight shifted by the `tz_offset_secs` the caller passed — so the
/// app can label the point without redoing the calendar maths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayCounts {
    pub day: i64,
    pub counts: KindCounts,
}

/// Median pull-request timings over a digest window.
///
/// **Time to merge** (`median_ttm_secs`) runs from a PR's `pr_opened` to its
/// `pr_merged`; **time to first review** (`median_ttfr_secs`) from `pr_opened`
/// to the earliest `pr_reviewed` on the same PR. A PR counts when the *closing*
/// event — the merge, or that first review — falls inside the window, so a
/// window reports what finished in it. Both are `null` when nothing did.
///
/// Each measure also carries a **p90** over the very same sample set, so the
/// Summary page can say what the slow tail costs and not only what the typical
/// PR costs. Both percentiles use one rule — *linear interpolation between the
/// two closest ranks* on the ascending samples, the R-7 definition that numpy,
/// pandas and Excel's `PERCENTILE.INC` share: the value sits at position
/// `p * (n - 1)`, and where that falls between two samples the answer is
/// interpolated between them and rounded up to the whole second.
///
/// Interpolating rather than taking the nearest rank is what makes the number
/// mean something on small sets. Nearest-rank returns the largest sample for
/// every `n` below ten, so a week with three merges would report its slowest
/// merge as its p90 and the number would carry no information the maximum did
/// not already carry. Under R-7: one sample makes p90 that sample, two make it
/// nine tenths of the way from the faster to the slower, ten put it a tenth of
/// the way from the ninth to the tenth.
///
/// `p90 >= median` always, for any sample set: the median is R-7 at `p = 0.5`,
/// the same rule at a lower `p`, and rounding the interpolation *up* keeps the
/// guarantee even where the median's own rounding goes the other way.
///
/// Each p90 is `None` under exactly the condition its median is: no samples.
///
/// `samples` is the number of distinct pull requests that contributed at least
/// one of the two measurements, so a caller can tell "fast" from "one PR".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrTiming {
    pub median_ttm_secs: Option<i64>,
    pub median_ttfr_secs: Option<i64>,
    /// The 90th percentile of the same samples `median_ttm_secs` describes.
    pub p90_ttm_secs: Option<i64>,
    /// The 90th percentile of the same samples `median_ttfr_secs` describes.
    pub p90_ttfr_secs: Option<i64>,
    pub samples: u64,
}

/// One pull request that is open right now, as the last poll saw it.
///
/// Read from the `pulls` table, which the poller upserts from the PR listing it
/// already fetches — so this costs no GitHub request and, like everything else
/// the engine reports, covers only repos Vigie watches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenPull {
    pub repo_id: u64,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author_login: String,
    pub author_avatar_url: Option<String>,
    pub draft: bool,
    pub created_at: i64,
    pub updated_at: i64,
    /// The later of `updated_at` and the newest stored event on this PR.
    pub last_activity_at: i64,
    /// GitHub's "Pull Request Simple" list shape omits both, so they are `null`
    /// until something fetches the PR on its own.
    pub additions: Option<u32>,
    pub deletions: Option<u32>,
    pub requested_reviewers: Vec<String>,
    /// `approved`, `changes_requested`, `review_required`, or `null` when
    /// nobody has been asked and nobody has reviewed. Derived from the stored
    /// `pr_reviewed` events, newest review per reviewer.
    pub review_decision: Option<String>,
}

/// The three `review_decision` values, so nothing has to spell them twice.
pub const REVIEW_APPROVED: &str = "approved";
pub const REVIEW_CHANGES_REQUESTED: &str = "changes_requested";
pub const REVIEW_REQUIRED: &str = "review_required";

/// The `pulls.state` values. A merged PR is `merged`, never `closed` — the same
/// rule `Watch.state` and `pr_merged` follow.
pub const PULL_STATE_OPEN: &str = "open";

/// One person's activity in the window.
///
/// `total`, `counts` and `last_at` describe the window; `open_prs` and
/// `review_queue` describe *now* — they come from the `pulls` table, which
/// holds each PR's current state rather than a history of it, so a window in
/// the past still reports today's queue. That is deliberate: the queue is a
/// call to action, and a stale one would be worse than none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigestPerson {
    pub login: String,
    pub avatar_url: Option<String>,
    pub total: u64,
    pub counts: KindCounts,
    /// Open pull requests this person authored, right now. Not window-scoped.
    pub open_prs: u64,
    /// Open pull requests that currently request this person as a reviewer.
    /// Not window-scoped.
    pub review_queue: u64,
    /// When this person last did anything inside the window. `start` at the
    /// earliest, `end - 1` at the latest.
    pub last_at: i64,
}

/// One PR or issue's activity in the window. `title` and `url` are the newest
/// event's, so a thread is labelled by the last thing that happened in it.
/// Commits have no thread and count only in `totals`, `people` and `repos`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigestThread {
    pub repo_id: u64,
    pub number: u64,
    pub kind: ThreadKind,
    pub title: String,
    pub url: String,
    pub events: u64,
    pub last_at: i64,
}

/// One repo's event total in the window, and who wrote its commits.
///
/// `top_author_login` is the login with the most `commit` events in this repo
/// inside the window and `top_author_share` its fraction of that repo's
/// commits, in `0.0..=1.0` — a repo at `0.9` is one person's, whatever the team
/// around it looks like. Ties go to the lowest login, so two authors level on
/// commits always give the same answer rather than whichever the hash map
/// happened to yield.
///
/// Commits only: a repo whose window holds reviews and issues but no commits
/// reports `None` and `0.0` rather than pretending a reviewer wrote it.
///
/// `PartialEq` but not `Eq`, because the share is a float.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DigestRepo {
    pub repo_id: u64,
    pub total: u64,
    pub top_author_login: Option<String>,
    pub top_author_share: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `KindCounts` indexes straight into a fixed array, so `index` must be the
    /// position in `ALL` and `ALL` must hold each kind exactly once. A new
    /// variant cannot slip past this unnoticed: `thread_kind` matches
    /// exhaustively, so adding one stops the crate compiling right here.
    #[test]
    fn every_kind_indexes_to_its_own_slot() {
        for (position, kind) in EventKind::ALL.into_iter().enumerate() {
            assert_eq!(kind.index(), position, "{kind:?} is not at its own position");
        }

        // Writing one kind must not disturb any other.
        let mut counts = KindCounts::default();
        for (n, kind) in EventKind::ALL.into_iter().enumerate() {
            counts.set(kind, n as u64 + 1);
        }
        for (n, kind) in EventKind::ALL.into_iter().enumerate() {
            assert_eq!(counts.get(kind), n as u64 + 1, "{kind:?} was overwritten");
        }
        assert_eq!(counts.total(), (1..=EventKind::ALL.len() as u64).sum::<u64>());
    }

    /// A commit belongs to no thread; every other kind belongs to exactly one.
    #[test]
    fn only_commits_have_no_thread_kind() {
        assert_eq!(EventKind::Commit.thread_kind(), None);
        let with_thread = EventKind::ALL
            .into_iter()
            .filter(|kind| kind.thread_kind().is_some())
            .count();
        assert_eq!(with_thread, EventKind::ALL.len() - 1);
        assert_eq!(EventKind::PrCommented.thread_kind(), Some(ThreadKind::Pull));
        assert_eq!(EventKind::IssueCommented.thread_kind(), Some(ThreadKind::Issue));
    }
}
