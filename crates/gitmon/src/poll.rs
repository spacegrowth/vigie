//! One repo's poll: fetch, derive events, and hand them back for filtering.
//!
//! This module performs network I/O only. It never touches the store, so the
//! engine can fetch without holding the database lock.

use crate::error::Result;
use crate::github::models::{
    Commit, Issue, IssueComment, PullRequest, Review, ReviewComment, User,
};
use crate::github::{Conditional, GitHubClient};
use crate::types::{EventKind, Repo, ThreadKind};
use serde::de::DeserializeOwned;
use std::collections::HashMap;

/// The overlap subtracted from the watermark to absorb clock skew.
pub const OVERLAP_SECS: i64 = 300;
/// The window used the first time a repo is polled.
pub const FIRST_POLL_LOOKBACK_SECS: i64 = 24 * 60 * 60;
/// The narrowest span one `backfill` step may reach back.
pub const BACKFILL_MIN_SPAN_SECS: i64 = 60 * 60;
/// The widest span one `backfill` step may reach back. A single call asking for
/// more than this is clamped, not rejected: the caller can simply call again.
pub const BACKFILL_MAX_SPAN_SECS: i64 = 90 * 24 * 60 * 60;
/// What one `backfill` step reaches back when the caller names no span.
pub const BACKFILL_DEFAULT_SPAN_SECS: i64 = 7 * 24 * 60 * 60;
/// Longest `body_preview`, in characters.
const PREVIEW_CHARS: usize = 200;

/// An event derived from a response but not yet filtered or stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewEvent {
    pub kind: EventKind,
    /// Unique within `(repo_id, kind)`; the dedupe index's third column.
    pub external_id: String,
    pub actor_login: String,
    pub actor_avatar_url: Option<String>,
    pub title: String,
    pub body_preview: Option<String>,
    /// The untruncated body, stored so the app can render it without refetching.
    pub body: Option<String>,
    pub url: String,
    pub number: Option<u64>,
    pub occurred_at: i64,
}

/// One PR or issue as a poll saw it in a listing.
///
/// Carries what auto-watch and the state refresh need and nothing else: this
/// module still performs network I/O only, so it hands the facts back and the
/// engine decides what to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ThreadSeen {
    pub number: u64,
    pub kind: ThreadKind,
    pub title: String,
    /// `open`, `closed` or `merged` — a merged PR reports `merged`, never
    /// `closed`, matching the way `pr_merged` displaces `pr_closed`.
    pub state: String,
    pub author_login: String,
    /// Logins GitHub lists under `requested_reviewers`; always empty for an
    /// issue.
    pub requested_reviewers: Vec<String>,
}

/// One pull request exactly as the PR listing described it.
///
/// [`ThreadSeen`] carries the handful of facts auto-watch needs and is shared
/// with issues; this carries the whole row the `pulls` table stores, and only
/// pull requests have one. Both come off the same listing, so neither costs a
/// request of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PullSeen {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author_login: String,
    pub author_avatar_url: Option<String>,
    /// `open`, `closed` or `merged`, from [`pull_state`].
    pub state: String,
    pub draft: bool,
    pub created_at: i64,
    pub updated_at: i64,
    pub merged_at: Option<i64>,
    pub closed_at: Option<i64>,
    /// Absent from the list shape GitHub calls "Pull Request Simple"; present
    /// only when something fetched this PR on its own.
    pub additions: Option<u32>,
    pub deletions: Option<u32>,
    pub requested_reviewers: Vec<String>,
}

