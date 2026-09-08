// TS mirrors of docs/CONTRACT.md "Types". Keep field names snake_case to
// match the JSON the Rust side sends over IPC verbatim — no case mapping.

import { clockTime } from "./time";

export type FilterMode = "all" | "team";

export type EventKind =
  | "commit"
  | "pr_opened"
  | "pr_merged"
  | "pr_closed"
  | "pr_reviewed"
  | "pr_commented"
  | "issue_opened"
  | "issue_commented";

export const ALL_EVENT_KINDS: EventKind[] = [
  "commit",
  "pr_opened",
  "pr_merged",
  "pr_closed",
  "pr_reviewed",
  "pr_commented",
  "issue_opened",
  "issue_commented",
];

export interface Team {
  /** 0 only for a team that has not been saved yet (`import_org_team`). */
  id: number;
  name: string;
  logins: string[];
}

/** The id the engine gives a team that exists only in memory. */
export const UNSAVED_TEAM_ID = 0;

export interface Repo {
  id: number;
  owner: string;
  name: string;
  url: string;
  /** The account whose token polls this repo; empty when not yet claimed
   * (a pre-v5 install's repos, until the first account claims them). */
  account_login: string;
  default_branch: string;
  last_polled_at: number | null;
  last_error: string | null;
  /** Hidden repos are out of every view and out of every poll, so they cost
   * no GitHub requests — but nothing they collected is deleted, and unhiding
   * brings all of it straight back. Not the same as removing (docs/CONTRACT.md
   * "Hiding a repo"): `remove_repo` cascades the history away for good. */
  hidden: boolean;
}

/** A signed-in GitHub login with its own token (docs/CONTRACT.md
 * "Accounts") — an install may have several, each polling its own repos. */
export interface Account {
  login: string;
  avatar_url: string | null;
  added_at: number;
}

export interface Event {
  id: number;
  repo_id: number;
  kind: EventKind;
  actor_login: string;
  actor_avatar_url: string | null;
  title: string;
  body_preview: string | null;
  /** The full markdown body, untruncated; the app renders it in place. */
  body: string | null;
  url: string;
  number: number | null;
  occurred_at: number;
  seen: boolean;
  /** Teams the actor is in *right now* — resolved on every read, never stored. */
  team_ids: number[];
  /** Whether (repo_id, number) is a currently watched thread. Always false for a commit. */
  watched: boolean;
}

export interface RepoError {
  repo_id: number;
  message: string;
  /** Unix seconds at which the budget refills. Only ever set when the failure
   * was `rate_limited`; `null` for every other kind. */
  reset_at: number | null;
}

export interface PollResult {
  new_events: Event[];
  errors: RepoError[];
  rate_limit_remaining: number | null;
  /** Unix seconds at which `rate_limit_remaining`'s budget refills — the same
   * account's `x-ratelimit-reset`. `null` when that account's last response
   * carried no reset header, or when the poll used no account. */
  rate_limit_reset_at: number | null;
  /** The size of that budget (`x-ratelimit-limit`). `null` when the response
   * carried no limit header — treat that as GitHub's documented default
   * (5,000/hour), not as "unknown". */
  rate_limit_limit: number | null;
}

/** One repo's share of a `backfill` step (engine `RepoBackfill`): the window
 * it reached for and what came of it. A repo that could not be backfilled
 * still gets a row — `inserted` 0 and `error` set — so the caller can tell
 * "nothing was there" from "we never looked". `from`/`to` are both 0 for a
 * repo that has never polled, the one case where no window was chosen at all. */
export interface RepoBackfill {
  repo_id: number;
  /** The window's inclusive lower bound, and the repo's new backfill floor
   * once the step succeeds. */
  from: number;
  /** The window's exclusive upper bound — the floor this step started from. */
  to: number;
  /** Rows this step actually added; re-running an overlapping window inserts
   * nothing. */
  inserted: number;
  /** `null` on a clean step. `"not polled yet"` when the repo has no floor to
   * step down from, `"signed out"` when no token can authenticate it,
   * `"window truncated"` when the page cap cut the fetch short (the cursor
   * still advances), and otherwise the engine error the fetch failed with. */
  error: string | null;
}

/** The outcome of one `backfill` call across the repos it was aimed at. */
export interface BackfillResult {
  repos: RepoBackfill[];
  rate_limit_remaining: number | null;
  /** Same meaning as `PollResult.rate_limit_reset_at`. */
  rate_limit_reset_at: number | null;
  /** Same meaning as `PollResult.rate_limit_limit`. */
  rate_limit_limit: number | null;
}

