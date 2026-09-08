// Pure logic for the Summary v2 page (Summary.svelte + components/summary/*):
// window math, KPI deltas, the engineer roster, PR-state grouping, and the
// rule-based "needs attention" list. Kept out of the .svelte files so it's
// unit-testable without mounting a component — see summary.test.ts.
import { ALL_EVENT_KINDS } from "./types";
import type { Digest, DigestPerson, DigestThread, EventKind, KindCounts, OpenPull, ReviewDecision } from "./types";

// --- kind-mix presentation (shared by the activity-mix bar, the engineer
// focus per-day stacked bar, and the roster sparkline hover labels — one
// definition instead of three copies of the same palette/wording). ---

/** Bar-fill colour per event kind. Pulled from the exact CSS vars
 * KindBadge.svelte already uses (its `fg` tone — more legible than `bg` at
 * a thin bar height) so every stacked bar in the app draws from the one
 * kind palette instead of inventing another. */
export const KIND_BAR_COLOR: Record<EventKind, string> = {
  commit: "var(--badge-commit-fg)",
  pr_opened: "var(--badge-open-fg)",
  pr_merged: "var(--badge-merged-fg)",
  pr_closed: "var(--badge-closed-fg)",
  pr_reviewed: "var(--badge-review-fg)",
  pr_commented: "var(--badge-comment-fg)",
  issue_opened: "var(--badge-issue-fg)",
  issue_commented: "var(--badge-comment-fg)",
};

/** Human phrasing for a bar segment's hover tooltip / aria-label, e.g.
 * "12 commits" or "1 review". */
export const KIND_TOOLTIP: Record<EventKind, (n: number) => string> = {
  commit: (n) => `${n} commit${n === 1 ? "" : "s"}`,
  pr_opened: (n) => `${n} PR${n === 1 ? "" : "s"} opened`,
  pr_merged: (n) => `${n} PR${n === 1 ? "" : "s"} merged`,
  pr_closed: (n) => `${n} PR${n === 1 ? "" : "s"} closed`,
  pr_reviewed: (n) => `${n} review${n === 1 ? "" : "s"}`,
  pr_commented: (n) => `${n} PR comment${n === 1 ? "" : "s"}`,
  issue_opened: (n) => `${n} issue${n === 1 ? "" : "s"} opened`,
  issue_commented: (n) => `${n} issue comment${n === 1 ? "" : "s"}`,
};

/** Sum of every kind's count — the stacked bar's 100% denominator. */
export function totalOf(counts: KindCounts): number {
  return ALL_EVENT_KINDS.reduce((sum, k) => sum + counts[k], 0);
}

/** Non-zero kinds, in the fixed `ALL_EVENT_KINDS` order — which segments a
 * stacked bar actually needs to draw. */
export function presentKinds(counts: KindCounts): EventKind[] {
  return ALL_EVENT_KINDS.filter((k) => counts[k] > 0);
}

/** `n` as a percentage of `max` (0 when `max` is 0, never NaN/Infinity). */
export function pct(n: number, max: number): number {
  return max > 0 ? (n / max) * 100 : 0;
}

// --- window math ---

export interface Range {
  start: number;
  end: number;
}

/** The window immediately preceding `range`, of the same length — what the
 * KPI row's delta chips compare against. */
export function previousRange(range: Range): Range {
  const length = range.end - range.start;
  return { start: range.start - length, end: range.start };
}

/** `-new Date().getTimezoneOffset()*60` (docs/CONTRACT.md's `digest` arg) —
 * a function so tests can pass a fixed `Date` instead of the real clock. */
export function tzOffsetSecs(now: Date = new Date()): number {
  return -now.getTimezoneOffset() * 60;
}

// --- duration formatting (median time-to-merge / time-to-first-review) ---

/** `null` -> "—" (not enough samples); otherwise the coarsest two units that
 * fit ("2d 4h", "1h 12m", "45m", "<1m") — same rounding spirit as
 * `time.ts`'s relative-time helpers, but a plain duration, not "ago". */
export function formatDuration(secs: number | null): string {
  if (secs == null) return "—";
  const s = Math.max(0, Math.round(secs));
  if (s < 60) return "<1m";
  const mins = Math.round(s / 60);
  if (mins < 60) return `${mins}m`;
  const hours = Math.floor(mins / 60);
  const remMins = mins % 60;
  if (hours < 24) return remMins > 0 ? `${hours}h ${remMins}m` : `${hours}h`;
  const days = Math.floor(hours / 24);
  const remHours = hours % 24;
  return remHours > 0 ? `${days}d ${remHours}h` : `${days}d`;
}