/// What one repo's poll produced: the candidate events, and every thread it saw
/// in the PR or issue listing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct RepoPoll {
    pub events: Vec<NewEvent>,
    pub threads: Vec<ThreadSeen>,
    /// Every pull request the listing returned, in listing order. Unlike
    /// `events`, these are not window-filtered: a PR that appears at all is a
    /// PR whose current state we have just been told, and the `pulls` table
    /// records state rather than history.
    pub pulls: Vec<PullSeen>,
    /// True when a listing hit the [`MAX_PAGES`](crate::github::MAX_PAGES) cap
    /// with more pages still on offer, so this window was read incompletely.
    pub truncated: bool,
    /// The ETags GitHub handed back, keyed by the absolute *stable* URL each
    /// belongs to (see [`Lists`]). This module still touches no database: it
    /// reports what it learned and the engine decides whether to store it.
    pub etags: Vec<(String, String)>,
    /// The largest `X-Poll-Interval` any response carried, in seconds, or
    /// `None` when no endpoint offered a hint.
    pub poll_interval: Option<u64>,
}

/// The half-open span of time one fetch covers: `[since, until)`.
///
/// A poll leaves `until` open — everything from the watermark to now — while a
/// backfill closes it, because the window above it has already been fetched and
/// re-deriving it would be pure waste. Every event derivation below tests
/// membership through [`contains`](Self::contains) rather than against a bare
/// `since`, so the two callers cannot drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Window {
    pub since: i64,
    /// Exclusive upper bound. `None` means "no upper bound".
    pub until: Option<i64>,
}

impl Window {
    /// Everything from `since` onwards, with no upper bound — what a poll uses.
    pub fn from(since: i64) -> Window {
        Window { since, until: None }
    }

    /// The half-open `[since, until)` a backfill step fetches.
    pub fn between(since: i64, until: i64) -> Window {
        Window { since, until: Some(until) }
    }

    /// Whether an event at `ts` belongs to this window: `since <= ts < until`.
    pub fn contains(&self, ts: i64) -> bool {
        ts >= self.since && self.until.is_none_or(|until| ts < until)
    }
}

/// The start of the window to fetch for a repo, per the contract: the watermark
/// minus the overlap, or 24h back the first time.
pub(crate) fn window_start(last_polled_at: Option<i64>, now: i64) -> i64 {
    match last_polled_at {
        Some(watermark) => watermark - OVERLAP_SECS,
        None => now - FIRST_POLL_LOOKBACK_SECS,
    }
}

