# Engine contract

This is the interface between the Rust engine crate (`crates/gitmon`) and the Tauri
app (`app/`). Both sides are built against this document. Change it only by editing
this file first; the lead owns it.

## Domain

- **Team** — a named, ordered list of GitHub logins. An install has any number of teams;
  a login may belong to several. Teams categorise people; repos are shared by all teams.
- **Account** — a signed-in GitHub login with its own token. An install may have several
  (personal and work, say). Each repo belongs to the account that added it and is polled
  with that account's token. See "Accounts".
- **Repo** — a GitHub repository `owner/name` the app polls, owned by one account.
- **Event** — one thing a person did in a repo. Events are produced by polling, stored
  once, and never mutated except for `seen`.
- **Filter** — either `all` (every actor) or `team` (only logins in at least one team, plus a
  signed-in account's own login regardless of its team). Applied at `list_events` read time as
  `mode` — the feed's "My team / Everyone" control — never at ingestion: a poll and a backfill
  store every event they fetch, whoever the actor is, so narrowing or widening this is instant
  and never loses anything. When no team has any member, `team` mode passes everyone (an install
  that has not been configured yet must not read as one configured to exclude everybody); the app
  says so where the control lives. `Settings.filter_mode` carries this same value as what a fresh
  window's view filter starts from — see "Settings" below for the versions where it instead
  gated ingestion.
- **Watch** — a PR or issue the user follows. Every event on a watched thread always notifies,
  whatever `notify_kinds` says — unrelated to the Filter above, which a watched thread's events
  are still subject to like any other (`list_events`'s `mode` and `watched_only` compose, neither
  bypasses the other). Watches are added by hand or automatically (PRs the signed-in user
  authored or was asked to review). See "Watched threads".

## Types (serde JSON shapes; snake_case keys; timestamps are unix seconds, UTC)

```
Team        { id: u64, name: string, logins: string[] }   // id 0 = not yet saved (import_org_team)
Account     { login: string, avatar_url: string|null, added_at: i64 }
Repo        { id: u64, owner: string, name: string, url: string, account_login: string,
              default_branch: string, last_polled_at: i64|null, backfilled_to: i64|null,
              last_error: string|null,
              hidden: bool }           // out of every read and every poll while true; see "Hiding a repo"
FilterMode  "all" | "team"
EventKind   "commit" | "pr_opened" | "pr_merged" | "pr_closed" | "pr_reviewed"
            | "pr_commented" | "issue_opened" | "issue_commented"
Event       { id: u64, repo_id: u64, kind: EventKind, actor_login: string,
              actor_avatar_url: string|null, title: string, body_preview: string|null,
              body: string|null, url: string, number: u64|null, occurred_at: i64, seen: bool,
              team_ids: u64[],         // computed at read time from current team membership; not stored
              watched: bool }          // computed at read time: (repo_id, number) is a current Watch; not stored
Watch       { repo_id: u64, number: u64, kind: "pull" | "issue", title: string, state: string,
              source: "manual" | "author" | "reviewer", since: i64 }   // state as last seen: open|closed|merged
Thread      { repo_id: u64, number: u64, kind: "pull" | "issue", title: string, state: string,
              author_login: string, author_avatar_url: string|null, body: string|null,
              url: string, created_at: i64, head_ref: string|null, base_ref: string|null,
              additions: u32|null, deletions: u32|null, changed_files: u32|null,
              items: ThreadItem[] }                       // chronological
ThreadItem  { kind: "comment" | "review" | "review_comment" | "commit",
              actor_login: string, actor_avatar_url: string|null, body: string|null,
              state: string|null, path: string|null, line: u32|null, url: string, at: i64,
              sha: string|null }       // the commit sha for kind "commit", else null; feeds get_commit
CommitDetail{ repo_id: u64, sha: string, message: string, author_login: string,
              author_avatar_url: string|null, url: string, at: i64,
              additions: u32, deletions: u32, files: CommitFile[] }
CommitFile  { path: string, status: string, additions: u32, deletions: u32, patch: string|null }
RepoError   { repo_id: u64, message: string, reset_at: i64|null }   // reset_at when the error was rate_limited
PollResult  { new_events: Event[], errors: RepoError[], rate_limit_remaining: u32|null,
              rate_limit_reset_at: i64|null,      // unix seconds the budget refills; null if GitHub sent no reset header
              rate_limit_limit: u32|null,         // the budget's size; null if GitHub sent no limit header (treat as 5,000)
              skipped: u64[] }         // repo ids left alone because GitHub's X-Poll-Interval had not elapsed
PollOptions { force: bool }            // default false; true ignores every repo's X-Poll-Interval
QuietHours  { start_minute: u16, end_minute: u16 }   // minutes after local midnight; may wrap
Settings    { poll_interval_secs: u32,               // default 120, min 30
              filter_mode: FilterMode,               // default "team"; the view filter's starting value (see "Filter") — no longer gates ingestion
              notifications_enabled: bool,           // default true
              notify_kinds: EventKind[],             // default: all kinds
              quiet_hours: QuietHours|null,
              auto_watch: bool }                     // default true; see "Watched threads"
PersonSuggestion { login: string, avatar_url: string|null, source: "contributor" | "org_member" | "bot",
                   why: string }          // e.g. "41 commits · acme/platform", "member of acme", "dependabot"
RepoSuggestion   { owner: string, name: string, why: string }   // e.g. "you pushed 2 days ago", "12 open PRs"
DeviceLogin      { device_code: string, user_code: string, verification_uri: string,
                   verification_uri_complete: string|null, expires_in: u32, interval: u32 }
DeviceLoginStatus  "pending" | { token: string, login: string }   // JSON: {"status":"pending"} or {"status":"ok","token":..,"login":..}
Digest      { start: i64, end: i64, team_id: u64|null, actor: string|null,
              totals: { [EventKind]: u64 },          // every kind present, zero included
              people: DigestPerson[],                // by total desc, then login; top 20
              people_total: u64,                     // distinct people in the window, before the top-20 cut
              threads: DigestThread[],               // by events desc, then last_at desc; top 10
              repos: DigestRepo[],                   // by total desc; every watched repo with >0
              series: DayCounts[],                   // one per LOCAL day in the window, oldest first, gapless
              hours: u64[7][24],                     // [weekday Mon=0][local hour]; always 7 x 24
              pr_timing: PrTiming,
              unreviewed_merges: u64 }               // merges in the window with no review before them
DigestPerson{ login: string, avatar_url: string|null, total: u64, counts: { [EventKind]: u64 },
              open_prs: u64,          // open PRs they authored, NOW; not window-scoped
              review_queue: u64,      // open PRs currently requesting their review, NOW; not window-scoped
              last_at: i64 }          // their newest event inside the window
DigestThread{ repo_id: u64, number: u64, kind: "pull" | "issue", title: string, url: string,
              events: u64, last_at: i64 }
DigestRepo  { repo_id: u64, total: u64,
              top_author_login: string|null,         // most commits in the window; null when there are none
              top_author_share: f32 }                // that author's share of the repo's commits, 0..1
DayCounts   { day: i64, counts: { [EventKind]: u64 } }   // day = unix second that local day began
PrTiming    { median_ttm_secs: i64|null, median_ttfr_secs: i64|null,
              p90_ttm_secs: i64|null, p90_ttfr_secs: i64|null,   // same samples as the medians
              samples: u64 }
OpenPull    { repo_id: u64, number: u64, title: string, url: string, author_login: string,
              author_avatar_url: string|null, draft: bool, created_at: i64, updated_at: i64,
              last_activity_at: i64, additions: u32|null, deletions: u32|null,
              requested_reviewers: string[],
              review_decision: "approved" | "changes_requested" | "review_required" | null }
RepoBackfill    { repo_id: u64, from: i64, to: i64, inserted: u64, error: string|null }
BackfillResult  { repos: RepoBackfill[], rate_limit_remaining: u32|null,
                  rate_limit_reset_at: i64|null, rate_limit_limit: u32|null }   // same meaning as PollResult's
EngineError { kind: "auth" | "not_found" | "rate_limited" | "network" | "storage" | "invalid",
              message: string, reset_at: i64|null }  // reset_at only for rate_limited
```

`title` is one line: commit subject, PR/issue title, or `Review: approved` /
`Review: changes requested` / `Review: commented`. `body` is the full markdown body of the
comment, review, PR, issue, or commit message (nothing truncated; the app renders it).
`body_preview` is the first 200 chars of `body`, whitespace-collapsed, for single-line rows. `number` is the PR/issue number, null
for commits. `url` is the html_url of the specific thing (commit, PR, comment).

## Rust engine API (`gitmon::Engine`)

Synchronous, `Send + Sync`, one instance per process. All fallible methods return
`Result<T, EngineError>`.

```
Engine::open(data_dir: &Path) -> Result<Engine>          // creates data_dir/gitmon.db
engine.set_token(token: Option<String>)                  // legacy single-account path; see "Accounts"
engine.verify_token() -> Result<String>                  // returns the authenticated login (internal to sign-in)
engine.list_accounts() -> Vec<Account>                   // in added order
engine.add_account(token: &str) -> Result<Account>       // verifies, remembers the token in memory, claims orphan repos; see "Accounts"
engine.set_account_token(login: &str, token: &str)       // at startup, from the keychain; in-memory only
engine.forget_account_token(login: &str)                 // "sign out": drops the in-memory token only; the row, repos, events and watches are untouched
engine.remove_account(login: &str) -> Result<()>         // cascades its repos and their events; not_found if unknown
engine.start_device_login(client_id: &str) -> Result<DeviceLogin>       // see "Sign-in" below
engine.poll_device_login(client_id: &str, device_code: &str) -> Result<DeviceLoginStatus>

engine.list_teams() -> Vec<Team>                          // in display order
engine.create_team(name: &str, logins: &[String]) -> Result<Team>   // appended last; logins deduped, lowercased
engine.update_team(team: Team) -> Result<()>            // by id; name and logins replaced wholesale; not_found if unknown
engine.delete_team(team_id: u64) -> Result<()>          // events are untouched (they are not owned by teams)
engine.reorder_teams(ids: &[u64]) -> Result<()>         // invalid unless it is a permutation of every team id
engine.import_org_team(org: &str, team_slug: &str) -> Result<Team>   // fetches, does NOT save; returned id is 0
engine.suggest_people(query: &str, team_id: Option<u64>, limit: u32) -> Result<Vec<PersonSuggestion>>
                                                         // see "People suggestions" below
engine.suggest_active_people(query: &str, team_id: Option<u64>, limit: u32) -> Result<Vec<PersonSuggestion>>
                                                         // suggest_people's mirror image; see "People suggestions" below
engine.suggest_repos(account_login: Option<&str>) -> Result<Vec<RepoSuggestion>>   // that account's repos (default: every account, merged)

engine.add_repo(spec: &str, account_login: Option<&str>) -> Result<Repo>   // owner/name or any github.com URL; account defaults to the only account, else invalid
engine.remove_repo(repo_id: u64) -> Result<()>           // cascades events; the destructive one, see "Hiding a repo"
engine.list_repos() -> Vec<Repo>                         // visible repos only — what the app shows and the poller spends on
engine.list_hidden_repos() -> Vec<Repo>                  // the hidden ones, for the Repos view's own "Hidden" section
engine.set_repo_hidden(repo_id: u64, hidden: bool) -> Result<()>   // not_found if unknown; see "Hiding a repo"

engine.poll_now() -> PollResult                          // polls every repo; never panics; per-repo errors in .errors
engine.poll_now_with(options: PollOptions) -> PollResult  // the same poll; options.force ignores the X-Poll-Interval floor
engine.rescan(repo_id: Option<u64>) -> Result<()>       // clears the watermark(s) so the next poll refetches the last 24 h; dedupe makes it safe
engine.backfill(repo_id: Option<u64>, span_secs: i64) -> Result<BackfillResult>
                                                         // fetches the next OLDER window per repo; see "Backfill" below.
                                                         // not_found for an unknown repo_id; every other failure is per-repo, in .repos[].error
engine.list_events(repo_id: Option<u64>, actor: Option<&str>, team_id: Option<u64>, mode: FilterMode, watched_only: bool, before_id: Option<u64>, limit: u32) -> Vec<Event>
                                                         // newest first by (occurred_at, id); limit clamped to 500;
                                                         // repo_id and team_id keep events in that repo / whose actor is
                                                         // currently in that team (unknown id -> empty page, not an error);
                                                         // mode is the "My team / Everyone" view filter, see "Filter";
                                                         // watched_only keeps events on currently watched threads;
                                                         // every filter here composes with the others (an intersection)
engine.get_thread(repo_id: u64, number: u64) -> Result<Thread>        // live fetch, see "Reading in-app"
engine.get_commit(repo_id: u64, sha: &str) -> Result<CommitDetail>    // live fetch
engine.get_pull_files(repo_id: u64, number: u64) -> Result<Vec<CommitFile>>   // live fetch, see "Reading in-app"

engine.list_watches() -> Vec<Watch>                      // newest `since` first
engine.watch_thread(repo_id: u64, number: u64) -> Result<Watch>       // source "manual"; idempotent; see "Watched threads"
engine.unwatch_thread(repo_id: u64, number: u64) -> Result<()>        // not_found if not watched
engine.digest(start: i64, end: i64, team_id: Option<u64>, actor: Option<&str>, tz_offset_secs: i32) -> Result<Digest>
                                                         // see "Digests" below
engine.open_pulls(team_id: Option<u64>, actor: Option<&str>, reviewer: Option<&str>) -> Result<Vec<OpenPull>>
                                                         // open only, created_at ascending; see "Pulls" below
engine.mark_seen(ids: &[u64]) -> Result<()>
engine.unseen_count() -> u64

engine.get_settings() -> Settings
engine.set_settings(s: Settings) -> Result<()>
```

## Polling semantics

- Per repo, keep a watermark `last_polled_at`. On each poll fetch, with `since = watermark - 300s`
  (overlap absorbs clock skew): commits on the default branch; PRs sorted by `updated`
  descending stopping at the first PR updated before `since`; issue comments; PR review
  comments; issues (excluding PRs); and reviews for every PR updated in the window.
- First poll of a new repo uses `since = now - 24h`.
- Dedupe with a UNIQUE index on `(repo_id, kind, external_id)` where `external_id` is the
  commit sha, PR/issue number plus state transition, review id, or comment id. Re-polling
  the overlap window must never produce duplicate rows.
- `pr_merged` and `pr_closed` are distinct kinds; a merged PR yields only `pr_merged`.
- Every fetched event is stored, whoever the actor is — there is no ingestion filter (see
  "Filter"; `list_events`'s `mode` is where that scoping happens now, at read time).
  Team membership is not stored on events: `team_ids` is resolved on every read, so editing
  a team re-categorises the feed immediately and never backfills. The actor of a commit is the GitHub `author.login`
  (fall back to `committer.login`, then to the git author name with `actor_avatar_url` null).
- Pagination: follow `Link: rel="next"` up to 10 pages per endpoint per poll.
- Conditional requests: each list endpoint sends `If-None-Match` with the ETag stored for it
  (schema v7, `etags(url TEXT PRIMARY KEY, etag TEXT NOT NULL, fetched_at INTEGER NOT NULL)`).
  GitHub answers `304 Not Modified` with an empty body when nothing changed, and does not
  charge a 304 against the rate limit. A 304 listing contributes no events. Rows older than
  30 days are swept when the store opens. See "Conditional requests" below.
- Auth: `Authorization: Bearer <token>`, `Accept: application/vnd.github+json`,
  `X-GitHub-Api-Version: 2022-11-28`, `User-Agent: Vigie/<crate version> (macOS)`. A 401 is `auth`, 404 is
  `not_found`, 403/429 with `x-ratelimit-remaining: 0` is `rate_limited` with `reset_at`
  from `x-ratelimit-reset`.
- On any repo error, that repo's watermark does not advance, `last_error` is set, and the
  other repos still poll.

### Conditional requests

An ETag identifies one exact URL, query string included — and every poll's URL carries a
`since=` that moves with the watermark, so a cache keyed on the URL that goes on the wire
would miss every single time.

- **The stable-URL rule.** Each list endpoint's ETag is filed under a *stable* URL: the same
  request with its `since=` (and, for commits, `until=`) dropped. That URL is a key and
  nothing else — it is never fetched, and dropping `since` from the request itself would
  change what GitHub returns. Example: the request
  `/repos/acme/platform/issues/comments?sort=updated&direction=desc&per_page=100&since=2026-09-07T10:00:00Z`
  files its tag under
  `/repos/acme/platform/issues/comments?sort=updated&direction=desc&per_page=100`
  (resolved against the API base URL), because everything before `since` is what makes the
  question the same question from one poll to the next.
- The key is sound because an ETag is a hash of the response body. In the steady state — a
  quiet repo polled again one interval later — the body for the old `since` and the body for
  the new one are the same bytes, so GitHub answers 304. When something did change, the
  bodies differ and GitHub answers 200 with the new one. The tag decides whether the answer
  was free, never what the answer is.
- That reasoning holds only while `since` is the ordinary watermark-minus-300s. **Conditional
  requests are sent only in that steady state**: a first poll (`now - 24h`), the first poll
  after a `rescan`, and every backfill window send no tag at all and simply store whatever
  comes back.
- The `/pulls` listing carries no `since`, so its stable URL is its request URL. Per-PR
  `/pulls/{n}/reviews` is unconditional: one ETag row per PR is not worth keeping, and the
  PR listing above already 304s when nothing on any PR moved.
- Only page one of a paginated listing is asked conditionally, and only page one's ETag is
  kept — the `rel="next"` pages are different URLs whose tags nothing stored. A 304 on page
  one short-circuits the whole listing: page two is never requested.
- `x-ratelimit-remaining` is read from every response **except** a 304, alongside
  `x-ratelimit-limit` and `x-ratelimit-reset` off that same response (`github::RateLimitReading`)
  — never only on the error path, so the status bar's meter has a reset time and a budget size
  on the ordinary happy path, not only once a repo has already hit the limit. GitHub does not
  charge for a 304, and the headers it sends beside one describe a budget the request never
  spent, so recording them would overwrite the live reading with a stale one.
- `rescan` clears the cached tags for the repos it clears the watermark of (all of them when
  `repo_id` is null). A rescan exists to refetch, and a tag GitHub still matches would turn
  that refetch into an empty 304.

### Poll interval

GitHub's `X-Poll-Interval: <seconds>` is a floor on how often one repo may be asked about.

- The largest hint seen across a repo's endpoints in one poll is stored in
  `repos.poll_interval_secs` (schema v7, `ALTER TABLE repos ADD COLUMN poll_interval_secs
  INTEGER`), replacing whatever was there. A hint is advice about *now*: keeping the
  high-water mark of every hint ever seen would let one spike throttle a repo for good.
  NULL — no hint yet — means no floor. This is not the user's own
  `Settings.poll_interval_secs`, which is how often the app polls at all.
- A poll skips any repo whose `last_polled_at + poll_interval_secs > now` and names it in
  `PollResult.skipped`. A skip is not a failure: nothing is fetched, the watermark does not
  move, and `last_error` is left exactly as it was.
- `PollOptions.force` ignores the floor entirely and is what a hand-driven "Poll now" passes;
  a forced poll always reports `skipped: []`. The background timer leaves it false.
  (Not yet wired through the Tauri `poll_now` command, which still takes no argument and so
  always polls unforced.)

### Backfill

The feed keeps going into the past on demand. Each repo carries a second cursor,
`repos.backfilled_to` (schema v6, `ALTER TABLE repos ADD COLUMN backfilled_to INTEGER`): the
oldest instant that repo's stored history covers — the bottom of the contiguous run of windows
fetched down from the watermark.

- The cursor is `null` until a repo's first *successful* poll, which seeds it with that poll's
  own window start (`now - 24h`). Later polls leave it alone: they fetch a newer, narrower
  window, so letting one rewrite the cursor would erase whatever backfill had reached.
- `backfill(repo_id, span_secs)` steps the cursor down by one span per call: it fetches the
  half-open window `[backfilled_to - span, backfilled_to)` and, on success, records the new
  floor. Calling it repeatedly walks steadily backwards with no gaps and no re-reading.
  `span_secs` is clamped to `[1h, 90d]` rather than rejected; the app's default is 7 days.
- A repo with a `null` cursor is reported, not guessed at: its row carries
  `error: "not polled yet"` with `from` and `to` both 0, and nothing about the repo changes.
  Likewise `"signed out"` when its account has no token this session.
- The window is `[from, to)` — half-open, so two adjacent steps share an edge and each claims
  it exactly once. Commits are bounded on the wire with GitHub's `since`/`until`; PRs, issues
  and comments carry `since = from` only and are filtered to the window client-side.
- Backfilled events are inserted **already seen**. They never count towards the unseen badge
  and never raise a notification: it is history the user scrolled back for, not news.
- Backfill never touches `last_polled_at`, and never runs auto-watch. It applies the same
  ingestion filter a poll does.
- Any repo's failure lands in that repo's `error` and does not stop the others; a failed step
  leaves its cursor where it was, so the same window is retried rather than skipped.
- The ten-page cap still applies. A window that hits it with more pages on offer reports
  `error: "window truncated"` and **still advances the cursor** — re-reading the same busy
  window would hit the same cap for ever and the feed would never get past it.
- `rescan` clears `backfilled_to` alongside `last_polled_at`: it restarts the contiguous run
  from a fresh 24-hour window, so a floor left behind would claim coverage of a stretch the
  repo has stopped tracking. The next successful poll re-seeds it, and the dedupe index means
  the history already stored is not lost — only re-reachable.

### Hiding a repo

The middle state between watching a repo and removing it: `hidden` stops it costing anything
without throwing away what it has already collected. `remove_repo` remains the destructive
one and is unchanged — it still cascades the repo's events, pulls and watches away for good.

- **Out of every read.** A hidden repo contributes nothing to `list_events` (including an
  explicit `repo_id` naming it), `digest`, `open_pulls`, `list_watches`, `unseen_count`, the
  people suggestions' activity ranking, or `list_repos` — and therefore nothing to the app's
  repo filter, whose options are `list_repos`. Its rows stay in the database untouched;
  nothing is deleted and nothing is rewritten.
- **Out of every poll.** `poll_now` and `backfill(None)` both iterate `list_repos`, so a
  hidden repo is never fetched and spends no request. An explicit `backfill(repo_id)` on one
  reports `"hidden"` in its `RepoBackfill.error` rather than spending the budget. Its
  `last_polled_at` simply stops advancing — nothing polls it, so nothing advances it.
- **Still watched.** `add_repo` on a hidden repo returns the existing row (still hidden), and
  `suggest_repos` therefore excludes hidden repos from what it offers, exactly as it excludes
  visible ones.
- **Unhiding restores everything**, with no refetch: the stored events, pulls and watches are
  all back in every read the moment the flag flips, and the repo polls again on the next cycle.
- **The resume window is capped.** Because the watermark did not advance while hidden, an
  uncapped resume would fetch `now - last_polled_at` in one go — and that costs one request
  *per pull request updated in the window*, not one request, so a repo hidden for a month
  could cost hundreds the instant it came back. So unhiding a repo whose watermark is older
  than the 24 h first-poll lookback clears the watermark, the backfill floor and the stale
  `last_error`, exactly as `rescan` does: the next poll costs one ordinary first-poll window
  whatever the gap was, dedupe keeps the overlap from duplicating anything, and `backfill`
  can walk deliberately back down into the gap a step at a time. A repo unhidden within the
  day keeps its watermark and simply resumes.
- Schema v9 adds `repos.hidden INTEGER NOT NULL DEFAULT 0` — additive and idempotent; every
  repo in an upgraded database reads back visible.

## Accounts

Schema v5 adds `accounts(login TEXT PRIMARY KEY, avatar_url, added_at)` and
`repos.account_login TEXT NOT NULL DEFAULT ''` (an empty string means "not yet claimed"). Additive.

- Tokens never touch the database. The app keeps one keychain item per account
  (`service dev.vigie.app`, `account <login>`) and hands them to the engine at startup with
  `set_account_token`; the engine keeps them in memory only, keyed by login.
- `add_account(token)` calls `GET /user` with that token, upserts the `accounts` row, remembers
  the token, and claims every repo whose `account_login` is empty (the pre-v5 install's repos).
  Adding an account that already exists replaces its token and returns the row.
- Each repo is polled with its own account's token; a repo whose account has no token this
  session gets `last_error = "signed out"` and is skipped, never dropped. Rate limits are
  per account, so `PollResult.rate_limit_remaining` reports the lowest across accounts —
  `rate_limit_reset_at` and `rate_limit_limit` come from that *same* account's own last
  response, never mixed with another's — and `RepoError.reset_at` is the failing repo's
  account's reset time.
- The signed-in logins for auto-watch are every account's login; `Watch.source` is
  `author`/`reviewer` whichever account matched.
- Two distinct app actions on an account, not to be confused: "Sign out" (`sign_out_account`)
  deletes that account's keychain item and calls `forget_account_token`, dropping only the
  in-memory token for this session — the account row stays listed, and its repos poll-skip as
  "signed out" until it signs back in. "Remove account" (`remove_account`) deletes the account
  and cascades its repos, events and watches; only it touches the database.
- `suggest_people` searches org members with the token of the repo's account; results are
  merged and deduped across accounts.
- Legacy: the pre-v5 keychain item (`account github-token`) is migrated by the app at first
  launch: it calls `add_account(token)` once, stores the token under the login it returns, and
  deletes the old item. `set_token`/`verify_token` stay for that step and for tests.

## Teams storage

Schema v3: `teams(id INTEGER PRIMARY KEY, name TEXT NOT NULL, position INTEGER NOT NULL)` and
`team_logins(team_id, login, position, UNIQUE(team_id, login))` replacing the single-team
`team_logins` and `settings.team_name`. Migration from v2 moves the existing team (name and
logins, in order) into `teams` as id 1 at position 0. A fresh database starts with no teams;
the app's onboarding creates the first one. `Team::default()` is gone.

## Digests

`digest(start, end, team_id, actor, tz_offset_secs)` summarises the events already stored for
the half-open window `[start, end)` in unix seconds; the app computes the window from the
user's local calendar (today, this week starting Monday, this month, or a custom range) and
passes timestamps. `team_id` keeps events whose actor is currently in that team; `actor` keeps
one login; both may be given. `invalid` when `end <= start`. A digest is computed from the
store only, never from GitHub, so it covers what Vigie has watched: nothing before a repo was
added, and nothing the ingestion filter dropped. `DigestThread.kind` is `"pull"` for PR event
kinds and `"issue"` for issue kinds; commits have no thread and count only in `totals`,
`people` and `repos`. `title` and `url` are those of the newest event in the thread.

`tz_offset_secs` is the app's own UTC offset and decides one thing only: where a *day* starts,
and so which bucket of `series` and which cell of `hours` an event falls in. Every other number
in the digest is unaffected by it.

- **`series`** is one `DayCounts` per local day the window touches, oldest first, quiet days
  present and zeroed so a chart has no gaps. `day` is the unix second that local day began.
  The window is half-open here too: one ending exactly at local midnight covers the day before
  it. Capped at 3660 entries (ten years), which no period the app offers approaches.
- **`hours`** is always seven rows of twenty-four: `hours[weekday][hour]`, weekday Monday = 0,
  hour in local time. `series` and `hours` are folded from the same buckets, so both always sum
  to `totals`.
- **`pr_timing`** is medians and p90s over the same window. *Time to merge* runs from a PR's
  `pr_opened` event to its `pr_merged`; *time to first review* from `pr_opened` to the earliest
  `pr_reviewed` on the same `(repo_id, number)`. A PR counts when the **closing** event — the
  merge, or that first review — falls inside the window, so a window reports what finished in
  it; a PR whose `pr_opened` was never stored is measured by neither. `team_id`/`actor` scope
  these by the PR's **author**, not by whoever merged or reviewed. Every figure is `null` when
  nothing qualified (never `0`, which would read as "instant"); an even sample count averages
  the two middle values. `samples` counts the distinct PRs that contributed either measurement.
- **`pr_timing.p90_ttm_secs` / `p90_ttfr_secs`** are the 90th percentile of the *same* sample
  sets their medians describe — the slow tail beside the typical case. Both use **R-7 linear
  interpolation** (numpy, pandas and Excel's `PERCENTILE.INC`): the answer sits at rank
  `0.9 * (n - 1)` on the ascending samples and is interpolated between the two closest ranks,
  rounded up to the whole second. Interpolating rather than taking the nearest rank is what
  makes the number mean anything on a small set — nearest-rank returns the largest sample for
  every `n` below ten, so a week with three merges would report its slowest merge as its p90.
  `p90 >= median` always. One sample makes the p90 that sample; each p90 is `null` under
  exactly the condition its median is.
- **`unreviewed_merges`** counts the pull requests merged inside the window for which the store
  holds **no** `pr_reviewed` event at any time up to that merge — what shipped with nobody's
  review on it. The check looks at all of history up to the merge instant, not just the window,
  so a PR reviewed last month and merged this week is reviewed; a PR reviewed only *after* it
  was merged is not. `team_id`/`actor` scope it by the PR's **author**, as `pr_timing` does; an
  unscoped digest counts every merge in the window, while a scoped one can only count merges
  whose `pr_opened` is stored, because nothing else names the author. **It sees only what Vigie
  stored**: a repo added after a review happened holds no `pr_reviewed` for that PR, so its
  later merge reads as unreviewed — the same caveat `review_decision` carries, and the reason
  this number is a prompt to look rather than an audit.
- **`repos[].top_author_login` and `repos[].top_author_share`** are that repo's busiest commit
  author in the window and their fraction of its commits, in `0..1` — a repo at `0.9` is one
  person's. **Commits only**: reviews, comments and issues raise `total` and leave the share
  alone, and a repo whose window holds no commits reports `null` and `0.0` rather than
  promoting a reviewer to author. Ties go to the lowest login, so the answer is the same every
  time it is asked. Scoped by `team_id`/`actor` like the rest of the window's counts, which
  means a digest narrowed to one person shows every repo at `1.0`.
- **`people[].open_prs` and `people[].review_queue`** come from the `pulls` table below and
  describe **now**, not the window — a queue is a call to action, and a stale one would be
  worse than none. `people[].last_at` and everything else about a person is the window's.
  `people_total` counts every distinct person active in the window, before the top-20 cut.

## Pulls

Schema v8 adds `pulls(repo_id REFERENCES repos ON DELETE CASCADE, number, title, url,
author_login, author_avatar_url, state CHECK (state IN ('open','closed','merged')), draft,
created_at, updated_at, merged_at, closed_at, additions, deletions, requested_reviewers,
review_decision, last_activity_at, PRIMARY KEY (repo_id, number))`, plus an index on
`(state, repo_id)`. Additive; nothing else changes, and nothing is back-filled.

- **Filled in by polling, at no extra cost.** Every PR the poll's own `/pulls` listing returns
  is upserted, and the backfill path writes them too. There is no new GitHub request anywhere:
  the table records what the poller was already told. A row is *current state*, not history, so
  a later poll replaces it wholesale — except `additions`/`deletions`, which the list shape
  omits and which a re-list therefore must not wipe.
- `state` is `merged` for a PR with a `merged_at`, never `closed` — the same rule `Watch.state`
  and `pr_merged` follow. `requested_reviewers` is a JSON array of logins.
- **`review_decision`** is derived on write from the stored `pr_reviewed` events for that
  `(repo_id, number)`: the newest review *per reviewer* decides that reviewer's position, then
  any `changes_requested` gives `changes_requested`, else any `approved` gives `approved`, else
  `review_required` when somebody has been asked, else `null`. A review's state is carried by
  the event `title` (`Review: approved` / `Review: changes requested` / `Review: commented`),
  so a plain comment is neither an approval nor a block. Events are stored before the PR rows
  are written, so a review arriving with a poll is reflected in the same poll.
- **`last_activity_at`** is the later of GitHub's `updated_at` and the newest stored event on
  that PR.
- `open_pulls(team_id, actor, reviewer)` returns the `open` rows, `created_at` ascending.
  `team_id` keeps PRs whose *author* is currently in that team (an unknown team matches nobody,
  quietly, as in `digest`), `actor` keeps one author, `reviewer` keeps the PRs currently
  requesting one login's review; given together they intersect, and all three ignore casing.
- Removing a repo — or an account, which removes its repos — deletes its pull rows.

## Watched threads

Schema v4 adds `watches(repo_id REFERENCES repos ON DELETE CASCADE, number, kind, title, state,
source, since, PRIMARY KEY (repo_id, number))`. Additive; nothing else changes.

- `watch_thread(repo_id, number)` records a manual watch. `kind`, `title` and `state` come from
  the stored events for that thread when any exist, otherwise from one live `get_thread`.
  Watching an already-watched thread returns the existing row unchanged (its `source` and
  `since` stay). `unwatch_thread` deletes the row; `not_found` if there is none.
- **Auto-watch** (when `settings.auto_watch`): during a poll, a PR whose `user.login` is the
  signed-in login is watched with source `author`; a PR whose `requested_reviewers[]` contains
  the signed-in login is watched with source `reviewer`. The engine remembers the signed-in
  login in memory after `verify_token` or a successful `poll_device_login`; with no login
  known, auto-watch does nothing. Auto-watch never overwrites an existing row.
- **Ingestion**: an event whose `(repo_id, number)` is watched is stored even when the filter
  would drop its actor. The check uses the watches present at the start of the poll plus any
  added by auto-watch during the same poll. Commits (no `number`) are never watched.
- **State**: on every poll, for each watched thread seen in the PR or issue listing, the row's
  `state` and `title` are refreshed. Merged or closed threads stay watched until unwatched;
  the app offers "Clear closed" (unwatch every row whose state is not `open`).
- **Read time**: `Event.watched` is resolved on every read like `team_ids`, so unwatching
  un-marks the feed immediately. `list_events(watched_only = true)` keeps only watched rows.
- **Notifications** (app side): a new event with `watched: true` always raises a notification,
  whatever `notify_kinds` says; `notifications_enabled` and quiet hours still apply, and the
  more-than-10 summary collapse still applies. The feed marks watched rows with an eye glyph
  and the accent tint so they stand out from ordinary rows.

## Reading in-app

Nothing the user needs to read requires leaving the app. The detail pane for a PR has three
tabs: Conversation (`Thread.items`), Commits (the `commit` items; clicking one opens
`get_commit`), and Files (`get_pull_files`, each file's `patch` rendered as a unified diff
with additions and deletions tinted; `patch` null shows "too large to show here" with the
GitHub link). `get_pull_files` is `GET /repos/{o}/{r}/pulls/{n}/files`, following `Link`
up to 10 pages, cached 60 seconds like the other reads. `get_thread` fetches, live, the PR
or issue (`GET /repos/{o}/{r}/pulls/{n}` or `/issues/{n}`), its issue comments, and for a PR
its reviews and review comments, and merges them into one chronological `items` list.
`get_commit` fetches `GET /repos/{o}/{r}/commits/{sha}` including per-file patches (patch may be
null for binary or very large files). Both are cached in memory for 60 seconds keyed by
their arguments. "Open on GitHub" in the app is a secondary link, never the only way to read.

## Sign-in (GitHub OAuth device flow, rendered in-app)

Vigie is a public native client, so it signs in with the OAuth **device authorization flow**:
no client secret, no local web server. The GitHub OAuth App (registered by the owner, device
flow enabled) is identified by a client ID compiled into the app.

- `start_device_login(client_id)` POSTs `https://github.com/login/device/code` with
  `client_id` and `scope=repo read:org read:user` (`Accept: application/json`) and returns
  the response as `DeviceLogin`.
- `poll_device_login(client_id, device_code)` POSTs `https://github.com/login/oauth/access_token`
  with `client_id`, `device_code`, `grant_type=urn:ietf:params:oauth:grant-type:device_code`.
  `authorization_pending` and `slow_down` return `"pending"` (the caller waits `interval`
  seconds, plus 5 on `slow_down`); `expired_token` and `access_denied` are `auth` errors with
  the GitHub message; success sets the token on the engine, calls `/user`, and returns
  `{token, login}`. The token is a classic OAuth token (`gho_…`); the app stores it in the
  keychain exactly as it stored a PAT. A keychain save failure does not fail the sign-in:
  the token is live on the engine for the session, and the app emits `token-save-failed`
  (payload: `{message}`) so the UI can warn that the sign-in won't survive a relaunch.
- The app does NOT open the system browser for sign-in. It opens a Vigie-owned window on
  `verification_uri_complete` (GitHub returns it alongside `verification_uri`; the code is
  prefilled) so the user approves inside the app, then closes that window on `ok`.
- That window is a `WKWebView`, which exposes the WebAuthn API but has no authenticator behind
  it, so a passkey cannot complete there: beside the user code the app offers "Use Safari
  instead", which opens the same verification URL in the default browser via
  `open_signin_in_browser` while the poll loop keeps running, so either window can finish the
  sign-in. The hand-off is also automatic — when the in-app window navigates to a GitHub URL
  containing `webauthn` or `passkey`, the app opens that URL in the default browser and emits
  `passkey-handoff` (no payload) so the UI can note "Passkeys need Safari; finish signing in
  there."
- There is no personal-access-token path in the app. `engine.verify_token()` exists only so
  the device flow can resolve the login after a token is obtained; the app never exposes a
  token field.
- The engine never persists the token, the device code, or the client ID.

## People suggestions

`suggest_people(query, limit)` searches, case-insensitively by login prefix then substring:
1. the distinct `actor_login`s already stored in `events` for every watched repo (these are
   the contributors: commit authors, reviewers, commenters), ranked by event count, with
   `why` = "<n> <most common kind>s · <repo with most events>";
2. members of each watched repo's owning org (`GET /orgs/{owner}/members`, cached for one
   hour in memory, skipped when the owner is a user or the token lacks `read:org`), with
   `why` = "member of <org>";
3. logins ending in `[bot]` are `source: "bot"` and sorted last.
An empty query returns the top `limit` by the same ranking. With `team_id`, results exclude
logins already in that team; without it nothing is excluded. This is the team-*building* flow
(Teams' "add a member" search).

`suggest_active_people(query, team_id?, limit)` ranks and searches identically, but `team_id`
means the opposite: given, it keeps *only* logins already in that team (an id matching no team,
or a team with no members, yields no results — the same "narrows to the population" sense
`digest`'s `team_id` has); without it nothing is restricted, same as `suggest_people`. This is
for narrowing a search *to* a team scope rather than filling one in — e.g. Summary's person
filter passing its page-level team scope, where `suggest_people`'s exclusion would hide exactly
the people being searched for.

## Tauri commands (app side; JSON mirrors the types above)

```
start_device_login()                -> DeviceLogin            // client_id comes from the app's build config
poll_device_login(device_code)      -> DeviceLoginStatus      // on ok, add_account(token) and store the token in the keychain under its login; a save failure emits token-save-failed instead of failing
list_accounts() / remove_account(login)                        // remove_account cascades: deletes the account, its repos, events and watches, and its keychain item
sign_out_account(login)             // "sign out": deletes that login's keychain item and forget_account_token(login); the account row and its repos are untouched
sign_out()                          // removes every account: keychain items and engine tokens
load_token()                        -> string|null            // from keychain; sets it on the engine
clear_token()
current_login()                     -> string|null            // the login the engine resolved; null when signed out
list_teams() / create_team(name, logins) / update_team(team) / delete_team(team_id)
reorder_teams(ids) / import_org_team(org, team_slug)
suggest_people(query, team_id?, limit)
suggest_active_people(query, team_id?, limit)   // suggest_people's mirror image: team_id restricts TO that team instead of excluding it
add_repo(spec, account_login?) / remove_repo(repo_id) / list_repos()   // list_repos leaves hidden repos out
list_hidden_repos() / set_repo_hidden(repo_id, hidden)       // see "Hiding a repo"
suggest_repos(account_login?)
poll_now()                          -> PollResult
backfill(repo_id?, span_secs?)      -> BackfillResult         // span_secs defaults to 7 days; the app calls it when the feed reaches the end of its history
rescan(repo_id?)                                             // the app calls it after onboarding, and from a "Refetch last 24 h" action in Repos
list_events(repo_id?, actor?, team_id?, mode, watched_only, before_id?, limit)
get_thread(repo_id, number) / get_commit(repo_id, sha) / get_pull_files(repo_id, number)
list_watches() / watch_thread(repo_id, number) / unwatch_thread(repo_id, number)
digest(start, end, team_id?, actor?, tz_offset_secs)   // tz_offset_secs is the webview's own UTC offset
open_pulls(team_id?, actor?, reviewer?)   -> OpenPull[]      // open PRs, oldest first; from the store, never GitHub
mark_seen(ids) / unseen_count()
get_settings() / set_settings(settings)
```

Errors cross IPC as the `EngineError` JSON shape. The app runs the poll timer on the Rust
side (tokio interval driven by `poll_interval_secs`), emits a `new-events` event to the
webview with each non-empty `PollResult`, and raises one OS notification per new event
whose kind is in `notify_kinds`, unless notifications are disabled or now is inside
quiet hours. More than 10 new events in one poll collapse into a single summary
notification.

Notification text is plain (macOS allows no styling), so structure carries the meaning.
Title: `<actor> <verb> on <owner/name> #<number>` (verbs: opened, merged, closed, reviewed,
commented on; commits: `<actor> pushed to <owner/name>`). Body line 1: the PR or issue
title (commits: the first message line and the short sha). Body line 2, only when the event
has text: the `body_preview` in curly quotes, cut at 90 characters with an ellipsis. Watched
events prefix the title with `Watched:`. The summary notification stays `<n> new events`.

## Bloat budget (hard acceptance criteria for the app)

- Tauri 2, system WebView, no Electron, no bundled browser.
- One fixed look, "Cool sage": the app ignores the system light/dark setting. Tokens live in
  `docs/design/` (the agreed mockups): surfaces #f6f8f5 and #e9eee7, text #1f2a22, muted
  #65746a, accent #2f7a4f, dividers #e3e9e0, borders #d5ddd2.
- Frontend: Vite plus vanilla TypeScript or Svelte 5. No React, no component library,
  no CSS framework, no icon font. Inline SVG icons only.
- Release `.app` bundle under 15 MB. Idle RSS under 120 MB with the window closed,
  measured with `ps -o rss` on the main process after five minutes.