/** Signed count for a delta chip: "+3", "-2", "±0". */
export function formatSignedCount(delta: number): string {
  if (delta === 0) return "±0";
  return delta > 0 ? `+${delta}` : `${delta}`;
}

/** Signed duration for a delta chip: "+1h 20m", "-45m", "±0". */
export function formatSignedDuration(deltaSecs: number): string {
  if (deltaSecs === 0) return "±0";
  return `${deltaSecs > 0 ? "+" : "-"}${formatDuration(Math.abs(deltaSecs))}`;
}

// --- KPI row ---

export type KpiKey = "openPrs" | "awaitingReview" | "stale" | "merged" | "medianTtm" | "medianTtfr";

export interface KpiTile {
  key: KpiKey;
  label: string;
  displayValue: string;
  /** A p90 reading beside the tile's median, labelled so it's obvious which
   * is which (e.g. "p90 9d 4h") — `null` for tiles with no median (the three
   * live gauges, `merged`) or when there aren't enough samples. The p90 is
   * where the PR that sat nine days shows up; the median alone hides it. */
  secondaryLabel: string | null;
  /** `null` when there's nothing to compare against (no previous-window
   * data, or the value itself is unavailable). */
  deltaLabel: string | null;
  /** `null` for a zero delta or when `deltaLabel` is null — "neutral", not
   * styled green or red. */
  deltaGood: boolean | null;
}

export const STALE_AFTER_SECS = 3 * 86_400;

/** Not draft, and nobody has reviewed it yet (or GitHub is waiting on
 * changes being re-requested) — the "awaiting review" KPI and roster
 * queue's shared predicate. */
export function isAwaitingReview(p: OpenPull): boolean {
  return !p.draft && (p.review_decision === null || p.review_decision === "review_required");
}

export function isStale(p: OpenPull, now: number): boolean {
  return now - p.last_activity_at > STALE_AFTER_SECS;
}

export interface PullGaugeCounts {
  open: number;
  awaitingReview: number;
  stale: number;
}

export function gaugeCountsNow(pulls: OpenPull[], now: number): PullGaugeCounts {
  return {
    open: pulls.length,
    awaitingReview: pulls.filter(isAwaitingReview).length,
    stale: pulls.filter((p) => isStale(p, now)).length,
  };
}

/** SHORTCUT: `openPulls` (docs/CONTRACT.md "Pulls") is a live snapshot with
 * no history behind it — there's no "open PRs as of a past moment" query in
 * the contract. This estimates the previous window's end-of-window gauge
 * from the SAME live snapshot's own per-PR timestamps: only counting PRs
 * that are *still open right now* toward "existed by `asOf`" necessarily
 * misses any PR that was open then but has since merged or closed, so every
 * field here is a floor, not an exact historical reading — same shape as
 * Summary.svelte's own `oldestKnownAt` shortcut. `awaitingReview` compounds
 * this further: `review_decision` is *today's* decision, which may not be
 * what it was `asOf` either. Good enough to sign a delta chip's direction;
 * not a substitute for a real pulls-history table. */
export function gaugeCountsAsOf(pulls: OpenPull[], asOf: number): PullGaugeCounts {
  const existedByThen = pulls.filter((p) => p.created_at < asOf);
  return {
    open: existedByThen.length,
    awaitingReview: existedByThen.filter(isAwaitingReview).length,
    stale: existedByThen.filter((p) => asOf - p.last_activity_at > STALE_AFTER_SECS).length,
  };
}

const KPI_GOOD_DIRECTION: Record<KpiKey, "up" | "down"> = {
  openPrs: "down",
  awaitingReview: "down",
  stale: "down",
  merged: "up",
  medianTtm: "down",
  medianTtfr: "down",
};

/** `null` (neutral) for a zero delta; otherwise whether the change moved in
 * that metric's good direction (down for every KPI except `merged`, per the
 * packet's "fewer stale is good, more merged is good"). */
function deltaGoodness(key: KpiKey, delta: number): boolean | null {
  if (delta === 0) return null;
  const wantUp = KPI_GOOD_DIRECTION[key] === "up";
  return delta > 0 ? wantUp : !wantUp;
}

function countTile(key: KpiKey, label: string, value: number | null, prevValue: number | null): KpiTile {
  const delta = value == null || prevValue == null ? null : value - prevValue;
  return {
    key,
    label,
    displayValue: value == null ? "—" : String(value),
    secondaryLabel: null,
    deltaLabel: delta == null ? null : formatSignedCount(delta),
    deltaGood: delta == null ? null : deltaGoodness(key, delta),
  };
}