/// Fetches everything in `window` for one repo and derives its events.
///
/// The lower bound is what goes on the wire (`since=` on every endpoint that
/// takes one, plus `until=` for commits, which is the only listing GitHub lets
/// us bound from above). The upper bound is enforced here, on every derived
/// event, so `[since, until)` holds exactly whatever GitHub's own filtering
/// does — a PR list sorted by `updated` happily returns a thread whose *events*
/// all predate the window, and those must not be kept.
///
/// `known_etags` switches conditional requests on: it maps each list endpoint's
/// *stable* URL (see [`Lists`]) to the ETag the last fetch stored for it, and
/// `None` disables conditional requests for this fetch altogether. Backfill
/// always passes `None`.
///
/// Any error aborts this repo only; the caller isolates it.
pub(crate) fn poll_repo(
    client: &GitHubClient,
    repo: &Repo,
    window: Window,
    known_etags: Option<&HashMap<String, String>>,
) -> Result<RepoPoll> {
    let slug = format!("{}/{}", repo.owner, repo.name);
    let since = window.since;
    let since_iso = to_iso8601(since);
    let mut out: Vec<NewEvent> = Vec::new();
    let mut threads: Vec<ThreadSeen> = Vec::new();
    let mut seen_pulls: Vec<PullSeen> = Vec::new();
    let mut lists = Lists::new(client, known_etags);
    // number -> title, so comments can carry their PR/issue title.
    let mut titles: HashMap<u64, String> = HashMap::new();

    // Every conditional listing below is built the same way: a stable URL that
    // carries only the parameters that never move, then the window's own
    // `since=`/`until=` appended to make the URL that actually goes on the
    // wire. The stable half is the ETag key; the whole thing is the request.
    let commits_key = format!(
        "/repos/{slug}/commits?sha={}&per_page=100",
        encode(&repo.default_branch)
    );
    let pulls_key =
        format!("/repos/{slug}/pulls?state=all&sort=updated&direction=desc&per_page=100");
    let issues_key =
        format!("/repos/{slug}/issues?state=all&sort=updated&direction=desc&per_page=100");
    let issue_comments_key =
        format!("/repos/{slug}/issues/comments?sort=updated&direction=desc&per_page=100");
    let review_comments_key =
        format!("/repos/{slug}/pulls/comments?sort=updated&direction=desc&per_page=100");

    // --- commits on the default branch ---------------------------------
    // Commits are the one listing GitHub bounds from above for us, so the
    // window's upper edge goes on the wire here and is not paid for in pages.
    let until_param = match window.until {
        Some(until) => format!("&until={}", encode(&to_iso8601(until))),
        None => String::new(),
    };
    let commits: Vec<Commit> = lists.list(
        &commits_key,
        &format!("{commits_key}&since={}{until_param}", encode(&since_iso)),
        |_| true,
    )?;
    for commit in &commits {
        if let Some(event) = commit_event(commit, window) {
            out.push(event);
        }
    }

    // --- pull requests, newest-updated first, stopping at the window edge ---
    // This listing carries no `since` at all, so its stable URL *is* its
    // request URL and its ETag needs no special care.
    let pulls: Vec<PullRequest> = lists.list(&pulls_key, &pulls_key, |pr: &PullRequest| {
        parse_ts(&pr.updated_at).is_none_or(|updated| updated >= since)
    })?;
    for pr in &pulls {
        titles.insert(pr.number, pr.title.clone());
        let reviewers: Vec<String> =
            pr.requested_reviewers.iter().map(|user| user.login.clone()).collect();
        let (author_login, author_avatar_url) = actor_of(pr.user.as_ref());
        threads.push(ThreadSeen {
            number: pr.number,
            kind: ThreadKind::Pull,
            title: pr.title.clone(),
            state: pull_state(pr),
            author_login: author_login.clone(),
            requested_reviewers: reviewers.clone(),
        });
        // A PR whose `created_at` GitHub sent in a shape chrono cannot read has
        // no honest place on a timeline, so it is left out of `pulls` rather
        // than filed under the epoch. Its events are unaffected.
        if let Some(created_at) = parse_ts(&pr.created_at) {
            seen_pulls.push(PullSeen {
                number: pr.number,
                title: pr.title.clone(),
                url: pr.html_url.clone(),
                author_login,
                author_avatar_url,
                state: pull_state(pr),
                draft: pr.draft,
                created_at,
                updated_at: parse_ts(&pr.updated_at).unwrap_or(created_at),
                merged_at: pr.merged_at.as_deref().and_then(parse_ts),
                closed_at: pr.closed_at.as_deref().and_then(parse_ts),
                additions: pr.additions,
                deletions: pr.deletions,
                requested_reviewers: reviewers,
            });
        }
        out.extend(pull_request_events(pr, window));
    }

    // --- reviews for every PR touched in the window ---------------------
    // Unconditional on purpose: one URL per PR would need one ETag row per PR,
    // and the PR listing above already 304s when nothing on any PR moved.
    for pr in &pulls {
        let reviews: Vec<Review> = lists
            .unconditional(&format!("/repos/{slug}/pulls/{}/reviews?per_page=100", pr.number))?;
        for review in &reviews {
            if let Some(event) = review_event(review, pr.number, window) {
                out.push(event);
            }
        }
    }

    // --- issues (pull requests excluded) --------------------------------
    let issues: Vec<Issue> = lists.list(
        &issues_key,
        &format!("{issues_key}&since={}", encode(&since_iso)),
        |_| true,
    )?;
    for issue in &issues {
        if issue.pull_request.is_some() {
            continue;
        }
        titles.insert(issue.number, issue.title.clone());
        threads.push(ThreadSeen {
            number: issue.number,
            kind: ThreadKind::Issue,
            title: issue.title.clone(),
            state: issue.state.clone().unwrap_or_else(|| "open".to_string()),
            author_login: actor_of(issue.user.as_ref()).0,
            requested_reviewers: Vec::new(),
        });
        if let Some(event) = issue_event(issue, window) {
            out.push(event);
        }
    }

    // --- comments on issues and PRs -------------------------------------
    let issue_comments: Vec<IssueComment> = lists.list(
        &issue_comments_key,
        &format!("{issue_comments_key}&since={}", encode(&since_iso)),
        |_| true,
    )?;
    let review_comments: Vec<ReviewComment> = lists.list(
        &review_comments_key,
        &format!("{review_comments_key}&since={}", encode(&since_iso)),
        |_| true,
    )?;

    let mut resolver = TitleResolver { client, slug: &slug, titles };
    for comment in &issue_comments {
        if let Some(event) = issue_comment_event(comment, window, &mut resolver) {
            out.push(event);
        }
    }
    for comment in &review_comments {
        if let Some(event) = review_comment_event(comment, window, &mut resolver) {
            out.push(event);
        }
    }

    Ok(RepoPoll {
        events: out,
        threads,
        pulls: seen_pulls,
        truncated: lists.truncated,
        etags: lists.fresh,
        poll_interval: lists.poll_interval,
    })
}