export interface QuietHours {
  start_minute: number;
  end_minute: number;
}

export interface Settings {
  poll_interval_secs: number;
  filter_mode: FilterMode;
  notifications_enabled: boolean;
  notify_kinds: EventKind[];
  quiet_hours: QuietHours | null;
  /** Watch the signed-in user's own PRs and the ones they were asked to review. */
  auto_watch: boolean;
}

export type PersonSuggestionSource = "contributor" | "org_member" | "bot";

export interface PersonSuggestion {
  login: string;
  avatar_url: string | null;
  source: PersonSuggestionSource;
  why: string;
}

export interface RepoSuggestion {
  owner: string;
  name: string;
  why: string;
}

export interface DeviceLogin {
  device_code: string;
  user_code: string;
  verification_uri: string;
  verification_uri_complete: string | null;
  expires_in: number;
  interval: number;
}

export type DeviceLoginStatus = { status: "pending" } | { status: "ok"; token: string; login: string };

export type ThreadKind = "pull" | "issue";
export type ThreadItemKind = "comment" | "review" | "review_comment" | "commit";

export interface ThreadItem {
  kind: ThreadItemKind;
  actor_login: string;
  actor_avatar_url: string | null;
  body: string | null;
  state: string | null;
  path: string | null;
  line: number | null;
  url: string;
  at: number;
  /** The commit sha, on `commit` items only; null on every other kind. */
  sha: string | null;
}

export type WatchSource = "manual" | "author" | "reviewer";

/** A PR or issue the user follows (docs/CONTRACT.md "Watched threads").
 * `state` is "as last seen" — refreshed by each poll that sees the thread in
 * a listing, not live. */
export interface Watch {
  repo_id: number;
  number: number;
  kind: ThreadKind;
  title: string;
  state: string;
  source: WatchSource;
  since: number;
}

export interface Thread {
  repo_id: number;
  number: number;
  kind: ThreadKind;
  title: string;
  state: string;
  author_login: string;
  author_avatar_url: string | null;
  body: string | null;
  url: string;
  created_at: number;
  head_ref: string | null;
  base_ref: string | null;
  additions: number | null;
  deletions: number | null;
  changed_files: number | null;
  items: ThreadItem[];
}

export interface CommitFile {
  path: string;
  status: string;
  additions: number;
  deletions: number;
  patch: string | null;
}

export interface CommitDetail {
  repo_id: number;
  sha: string;
  message: string;
  author_login: string;
  author_avatar_url: string | null;
  url: string;
  at: number;
  additions: number;
  deletions: number;
  files: CommitFile[];
}

/** Per-kind counts, keyed by the wire name, every kind present with zeros
 * included (docs/CONTRACT.md's `KindCounts`). */
export type KindCounts = Record<EventKind, number>;

export interface DigestPerson {
  login: string;
  avatar_url: string | null;
  total: number;
  counts: KindCounts;
  /** Currently open PRs they authored — a live count, not scoped to the
   * digest's window (docs/CONTRACT.md "Pulls"). */
  open_prs: number;
  /** Currently open PRs where they're a requested reviewer — same live,
   * unwindowed count as `open_prs`. */
  review_queue: number;
  /** Unix seconds of their most recent event inside the window — always
   * present: a `DigestPerson` only exists for someone with at least one
   * event in `[start, end)`. */
  last_at: number;
}

export interface DigestThread {
  repo_id: number;
  number: number;
  kind: ThreadKind;
  title: string;
  url: string;
  events: number;
  last_at: number;
}

export interface DigestRepo {
  repo_id: number;
  total: number;
  /** The login with the most `commit` events in this repo this window, or
   * `null` when the window holds no commits for this repo. */
  top_author_login: string | null;
  /** That login's share of this repo's commits, `0..1` — `0` when
   * `top_author_login` is `null`. A different population from `total`
   * (every event kind): do not mix them. */
  top_author_share: number;
}

/** One local day's counts within a digest window (docs/CONTRACT.md
 * "Digests"), bucketed using the `tz_offset_secs` the caller passed to
 * `digest`. */
export interface DayCounts {
  /** Unix seconds at local midnight for this day. */
  day: number;
  counts: KindCounts;
}

/** PR cycle-time medians for the window (docs/CONTRACT.md "Digests"), from
 * pr_opened -> first pr_reviewed -> pr_merged joined by (repo_id, number).
 * Either median is `null` when there weren't enough qualifying PRs to
 * compute it — `samples` is the PR count the medians were computed over,
 * not a per-field count, so a null median can still carry a non-zero
 * `samples` (e.g. every sampled PR merged but none had a review). */