/** `p90Value`, when given, becomes the tile's `secondaryLabel` ("p90 9d 4h")
 * — the packet's "show p90 beside the median wherever a median is already
 * shown". `null` (no samples) renders no secondary line at all, same as the
 * median's own "—" case. */
function durationTile(
  key: KpiKey,
  label: string,
  value: number | null,
  prevValue: number | null,
  p90Value: number | null = null,
): KpiTile {
  const delta = value == null || prevValue == null ? null : value - prevValue;
  return {
    key,
    label,
    displayValue: formatDuration(value),
    secondaryLabel: p90Value == null ? null : `p90 ${formatDuration(p90Value)}`,
    deltaLabel: delta == null ? null : formatSignedDuration(delta),
    deltaGood: delta == null ? null : deltaGoodness(key, delta),
  };
}

/** The KPI row's 6 tiles. `current`/`previous` are the same-scope digest
 * calls (team, or team+actor once a person is selected — "Engineer focus"
 * reuses this same computation, just narrower). `pullsNow` is that same
 * scope's live `openPulls()` result; `null` when that call failed (rendered
 * as "—", no delta — not a fabricated zero). `previous` is `null` when the
 * previous-window digest call failed, for the same reason. */
export function computeKpiTiles(args: {
  current: Digest;
  previous: Digest | null;
  pullsNow: OpenPull[] | null;
  previousWindowEnd: number;
  now: number;
}): KpiTile[] {
  const { current, previous, pullsNow, now } = args;
  const gaugeNow = pullsNow ? gaugeCountsNow(pullsNow, now) : null;

  return [
    // No previous value for the three live gauges: `pulls` stores only what is
    // OPEN right now, so a PR open during the previous window but closed since
    // is invisible — every "previous" would read low and the delta would skew
    // systematically toward "worse". A wrong arrow is worse than no arrow;
    // these get a real delta once the store keeps PR history.
    countTile("openPrs", "Open PRs", gaugeNow?.open ?? null, null),
    countTile("awaitingReview", "Awaiting review", gaugeNow?.awaitingReview ?? null, null),
    countTile("stale", "Stale (3+ days)", gaugeNow?.stale ?? null, null),
    countTile("merged", "Merged in window", current.totals.pr_merged, previous?.totals.pr_merged ?? null),
    durationTile(
      "medianTtm",
      "Median time to merge",
      current.pr_timing.median_ttm_secs,
      previous?.pr_timing.median_ttm_secs ?? null,
      current.pr_timing.p90_ttm_secs,
    ),
    durationTile(
      "medianTtfr",
      "Median time to first review",
      current.pr_timing.median_ttfr_secs,
      previous?.pr_timing.median_ttfr_secs ?? null,
      current.pr_timing.p90_ttfr_secs,
    ),
  ];
}

// --- in-flight PRs, grouped by state ---

export type PullStateGroup = "draft" | "awaiting_review" | "changes_requested" | "approved";

export const PULL_STATE_GROUP_ORDER: PullStateGroup[] = ["draft", "awaiting_review", "changes_requested", "approved"];

export const PULL_STATE_GROUP_LABEL: Record<PullStateGroup, string> = {
  draft: "Draft",
  awaiting_review: "Awaiting review",
  changes_requested: "Changes requested",
  approved: "Approved",
};

/** Draft first regardless of review state; otherwise by `review_decision`
 * (null/"review_required" both read as "awaiting review" — nobody has
 * finished looking yet). */
export function pullStateGroup(p: OpenPull): PullStateGroup {
  if (p.draft) return "draft";
  if (p.review_decision === "changes_requested") return "changes_requested";
  if (p.review_decision === "approved") return "approved";
  return "awaiting_review";
}

export interface OpenPullGroup {
  group: PullStateGroup;
  label: string;
  pulls: OpenPull[];
}

/** Groups by state in the packet's fixed order (Draft / Awaiting review /
 * Changes requested / Approved), omitting empty groups; within a group,
 * oldest-created first — the ones that have been waiting longest float to
 * the top of their bucket. */
export function groupOpenPulls(pulls: OpenPull[]): OpenPullGroup[] {
  const byGroup = new Map<PullStateGroup, OpenPull[]>();
  for (const p of pulls) {
    const g = pullStateGroup(p);
    if (!byGroup.has(g)) byGroup.set(g, []);
    byGroup.get(g)!.push(p);
  }
  return PULL_STATE_GROUP_ORDER.filter((g) => byGroup.has(g)).map((g) => ({
    group: g,
    label: PULL_STATE_GROUP_LABEL[g],
    pulls: byGroup.get(g)!.slice().sort((a, b) => a.created_at - b.created_at),
  }));
}