/// The list reads one repo fetch makes, plus the conditional-request
/// bookkeeping that rides along with them.
///
/// **The stable-URL rule.** An ETag identifies one exact URL, query string
/// included — and every poll's URL carries a `since=` that moves with the
/// watermark, so keying the cache on the URL that goes on the wire would miss
/// every time. So each endpoint's tag is filed under a *stable* URL instead:
/// the same request with its `since=`/`until=` dropped. That URL is a key and
/// nothing else; it is never fetched, and dropping `since` from the request
/// itself would change what GitHub returns.
///
/// The key is sound because an ETag is a hash of the response body. In the
/// steady state — a quiet repo, polled again one interval later — the body for
/// `since=T1` and the body for `since=T2` are the same bytes (usually `[]`), so
/// GitHub answers 304 and the poll costs nothing. When something did change,
/// the bodies differ and GitHub answers 200 with the new one. Either way the
/// answer is correct; the tag only decides whether it was free.
///
/// That reasoning holds only while `since` is the ordinary
/// watermark-minus-overlap. A first poll reaching back 24 hours, or a backfill
/// reaching into last month, asks a different question of the same endpoint, so
/// the engine passes `known` as `None` for those and no tag goes out.
struct Lists<'a> {
    client: &'a GitHubClient,
    /// ETags the store had for this repo, keyed by absolute stable URL. `None`
    /// disables conditional requests for this whole fetch.
    known: Option<&'a HashMap<String, String>>,
    /// Tags GitHub handed back, for the engine to store.
    fresh: Vec<(String, String)>,
    poll_interval: Option<u64>,
    truncated: bool,
}

impl<'a> Lists<'a> {
    fn new(client: &'a GitHubClient, known: Option<&'a HashMap<String, String>>) -> Lists<'a> {
        Lists { client, known, fresh: Vec::new(), poll_interval: None, truncated: false }
    }

    /// One conditional list read. `key` is the stable URL the ETag is filed
    /// under and is never requested; `url` is what goes on the wire.
    ///
    /// A `NotModified` yields no items, which is exactly right: this endpoint
    /// contributes no events to this poll.
    fn list<T, F>(&mut self, key: &str, url: &str, keep: F) -> Result<Vec<T>>
    where
        T: DeserializeOwned,
        F: FnMut(&T) -> bool,
    {
        let client = self.client;
        let key = client.url_for(key);
        let etag = self.known.and_then(|known| known.get(&key));
        match client.get_paged_conditional(url, etag.map(String::as_str), keep)? {
            Conditional::NotModified => Ok(Vec::new()),
            Conditional::Fresh { value, etag, poll_interval } => {
                if let Some(etag) = etag {
                    self.fresh.push((key, etag));
                }
                // `Option`'s ordering puts `None` below every `Some`, so this
                // keeps the largest hint this fetch saw.
                self.poll_interval = self.poll_interval.max(poll_interval);
                self.truncated |= value.truncated;
                Ok(value.items)
            }
        }
    }

    /// A list read that never carries an ETag and never stores one.
    fn unconditional<T: DeserializeOwned>(&mut self, url: &str) -> Result<Vec<T>> {
        let (items, cut) = self.client.get_paged_capped(url, |_| true)?;
        self.truncated |= cut;
        Ok(items)
    }
}