export interface PrTiming {
  median_ttm_secs: number | null;
  median_ttfr_secs: number | null;
  /** 90th percentile over the same sample set as `median_ttm_secs` — where
   * the PR that sat nine days shows up. `null` under the same condition the
   * median is (no samples). */
  p90_ttm_secs: number | null;
  /** 90th percentile over the same sample set as `median_ttfr_secs`. */
  p90_ttfr_secs: number | null;
  samples: number;
}

/** A period summary of stored events (docs/CONTRACT.md "Digests"), computed
 * from the store alone over the half-open window `[start, end)`. */
export interface Digest {
  start: number;
  end: number;
  team_id: number | null;
  actor: string | null;
  totals: KindCounts;
  /** By total desc, then login; top 20. */
  people: DigestPerson[];
  /** Every person the totals above were rolled up from, before the top-20
   * clamp — lets the roster tell "20 of 20" apart from "20 of 60". */
  people_total: number;
  /** By events desc, then last_at desc; top 10. */
  threads: DigestThread[];
  /** Every watched repo with a non-zero total, by total desc. */
  repos: DigestRepo[];
  /** One entry per local day in `[start, end)`, local-time bucketed via the
   * `tz_offset_secs` passed to `digest`. */
  series: DayCounts[];
  /** Weekday x hour heatmap of event times, local time — `hours[0]` is
   * Monday, `hours[d][h]` is the event count for that weekday/hour. */
  hours: number[][];
  pr_timing: PrTiming;
  /** PRs merged inside the window with no `pr_reviewed` event stored for
   * them at any time up to the merge, scoped by the PR's **author** (same
   * scoping as `pr_timing`). Sees only what the store holds: a repo added
   * after a review happened reads as unreviewed — a prompt to look, not an
   * audit. */
  unreviewed_merges: number;
}

/** Every state GitHub's review-decision can settle on for an open PR
 * (docs/CONTRACT.md "Pulls") — `null` covers "no reviews yet". */
export type ReviewDecision = "approved" | "changes_requested" | "review_required" | null;

/** A currently-open pull request (docs/CONTRACT.md "Pulls") — a live
 * snapshot from the `pulls` table, not scoped to any digest window. */
export interface OpenPull {
  repo_id: number;
  number: number;
  title: string;
  url: string;
  author_login: string;
  author_avatar_url: string | null;
  draft: boolean;
  created_at: number;
  updated_at: number;
  last_activity_at: number;
  additions: number | null;
  deletions: number | null;
  requested_reviewers: string[];
  review_decision: ReviewDecision;
}

export type EngineErrorKind =
  | "auth"
  | "not_found"
  | "rate_limited"
  | "network"
  | "storage"
  | "invalid";

export interface EngineError {
  kind: EngineErrorKind;
  message: string;
  reset_at: number | null;
}

/** Type guard for errors that cross IPC as the EngineError JSON shape. */
export function isEngineError(e: unknown): e is EngineError {
  return (
    typeof e === "object" &&
    e !== null &&
    "kind" in e &&
    "message" in e &&
    typeof (e as { kind: unknown }).kind === "string"
  );
}

/** Plain-English message for an EngineError, for inline error text. */
export function engineErrorMessage(e: EngineError): string {
  switch (e.kind) {
    case "auth":
      // The engine words the actionable cases itself — a token that needs
      // authorising for an organisation using single sign-on, or one that
      // simply cannot see the thing. Only fall back to "sign in again" when
      // it had nothing more specific to say.
      return e.message && e.message.trim().length > 0
        ? e.message
        : "GitHub rejected that sign-in. Sign in again to continue.";
    case "not_found":
      return "Not found. Double-check the name and that the token can see it.";
    case "rate_limited":
      return e.reset_at
        ? `Rate limited. Try again after ${clockTime(e.reset_at)}.`
        : "Rate limited. Try again in a bit.";
    case "network":
      return "Couldn't reach GitHub. Check your connection and try again.";
    case "storage":
      // The engine's own message names the failing subsystem (keychain
      // OSStatus, sqlite error, …) — far more useful than the canned text,
      // which survives only as the fallback.
      return e.message || "Local storage error. Try restarting the app.";
    case "invalid":
      return e.message || "That input isn't valid.";
    default:
      return e.message || "Something went wrong.";
  }
}