// --- engineer roster ---

export interface RosterRow {
  login: string;
  avatar_url: string | null;
  open_prs: number;
  review_queue: number;
  merged: number;
  reviews_given: number;
  comments: number;
  commits: number;
  last_at: number | null;
  /** True for a synthesized row: a team member the digest's `people` never
   * mentioned because they had no events at all in the window. */
  zeroActivity: boolean;
}

/** Team members the digest's `people` has no entry for — `null` when
 * `teamMemberLogins` itself is `null` ("All teams" selected, so there's no
 * single membership list to diff against; the caller renders that as "not
 * available" rather than an empty list, which would otherwise read as
 * "full attendance"). */
export function zeroActivityLogins(teamMemberLogins: string[] | null, people: DigestPerson[]): string[] | null {
  if (!teamMemberLogins) return null;
  const known = new Set(people.map((p) => p.login));
  return teamMemberLogins.filter((login) => !known.has(login));
}

function rosterRowFromPerson(p: DigestPerson): RosterRow {
  return {
    login: p.login,
    avatar_url: p.avatar_url,
    open_prs: p.open_prs,
    review_queue: p.review_queue,
    merged: p.counts.pr_merged,
    reviews_given: p.counts.pr_reviewed,
    comments: p.counts.pr_commented + p.counts.issue_commented,
    commits: p.counts.commit,
    last_at: p.last_at,
    zeroActivity: false,
  };
}

function zeroRosterRow(login: string): RosterRow {
  return {
    login,
    avatar_url: null,
    open_prs: 0,
    review_queue: 0,
    merged: 0,
    reviews_given: 0,
    comments: 0,
    commits: 0,
    last_at: null,
    zeroActivity: true,
  };
}

/** One row per `DigestPerson`, plus a zero-activity row for every team
 * member `people` has no entry for (when `teamMemberLogins` is known —
 * see `zeroActivityLogins`). */
export function rosterRows(people: DigestPerson[], teamMemberLogins: string[] | null): RosterRow[] {
  const rows = people.map(rosterRowFromPerson);
  const zeros = zeroActivityLogins(teamMemberLogins, people) ?? [];
  for (const login of zeros) rows.push(zeroRosterRow(login));
  return rows;
}

export type RosterSortKey = "login" | "open_prs" | "review_queue" | "merged" | "reviews_given" | "comments" | "commits" | "last_at";

export const DEFAULT_ROSTER_SORT: { key: RosterSortKey; dir: "asc" | "desc" } = { key: "open_prs", dir: "desc" };

function numericRosterValue(row: RosterRow, key: RosterSortKey): number {
  switch (key) {
    case "open_prs":
      return row.open_prs;
    case "review_queue":
      return row.review_queue;
    case "merged":
      return row.merged;
    case "reviews_given":
      return row.reviews_given;
    case "comments":
      return row.comments;
    case "commits":
      return row.commits;
    case "last_at":
      // Never active sorts as "oldest" in both directions, not "newest".
      return row.last_at ?? -Infinity;
    case "login":
      return 0; // unused — login sorts by localeCompare below
  }
}

/** Stable sort (ties keep their incoming order — already login-ish from the
 * digest) by any roster column; `dir` flips the comparison, not the tile's
 * meaning. */
export function sortRoster(rows: RosterRow[], key: RosterSortKey, dir: "asc" | "desc"): RosterRow[] {
  const sign = dir === "asc" ? 1 : -1;
  return rows.slice().sort((a, b) => {
    if (key === "login") return sign * a.login.localeCompare(b.login);
    return sign * (numericRosterValue(a, key) - numericRosterValue(b, key));
  });
}

// --- needs attention (rule-based) ---

export type AttentionRuleKey =
  | "stale"
  | "approvedUnmerged"
  | "changesRequestedStale"
  | "noReviewer"
  | "zeroActivity"
  | "reviewImbalance";

export interface AttentionItem {
  label: string;
  url?: string | null;
  /** The thread this item is about, so the row can open Vigie's own detail
   * pane instead of leaving for GitHub (`url` stays, for the pane's
   * secondary "Open on GitHub" control). `null`/absent for a rule whose
   * item isn't about any single thread — `zeroActivity`'s team member and
   * `reviewImbalance`'s reviewer, both person-level, not PR-level. */
  repo_id?: number | null;
  number?: number | null;
}

export interface AttentionRule {
  key: AttentionRuleKey;
  /** The always-visible summary line, count included. */
  label: string;
  count: number;
  /** Behind the disclosure; empty when `count` is 0. */
  items: AttentionItem[];
  /** True when the rule couldn't be evaluated at all (only `zeroActivity`,
   * and only with no single team selected) — rendered distinctly from "0
   * found". */
  unavailable?: boolean;
}