/// A pull request's state as a watch records it. GitHub's own `state` never
/// says `merged` — it says `closed` and sets `merged_at` — so the merge has to
/// be read off that, exactly as `pull_request_events` does.
fn pull_state(pr: &PullRequest) -> String {
    if pr.merged_at.is_some() {
        return "merged".to_string();
    }
    pr.state.clone().unwrap_or_else(|| "open".to_string())
}

/// Looks up PR/issue titles, falling back to a single extra fetch per unknown
/// number. A failed lookup degrades to `#<number>` rather than failing the poll.
///
/// SHORTCUT: titles are resolved per poll with no cross-poll cache, so a comment
/// on a PR/issue that was not itself touched in the window costs one extra
/// request. Fine while the miss rate is low (comments almost always land on
/// something updated in the same window). If polls ever show a burst of these,
/// give the store a `title` column on the PR/issue number and read it back here
/// instead of refetching.
struct TitleResolver<'a> {
    client: &'a GitHubClient,
    slug: &'a str,
    titles: HashMap<u64, String>,
}

impl TitleResolver<'_> {
    fn title_for(&mut self, number: u64) -> String {
        if let Some(title) = self.titles.get(&number) {
            return title.clone();
        }
        let fetched: Option<Issue> = self
            .client
            .get(&format!("/repos/{}/issues/{number}", self.slug))
            .map_err(|e| {
                log::debug!("could not read title for {}#{number}: {e}", self.slug);
                e
            })
            .ok();
        let title = fetched.map(|i| i.title).unwrap_or_else(|| format!("#{number}"));
        self.titles.insert(number, title.clone());
        title
    }
}

// ---- event derivation ---------------------------------------------------

/// The authored time, falling back to the committed time.
pub(crate) fn commit_time(commit: &Commit) -> Option<i64> {
    commit
        .commit
        .author
        .as_ref()
        .and_then(|a| a.date.as_deref())
        .or_else(|| commit.commit.committer.as_ref().and_then(|c| c.date.as_deref()))
        .and_then(parse_ts)
}

/// GitHub account first, then the raw git author name with no avatar.
/// Shared with `Engine::get_commit` so a commit is attributed identically
/// whether it arrives through a poll or through in-app reading.
pub(crate) fn commit_actor(commit: &Commit) -> (String, Option<String>) {
    match (&commit.author, &commit.committer) {
        (Some(user), _) => (user.login.clone(), user.avatar_url.clone()),
        (None, Some(user)) => (user.login.clone(), user.avatar_url.clone()),
        (None, None) => (
            commit
                .commit
                .author
                .as_ref()
                .and_then(|a| a.name.clone())
                .unwrap_or_else(|| "unknown".to_string()),
            None,
        ),
    }
}

fn commit_event(commit: &Commit, window: Window) -> Option<NewEvent> {
    let occurred_at = commit_time(commit)?;
    // GitHub's own `since`/`until` filter on `/commits` bounds the *committer*
    // date while the event is stamped with the author date, and the two differ
    // on any rebased or cherry-picked commit. The window is re-checked here so
    // `[since, until)` means one thing, not two.
    if !window.contains(occurred_at) {
        return None;
    }
    let (actor_login, actor_avatar_url) = commit_actor(commit);

    Some(NewEvent {
        kind: EventKind::Commit,
        external_id: commit.sha.clone(),
        actor_login,
        actor_avatar_url,
        title: first_line(&commit.commit.message),
        body_preview: preview(rest_of_message(&commit.commit.message).as_deref()),
        body: non_empty(&commit.commit.message),
        url: commit.html_url.clone(),
        number: None,
        occurred_at,
    })
}