const APPROVED_UNMERGED_AFTER_SECS = 1 * 86_400;
const CHANGES_REQUESTED_AFTER_SECS = 2 * 86_400;
const NO_REVIEWER_AFTER_SECS = 1 * 86_400;
const REVIEW_IMBALANCE_MIN_TOTAL = 4;
const REVIEW_IMBALANCE_SHARE = 0.5;

function pullAttentionItem(p: OpenPull, detail: string): AttentionItem {
  return { label: `#${p.number} ${p.title} — ${detail}`, url: p.url, repo_id: p.repo_id, number: p.number };
}

function plural(n: number, noun: string): string {
  return `${n} ${noun}${n === 1 ? "" : "s"}`;
}

/** The six rules (packet §6), each one line with a count plus its offending
 * items. `pulls` and `digestTeamWide` are always team-scoped (unfiltered by
 * any selected person) — team hygiene, not one engineer's view of it.
 * `teamMemberLogins` is `null` under "All teams" (see `zeroActivityLogins`). */
export function computeAttentionRules(args: {
  pulls: OpenPull[];
  digestTeamWide: Digest;
  teamMemberLogins: string[] | null;
  now: number;
}): AttentionRule[] {
  const { pulls, digestTeamWide, teamMemberLogins, now } = args;

  const stale = pulls.filter((p) => isStale(p, now));
  const approvedUnmerged = pulls.filter(
    (p) => p.review_decision === "approved" && now - p.last_activity_at > APPROVED_UNMERGED_AFTER_SECS,
  );
  const changesRequestedStale = pulls.filter(
    (p) => p.review_decision === "changes_requested" && now - p.last_activity_at > CHANGES_REQUESTED_AFTER_SECS,
  );
  const noReviewer = pulls.filter(
    (p) => !p.draft && p.requested_reviewers.length === 0 && now - p.created_at > NO_REVIEWER_AFTER_SECS,
  );
  const zeros = zeroActivityLogins(teamMemberLogins, digestTeamWide.people);

  const reviewCounts = new Map<string, number>();
  let reviewTotal = 0;
  for (const p of pulls) {
    for (const login of p.requested_reviewers) {
      reviewCounts.set(login, (reviewCounts.get(login) ?? 0) + 1);
      reviewTotal += 1;
    }
  }
  let imbalanced: { login: string; count: number } | null = null;
  if (reviewTotal >= REVIEW_IMBALANCE_MIN_TOTAL) {
    for (const [login, count] of reviewCounts) {
      if (count / reviewTotal > REVIEW_IMBALANCE_SHARE) {
        imbalanced = { login, count };
        break;
      }
    }
  }

  return [
    {
      key: "stale",
      label: `${plural(stale.length, "stale PR")} (no activity for 3+ days)`,
      count: stale.length,
      items: stale.map((p) => pullAttentionItem(p, `quiet ${formatDuration(now - p.last_activity_at)}`)),
    },
    {
      key: "approvedUnmerged",
      label: `${plural(approvedUnmerged.length, "approved PR")} sitting unmerged (1+ day)`,
      count: approvedUnmerged.length,
      items: approvedUnmerged.map((p) => pullAttentionItem(p, `approved, quiet ${formatDuration(now - p.last_activity_at)}`)),
    },
    {
      key: "changesRequestedStale",
      label: `${plural(changesRequestedStale.length, "PR")} with changes requested, untouched (2+ days)`,
      count: changesRequestedStale.length,
      items: changesRequestedStale.map((p) => pullAttentionItem(p, `quiet ${formatDuration(now - p.last_activity_at)}`)),
    },
    {
      key: "noReviewer",
      label: `${plural(noReviewer.length, "PR")} open 1+ day with no reviewer requested`,
      count: noReviewer.length,
      items: noReviewer.map((p) => pullAttentionItem(p, `open ${formatDuration(now - p.created_at)}`)),
    },
    {
      key: "zeroActivity",
      label:
        zeros == null
          ? "Zero-activity teammates — select a team to see this"
          : `${plural(zeros.length, "team member")} with zero activity this window`,
      count: zeros?.length ?? 0,
      items: (zeros ?? []).map((login) => ({ label: login })),
      unavailable: zeros == null,
    },
    {
      key: "reviewImbalance",
      label: imbalanced
        ? `${imbalanced.login} holds ${imbalanced.count} of ${reviewTotal} requested reviews`
        : "Review load looks balanced",
      count: imbalanced ? 1 : 0,
      items: imbalanced ? [{ label: `${imbalanced.login}: ${imbalanced.count} of ${reviewTotal} requested reviews` }] : [],
    },
  ];
}

// --- Aging WIP (packet §A1, replaces the activity-mix bar) ---

export type AgeBucket = "under1d" | "d1to3" | "d3to7" | "over7d";

export const AGE_BUCKET_ORDER: AgeBucket[] = ["under1d", "d1to3", "d3to7", "over7d"];

export const AGE_BUCKET_LABEL: Record<AgeBucket, string> = {
  under1d: "Under 1 day",
  d1to3: "1–3 days",
  d3to7: "3–7 days",
  over7d: "Over 7 days",
};

/** Which age bucket an open PR falls in, by wall-clock age since it was
 * opened (`created_at`) — the same "age", not "waiting time", framing as
 * `isStale`/the honesty rule below: this buckets how long a PR has existed,
 * not how long any particular review has been outstanding. */
export function pullAgeBucket(p: OpenPull, now: number): AgeBucket {
  const ageDays = (now - p.created_at) / 86_400;
  if (ageDays < 1) return "under1d";
  if (ageDays < 3) return "d1to3";
  if (ageDays < 7) return "d3to7";
  return "over7d";
}

export interface AgingBucketCount {
  bucket: AgeBucket;
  label: string;
  count: number;
}

/** Every bucket in fixed order, count included even when 0 — so the bar
 * always draws all four segments in the same order, not just the ones with
 * something in them. */
export function agingWipBuckets(pulls: OpenPull[], now: number): AgingBucketCount[] {
  const counts = new Map<AgeBucket, number>();
  for (const p of pulls) {
    const b = pullAgeBucket(p, now);
    counts.set(b, (counts.get(b) ?? 0) + 1);
  }
  return AGE_BUCKET_ORDER.map((bucket) => ({ bucket, label: AGE_BUCKET_LABEL[bucket], count: counts.get(bucket) ?? 0 }));
}

/** `bucket` null returns every pull unfiltered — clicking the same bucket
 * again clears the filter, per the packet. */
export function filterPullsByAgeBucket(pulls: OpenPull[], bucket: AgeBucket | null, now: number): OpenPull[] {
  if (!bucket) return pulls;
  return pulls.filter((p) => pullAgeBucket(p, now) === bucket);
}

// --- Stuck panel (packet §A2; abandoned-PR filtering added by gm-sumpolish-r1 §3) ---

/** A PR with no activity at all in this long isn't "waiting for review"
 * anymore — nobody is coming back to it. Excluded from the "oldest PR
 * awaiting review" tile so a single dead PR doesn't camp there forever and
 * train the user to ignore the whole panel. */
export const ABANDONED_AFTER_SECS = 30 * 86_400;

export interface OldestAwaitingReview {
  pull: OpenPull;
  ageSecs: number;
}

export interface StuckOldestResult {
  /** The oldest open PR that's both awaiting review and still plausibly
   * alive (some activity within ABANDONED_AFTER_SECS). `null` means there's
   * nothing actionable to show — could be because nothing is awaiting
   * review at all, or because everything that is has gone quiet;
   * `excludedAbandonedCount` tells those two cases apart. */
  oldest: OldestAwaitingReview | null;
  /** How many non-draft, undecided PRs were left out for having no activity
   * in over ABANDONED_AFTER_SECS. Zero means nothing was hidden. */
  excludedAbandonedCount: number;
}

/** The single oldest open PR still awaiting review (not draft, no decision
 * yet or changes re-requested — same predicate as the "awaiting review" KPI
 * gauge) AND not abandoned, by time since it was opened. Ties (identical
 * `created_at`) resolve to the lower `(repo_id, number)` so the answer is
 * stable, not hash-order-dependent. */
export function oldestAwaitingReview(pulls: OpenPull[], now: number): StuckOldestResult {
  const candidates = pulls.filter(isAwaitingReview);
  const alive: OpenPull[] = [];
  let excludedAbandonedCount = 0;
  for (const p of candidates) {
    if (now - p.last_activity_at > ABANDONED_AFTER_SECS) excludedAbandonedCount++;
    else alive.push(p);
  }
  if (alive.length === 0) return { oldest: null, excludedAbandonedCount };
  const oldest = alive.reduce((a, b) => {
    if (a.created_at !== b.created_at) return a.created_at < b.created_at ? a : b;
    return a.repo_id !== b.repo_id ? (a.repo_id < b.repo_id ? a : b) : a.number < b.number ? a : b;
  });
  return { oldest: { pull: oldest, ageSecs: now - oldest.created_at }, excludedAbandonedCount };
}