/// Emits at most one opened event and at most one close-or-merge event. A merged
/// PR yields `pr_merged` and never `pr_closed`.
fn pull_request_events(pr: &PullRequest, window: Window) -> Vec<NewEvent> {
    let mut events = Vec::new();
    let (author_login, author_avatar) = actor_of(pr.user.as_ref());

    if let Some(created) = parse_ts(&pr.created_at) {
        if window.contains(created) {
            events.push(NewEvent {
                kind: EventKind::PrOpened,
                external_id: format!("{}:opened", pr.number),
                actor_login: author_login.clone(),
                actor_avatar_url: author_avatar.clone(),
                title: pr.title.clone(),
                body_preview: preview(pr.body.as_deref()),
                body: pr.body.clone(),
                url: pr.html_url.clone(),
                number: Some(pr.number),
                occurred_at: created,
            });
        }
    }

    if let Some(merged) = pr.merged_at.as_deref().and_then(parse_ts) {
        if window.contains(merged) {
            // The list endpoint omits `merged_by`; fall back to the author.
            let (login, avatar) = match pr.merged_by.as_ref() {
                Some(user) => (user.login.clone(), user.avatar_url.clone()),
                None => (author_login.clone(), author_avatar.clone()),
            };
            events.push(NewEvent {
                kind: EventKind::PrMerged,
                external_id: format!("{}:merged", pr.number),
                actor_login: login,
                actor_avatar_url: avatar,
                title: pr.title.clone(),
                body_preview: preview(pr.body.as_deref()),
                body: pr.body.clone(),
                url: pr.html_url.clone(),
                number: Some(pr.number),
                occurred_at: merged,
            });
        }
        return events;
    }

    if let Some(closed) = pr.closed_at.as_deref().and_then(parse_ts) {
        if window.contains(closed) {
            events.push(NewEvent {
                kind: EventKind::PrClosed,
                external_id: format!("{}:closed", pr.number),
                actor_login: author_login,
                actor_avatar_url: author_avatar,
                title: pr.title.clone(),
                body_preview: preview(pr.body.as_deref()),
                body: pr.body.clone(),
                url: pr.html_url.clone(),
                number: Some(pr.number),
                occurred_at: closed,
            });
        }
    }
    events
}

fn review_event(review: &Review, number: u64, window: Window) -> Option<NewEvent> {
    // A pending review has no submission time and is not yet visible to anyone.
    let submitted = review.submitted_at.as_deref().and_then(parse_ts)?;
    if !window.contains(submitted) {
        return None;
    }
    let title = review_title(&review.state)?;
    let (actor_login, actor_avatar_url) = actor_of(review.user.as_ref());
    Some(NewEvent {
        kind: EventKind::PrReviewed,
        external_id: review.id.to_string(),
        actor_login,
        actor_avatar_url,
        title,
        body_preview: preview(review.body.as_deref()),
        body: review.body.clone(),
        url: review.html_url.clone(),
        number: Some(number),
        occurred_at: submitted,
    })
}

/// The contract names exactly three review titles; other states are not events.
fn review_title(state: &str) -> Option<String> {
    match state.to_ascii_uppercase().as_str() {
        "APPROVED" => Some("Review: approved".to_string()),
        "CHANGES_REQUESTED" => Some("Review: changes requested".to_string()),
        "COMMENTED" => Some("Review: commented".to_string()),
        _ => None,
    }
}

fn issue_event(issue: &Issue, window: Window) -> Option<NewEvent> {
    let created = parse_ts(&issue.created_at)?;
    if !window.contains(created) {
        return None;
    }
    let (actor_login, actor_avatar_url) = actor_of(issue.user.as_ref());
    Some(NewEvent {
        kind: EventKind::IssueOpened,
        external_id: format!("{}:opened", issue.number),
        actor_login,
        actor_avatar_url,
        title: issue.title.clone(),
        body_preview: preview(issue.body.as_deref()),
        body: issue.body.clone(),
        url: issue.html_url.clone(),
        number: Some(issue.number),
        occurred_at: created,
    })
}

fn issue_comment_event(
    comment: &IssueComment,
    window: Window,
    resolver: &mut TitleResolver<'_>,
) -> Option<NewEvent> {
    let created = parse_ts(&comment.created_at)?;
    if !window.contains(created) {
        return None;
    }
    let number = comment
        .issue_url
        .as_deref()
        .and_then(trailing_number)
        .or_else(|| number_from_html_url(&comment.html_url))?;
    // Only the html_url distinguishes a PR conversation from an issue one.
    let kind = if comment.html_url.contains("/pull/") {
        EventKind::PrCommented
    } else {
        EventKind::IssueCommented
    };
    let (actor_login, actor_avatar_url) = actor_of(comment.user.as_ref());
    Some(NewEvent {
        kind,
        // Issue-comment and review-comment ids come from separate id spaces, so
        // both are prefixed to keep them from colliding under `pr_commented`.
        external_id: format!("ic:{}", comment.id),
        actor_login,
        actor_avatar_url,
        title: resolver.title_for(number),
        body_preview: preview(comment.body.as_deref()),
        body: comment.body.clone(),
        url: comment.html_url.clone(),
        number: Some(number),
        occurred_at: created,
    })
}

fn review_comment_event(
    comment: &ReviewComment,
    window: Window,
    resolver: &mut TitleResolver<'_>,
) -> Option<NewEvent> {
    let created = parse_ts(&comment.created_at)?;
    if !window.contains(created) {
        return None;
    }
    let number = comment
        .pull_request_url
        .as_deref()
        .and_then(trailing_number)
        .or_else(|| number_from_html_url(&comment.html_url))?;
    let (actor_login, actor_avatar_url) = actor_of(comment.user.as_ref());
    Some(NewEvent {
        kind: EventKind::PrCommented,
        external_id: format!("rc:{}", comment.id),
        actor_login,
        actor_avatar_url,
        title: resolver.title_for(number),
        body_preview: preview(comment.body.as_deref()),
        body: comment.body.clone(),
        url: comment.html_url.clone(),
        number: Some(number),
        occurred_at: created,
    })
}

// ---- helpers ------------------------------------------------------------

fn actor_of(user: Option<&User>) -> (String, Option<String>) {
    match user {
        Some(user) => (user.login.clone(), user.avatar_url.clone()),
        None => ("ghost".to_string(), None),
    }
}

fn first_line(message: &str) -> String {
    message.lines().next().unwrap_or("").trim().to_string()
}

fn rest_of_message(message: &str) -> Option<String> {
    let mut lines = message.lines();
    lines.next()?;
    let rest = lines.collect::<Vec<_>>().join(" ");
    if rest.trim().is_empty() {
        None
    } else {
        Some(rest)
    }
}

/// First 200 characters, whitespace collapsed. Empty bodies become `None`.
fn preview(body: Option<&str>) -> Option<String> {
    let collapsed = body?.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    Some(collapsed.chars().take(PREVIEW_CHARS).collect())
}

/// The message as-is, or `None` when it is blank. Unlike `preview` this does
/// not collapse whitespace or truncate: the app renders the markdown verbatim.
pub(crate) fn non_empty(message: &str) -> Option<String> {
    if message.trim().is_empty() {
        None
    } else {
        Some(message.to_string())
    }
}

fn trailing_number(url: &str) -> Option<u64> {
    url.trim_end_matches('/').rsplit('/').next()?.parse().ok()
}

/// `https://github.com/o/n/pull/12#issuecomment-5` -> 12
fn number_from_html_url(url: &str) -> Option<u64> {
    let without_fragment = url.split('#').next()?;
    trailing_number(without_fragment)
}

pub(crate) fn parse_ts(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value).ok().map(|dt| dt.timestamp())
}