export interface ReviewerQueueWait {
  login: string;
  queueSize: number;
  /** Age of the oldest PR in this reviewer's queue — the number that makes
   * this "longest-waiting", not merely "most requests". */
  oldestAgeSecs: number;
}

/** The requested reviewer sitting on the single stalest pending review
 * request — ranked by how long the oldest thing in their queue has been
 * open, not by how many things are in it (a reviewer with 5 fresh requests
 * loses to one with 1 nine-day-old request). Only counts PRs still awaiting
 * review (`isAwaitingReview`) — an approved or changes-requested PR isn't
 * sitting in anyone's incoming queue any more. `null` when nobody has
 * anything outstanding. Ties go to the lower login, for a stable answer. */
export function longestWaitingReviewer(pulls: OpenPull[], now: number): ReviewerQueueWait | null {
  const byLogin = new Map<string, OpenPull[]>();
  for (const p of pulls) {
    if (!isAwaitingReview(p)) continue;
    for (const login of p.requested_reviewers) {
      if (!byLogin.has(login)) byLogin.set(login, []);
      byLogin.get(login)!.push(p);
    }
  }
  let best: ReviewerQueueWait | null = null;
  for (const [login, queue] of byLogin) {
    const oldest = queue.reduce((a, b) => (a.created_at <= b.created_at ? a : b));
    const oldestAgeSecs = now - oldest.created_at;
    if (!best || oldestAgeSecs > best.oldestAgeSecs || (oldestAgeSecs === best.oldestAgeSecs && login < best.login)) {
      best = { login, queueSize: queue.length, oldestAgeSecs };
    }
  }
  return best;
}

// --- Concentration hint (packet §A5) ---

/** Above this share, a repo's commits this window read as one person's,
 * whatever the team around it looks like — the packet's bus-factor hint. */
export const CONCENTRATION_THRESHOLD = 0.7;

/** "87% one author" — rounded, never a raw float, and only ever called once
 * the caller has already checked `share > CONCENTRATION_THRESHOLD` and
 * `login` is non-null (a hint, not a metric to show unconditionally). */
export function formatConcentration(share: number): string {
  return `${Math.round(share * 100)}% one author`;
}

// --- Worth a careful read (packet §A4) ---

export interface CarefulReadItem {
  pull: OpenPull;
  score: number;
  /** Reason in words, e.g. "742 lines changed, 6 comments, 6 days old, 2
   * reviewers" — never a quality judgement (no "bad"/"risky"), just what
   * fed the score. */
  reason: string;
}

export interface CarefulReadResult {
  /** Top 5 by score, ties broken oldest-first (a tie is more informative
   * resolved toward "has been open longer" than left to array order). */
  items: CarefulReadItem[];
  /** `false` when `threads` was empty and the "comment rounds" term was
   * therefore dropped, with the other three weights renormalised to sum to
   * 1 (see `computeCarefulRead`'s doc comment) — the caller uses this to
   * caption the list honestly instead of implying every factor was used. */
  usedCommentRounds: boolean;
}

const CAREFUL_READ_BASE_WEIGHTS = { size: 0.35, rounds: 0.3, age: 0.2, reviewers: 0.15 };

/** Min-max normalises `values` to `0..1` within themselves; `0` for every
 * value when they're all equal (including a single-element input) — no
 * divide-by-zero, and "no variation in this set" reads as "nothing to
 * distinguish them by", not an arbitrary 1 or NaN. */
/** Mirrors the engine's `DIGEST_THREAD_LIMIT` (crates/gitmon/src/types.rs):
 * how many threads a digest returns. Used to tell "no discussion" apart from
 * "fell outside the top ten". */
const DIGEST_THREAD_LIMIT = 10;

function minMaxNormalize(values: number[]): number[] {
  if (values.length === 0) return [];
  const min = Math.min(...values);
  const max = Math.max(...values);
  if (max === min) return values.map(() => 0);
  return values.map((v) => (v - min) / (max - min));
}

function carefulReadReason(args: { additions: number | null; deletions: number | null; rounds: number | null; ageSecs: number; reviewers: number }): string {
  const parts: string[] = [];
  const lines = (args.additions ?? 0) + (args.deletions ?? 0);
  if (lines > 0) parts.push(`${lines} line${lines === 1 ? "" : "s"} changed`);
  // "updates", not "comments": the count is every event on the thread —
  // pushes, reviews and comments alike — so naming it comments would be a
  // more precise claim than the number supports.
  if (args.rounds != null && args.rounds > 0) parts.push(`${args.rounds} update${args.rounds === 1 ? "" : "s"}`);
  const days = Math.floor(args.ageSecs / 86_400);
  parts.push(days >= 1 ? `${days} day${days === 1 ? "" : "s"} old` : "opened today");
  if (args.reviewers > 0) parts.push(`${args.reviewers} reviewer${args.reviewers === 1 ? "" : "s"}`);
  return parts.length > 0 ? parts.join(", ") : "small and quiet";
}