pub(crate) fn to_iso8601(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .unwrap_or_else(|| chrono::DateTime::from_timestamp(0, 0).expect("epoch is valid"))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

fn encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_uses_overlap_then_24h_lookback() {
        assert_eq!(window_start(Some(10_000), 20_000), 10_000 - OVERLAP_SECS);
        assert_eq!(window_start(None, 20_000), 20_000 - FIRST_POLL_LOOKBACK_SECS);
    }

    /// The window is half-open: an event exactly on `since` is inside it and one
    /// exactly on `until` is not — so two adjacent backfill steps that share an
    /// edge each claim it once, never twice and never zero times.
    #[test]
    fn a_closed_window_is_half_open_and_an_open_one_has_no_ceiling() {
        let closed = Window::between(100, 200);
        assert!(closed.contains(100), "the lower edge is inside");
        assert!(closed.contains(199));
        assert!(!closed.contains(200), "the upper edge belongs to the window above");
        assert!(!closed.contains(99));

        let open = Window::from(100);
        assert!(open.contains(100));
        assert!(open.contains(i64::MAX));
        assert!(!open.contains(99));
    }

    #[test]
    fn preview_collapses_whitespace_and_clamps() {
        assert_eq!(preview(Some("  a\n\n b  ")).unwrap(), "a b");
        assert_eq!(preview(Some("   ")), None);
        assert_eq!(preview(None), None);
        let long = "x".repeat(500);
        assert_eq!(preview(Some(&long)).unwrap().chars().count(), PREVIEW_CHARS);
    }

    #[test]
    fn numbers_come_off_api_and_html_urls() {
        assert_eq!(trailing_number("https://api.github.com/repos/o/n/issues/42"), Some(42));
        assert_eq!(
            number_from_html_url("https://github.com/o/n/pull/12#issuecomment-5"),
            Some(12)
        );
    }

    #[test]
    fn only_the_three_contract_review_titles_are_events() {
        assert_eq!(review_title("APPROVED").unwrap(), "Review: approved");
        assert_eq!(review_title("CHANGES_REQUESTED").unwrap(), "Review: changes requested");
        assert_eq!(review_title("COMMENTED").unwrap(), "Review: commented");
        assert_eq!(review_title("PENDING"), None);
        assert_eq!(review_title("DISMISSED"), None);
    }

    /// GitHub reports a merged PR as `state: "closed"` with a `merged_at`, so a
    /// watch that trusted `state` alone would never say `merged`.
    #[test]
    fn a_merged_pull_request_reports_merged_not_closed() {
        let pr = |state: Option<&str>, merged_at: Option<&str>| PullRequest {
            number: 1,
            title: "t".to_string(),
            html_url: "https://x".to_string(),
            user: None,
            body: None,
            state: state.map(str::to_string),
            created_at: "1970-01-01T00:00:00Z".to_string(),
            updated_at: "1970-01-01T00:00:00Z".to_string(),
            closed_at: merged_at.map(str::to_string),
            merged_at: merged_at.map(str::to_string),
            draft: false,
            merged_by: None,
            requested_reviewers: Vec::new(),
            head: None,
            base: None,
            additions: None,
            deletions: None,
            changed_files: None,
        };
        assert_eq!(pull_state(&pr(Some("open"), None)), "open");
        assert_eq!(pull_state(&pr(Some("closed"), None)), "closed");
        assert_eq!(
            pull_state(&pr(Some("closed"), Some("1970-01-01T00:00:10Z"))),
            "merged",
            "merged_at wins over the closed state GitHub reports beside it"
        );
        // A payload without `state` at all is treated as open, not as unknown.
        assert_eq!(pull_state(&pr(None, None)), "open");
    }

    #[test]
    fn timestamps_round_trip() {
        assert_eq!(parse_ts("1970-01-01T00:00:10Z"), Some(10));
        assert_eq!(to_iso8601(10), "1970-01-01T00:00:10Z");
        assert_eq!(parse_ts("not a date"), None);
    }
}