/**
 * "Worth a careful read" (packet §A4, board item 13): a deterministic score
 * per open PR — never a quality judgement, a prompt that a PR deserves
 * slower attention. Four signals, each min-max normalised to `0..1` within
 * `pulls` (the current set, not all-time): size (`additions + deletions`),
 * "comment rounds", age since opened, and requested-reviewer count —
 * weighted size .35 / rounds .3 / age .2 / reviewers .15.
 *
 * SHORTCUT: a per-PR count of `pr_commented`/`pr_reviewed` events has no
 * source on this page without a `get_thread` fetch per open PR (expensive,
 * and out of this packet's Boundaries). `threads` — the same
 * `Digest.threads` this page already fetched for "Most discussed", already
 * an events-per-thread count over the current window — stands in for it:
 * `threads` is windowed exactly like everything else here, so a PR that
 * doesn't match one is read as "not much discussion this window" (a real
 * answer), not a gap. Only when `threads` is empty entirely is the term
 * truly unavailable (there's nothing to look up for anyone); then it's
 * dropped and the other three weights renormalised to sum to 1, per the
 * packet's "drop the term, don't treat it as zero" instruction. Upgrade
 * path: a per-thread kind-count field on `Digest` (or `DigestThread`) would
 * remove both the mixed-kind approximation and the top-10 cap.
 */
export function computeCarefulRead(pulls: OpenPull[], threads: DigestThread[], now: number): CarefulReadResult {
  if (pulls.length === 0) return { items: [], usedCommentRounds: threads.length > 0 };

  const threadEventsByKey = new Map(threads.map((t) => [`${t.repo_id}:${t.number}`, t.events]));

  // The engine returns at most DIGEST_THREAD_LIMIT threads. When that cap is
  // reached AND some open PR has no entry, a missing entry no longer means
  // "no discussion" — it may simply have fallen outside the top ten. Scoring
  // those rows 0 would quietly zero the second-heaviest signal for most of
  // the set, so the term is dropped and the remaining weights renormalised,
  // exactly as when there are no threads at all.
  const threadsTruncated = threads.length >= DIGEST_THREAD_LIMIT;
  const someUnmatched = pulls.some((p) => !threadEventsByKey.has(`${p.repo_id}:${p.number}`));
  const usedCommentRounds = threads.length > 0 && !(threadsTruncated && someUnmatched);

  const sizes = pulls.map((p) => (p.additions ?? 0) + (p.deletions ?? 0));
  const ages = pulls.map((p) => Math.max(0, now - p.created_at));
  const reviewerCounts = pulls.map((p) => p.requested_reviewers.length);
  const rounds = pulls.map((p) => threadEventsByKey.get(`${p.repo_id}:${p.number}`) ?? 0);

  const normSize = minMaxNormalize(sizes);
  const normAge = minMaxNormalize(ages);
  const normReviewers = minMaxNormalize(reviewerCounts);
  const normRounds = usedCommentRounds ? minMaxNormalize(rounds) : null;

  const weights = usedCommentRounds
    ? CAREFUL_READ_BASE_WEIGHTS
    : (() => {
        const { size, age, reviewers } = CAREFUL_READ_BASE_WEIGHTS;
        const sum = size + age + reviewers;
        return { size: size / sum, age: age / sum, reviewers: reviewers / sum, rounds: 0 };
      })();

  const items: CarefulReadItem[] = pulls.map((p, i) => {
    const score =
      normSize[i] * weights.size +
      normAge[i] * weights.age +
      normReviewers[i] * weights.reviewers +
      (usedCommentRounds ? normRounds![i] * weights.rounds : 0);
    return {
      pull: p,
      score,
      reason: carefulReadReason({
        additions: p.additions,
        deletions: p.deletions,
        rounds: usedCommentRounds ? rounds[i] : null,
        ageSecs: ages[i],
        reviewers: reviewerCounts[i],
      }),
    };
  });

  items.sort((a, b) => b.score - a.score || a.pull.created_at - b.pull.created_at);
  return { items: items.slice(0, 5), usedCommentRounds };
}

// Re-exported only so components can annotate props without importing
// lib/types directly for these two — trivial, but keeps the summary/*
// components' imports pointed at one module for "the summary domain".
export type { KindCounts, ReviewDecision };
