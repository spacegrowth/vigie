// Pure logic for the backfill flow: Repos.svelte's fetch-right-after-adding
// and Feed.svelte's "Load older activity" both call the engine's `backfill`
// command, then hand its result here to decide what happened and how to say
// so in one line. Kept out of the .svelte files so it's unit-testable
// without mounting a component — see backfill.test.ts.
import type { BackfillResult, RepoBackfill } from "./types";

/** One step's window, in seconds — matches the engine's own
 * `BACKFILL_DEFAULT_SPAN_SECS` (crates/gitmon/src/poll.rs). Passed explicitly
 * rather than leaning on the command's own default so the two can't drift
 * apart silently, and so every call site asks for the same "one step" the
 * rest of this module's wording assumes. */
export const BACKFILL_STEP_SPAN_SECS = 7 * 24 * 60 * 60;

const NOT_POLLED_YET = "not polled yet";
const SIGNED_OUT = "signed out";
const WINDOW_TRUNCATED = "window truncated";

/** True when a repo's step ran clean enough to trust its `inserted` count —
 * either no error at all, or `"window truncated"` (the cursor still moved,
 * it just didn't see everything in one page). */
function ran(step: RepoBackfill): boolean {
  return step.error == null || step.error === WINDOW_TRUNCATED;
}

/** True when a repo's step never reached GitHub at all — there was nothing
 * to retry, only a precondition to fix first (poll once, or sign back in). */
function wasSkipped(step: RepoBackfill): boolean {
  return step.error === NOT_POLLED_YET || step.error === SIGNED_OUT;
}

/** True when `step` couldn't run because the repo has never completed a
 * poll — there is no `backfilled_to` floor to step down from yet. The fix is
 * to poll once, then retry the same backfill call. */
export function stepNeedsFirstPoll(result: BackfillResult, repoId: number): boolean {
  return result.repos.find((r) => r.repo_id === repoId)?.error === NOT_POLLED_YET;
}

/** One sentence for the single-repo step Repos.svelte runs right after
 * adding a repo. `result` is expected to carry exactly the one row asked
 * for; a missing row (the engine never omits one for a valid id) falls back
 * to a plain "couldn't check" rather than assuming success. */
export function describeNewRepoBackfill(result: BackfillResult, repoId: number): string {
  const step = result.repos.find((r) => r.repo_id === repoId);
  if (!step) return "Couldn't check for older activity.";
  if (step.error === SIGNED_OUT) return "Signed out — sign in again to fetch its history.";
  if (step.error === NOT_POLLED_YET) return "Couldn't fetch yet — this repo hasn't polled.";
  if (!ran(step)) return `Couldn't fetch older activity (${step.error}).`;
  if (step.inserted === 0) return "No activity in the last 7 days.";
  return `Found ${step.inserted} event${step.inserted === 1 ? "" : "s"} from the last 7 days.`;
}

export type BackfillOutcome =
  | { kind: "all-skipped" }
  | { kind: "all-failed" }
  | { kind: "nothing-found" }
  | { kind: "added"; count: number; from: number; to: number };

/** Sorts a whole-fleet `backfill(null, span)` step into one of four buckets
 * — used by "Load older activity" so a call that never actually reached
 * GitHub ("all-skipped": every repo is unpolled or signed out) reads
 * differently from one that reached it and simply found nothing
 * ("nothing-found"), rather than both silently looking like success. */
export function classifyBackfillResult(result: BackfillResult): BackfillOutcome {
  const { repos } = result;
  if (repos.length === 0 || repos.every(wasSkipped)) return { kind: "all-skipped" };

  const clean = repos.filter(ran);
  if (clean.length === 0) return { kind: "all-failed" };

  const count = clean.reduce((sum, r) => sum + r.inserted, 0);
  if (count === 0) return { kind: "nothing-found" };

  const from = Math.min(...clean.map((r) => r.from));
  const to = Math.max(...clean.map((r) => r.to));
  return { kind: "added", count, from, to };
}

/** The one-line result "Load older activity" shows after a step, for
 * whichever `classifyBackfillResult` bucket the step landed in.
 *
 * `filtered` says the feed is showing a narrower slice than the step
 * fetched: a step always runs across every watched repo, so under a team,
 * person or watched filter the count can be real while the visible list
 * gains nothing. Saying "across all repos" is the difference between an
 * honest number and one that looks broken. */
export function describeBackfillOutcome(outcome: BackfillOutcome, filtered = false): string {
  switch (outcome.kind) {
    case "all-skipped":
      return "Nothing to fetch yet — those repos haven't completed a first poll.";
    case "all-failed":
      return "Couldn't fetch older activity right now.";
    case "nothing-found":
      return filtered ? "Nothing older found in any repo." : "Nothing older found.";
    case "added": {
      const events = `${outcome.count} event${outcome.count === 1 ? "" : "s"}`;
      const range = formatDateRange(outcome.from, outcome.to);
      return filtered
        ? `Added ${events} from ${range}, across all repos — the current filter may hide them.`
        : `Added ${events} from ${range}.`;
    }
  }
}

const SHORT_MONTHS = [
  "Jan",
  "Feb",
  "Mar",
  "Apr",
  "May",
  "Jun",
  "Jul",
  "Aug",
  "Sep",
  "Oct",
  "Nov",
  "Dec",
];

/** `[fromSecs, toSecs)` -> "12–19 Aug" (same month) or "28 Jul–4 Aug"
 * (spanning months) — `toSecs` is exclusive, so the printed end date is the
 * day before it. Both dates render in local time, the same convention
 * `time.ts`'s `dayLabel`/`absoluteTime` use for a unix-seconds timestamp.
 *
 * Deliberately not `toLocaleDateString`: its day/month order and its month
 * spelling both follow the viewer's locale, which would make this read as
 * "Aug 18" instead of "18 Aug" for anyone not on an en-GB-like locale. A
 * fixed day-then-month-abbreviation format keeps the wording this module's
 * callers promise (and its own tests assert) independent of that. */
export function formatDateRange(fromSecs: number, toSecs: number): string {
  const from = new Date(fromSecs * 1000);
  const to = new Date(Math.max(fromSecs, toSecs - 1) * 1000);
  const label = (d: Date) => `${d.getDate()} ${SHORT_MONTHS[d.getMonth()]}`;
  const sameMonth = from.getFullYear() === to.getFullYear() && from.getMonth() === to.getMonth();
  return sameMonth ? `${from.getDate()}–${label(to)}` : `${label(from)}–${label(to)}`;
}

// --- Spending the request budget honestly (packet gm-guard-r1) -----------
//
// "Load older activity" used to be a free-looking button: nothing on it said
// what a click would cost, a first click silently re-armed itself to fire
// again from the end-of-feed sentinel on every later visit (even after
// remounting the view), and nothing ever stopped offering it once the fleet
// had genuinely run dry. The four pieces below make a step's cost visible
// before it's spent, and give Feed.svelte the three honest reasons to stop
// offering another one — a real "nothing older", a session-length safety
// cap, and the same rate-limit budget the status bar shows.

/** The button's label names the request it's about to make — a step's fixed
 * span and the exact number of repos it will hit, not a vague verb. */
export function backfillStepLabel(repoCount: number): string {
  const days = BACKFILL_STEP_SPAN_SECS / (24 * 60 * 60);
  return `Load ${days} more days · ${repoCount} repo${repoCount === 1 ? "" : "s"}`;
}

/** Below this many remaining requests, "Load older activity" stops offering
 * another step — the guard that actually caps the drain, independent of
 * whether the fleet still has older history. Roughly 500: comfortably above
 * what a single step over a large fleet spends (a step is at least one
 * listing request per repo, more where PRs moved), so tripping it still
 * leaves headroom for polling and everything else the hour needs. */
export const LOW_BUDGET_THRESHOLD = 500;

/** True once the remaining hourly allowance (the same figure the status bar
 * shows) is too low to spend on a step. `null` (not known yet) never trips
 * it — there is nothing to gate on. */
export function backfillBudgetLow(rateLimitRemaining: number | null): boolean {
  return rateLimitRemaining != null && rateLimitRemaining < LOW_BUDGET_THRESHOLD;
}

/** Why the control is disabled when `backfillBudgetLow` is true. */
export function describeLowBudget(rateLimitRemaining: number): string {
  return `Paused — ${rateLimitRemaining.toLocaleString()} GitHub requests left this hour`;
}

/** How many requests a step actually spent, when both readings are known —
 * the reading taken right before the call and the one the step itself
 * returned. `null` whenever that isn't knowable (either reading missing) or
 * isn't a spend at all (the hourly window rolled over mid-step and the
 * count went back up), so a caller can skip the line entirely rather than
 * print a negative or fabricated number. */
export function requestsSpent(before: number | null, after: number | null): number | null {
  if (before == null || after == null) return null;
  const spent = before - after;
  return spent > 0 ? spent : null;
}

/** The clause "Show what it cost" (packet item 6) adds alongside the
 * existing "Added N events…" line. */
export function describeRequestsSpent(spent: number): string {
  return `${spent.toLocaleString()} request${spent === 1 ? "" : "s"} used.`;
}

/** Session-length bookkeeping behind the "stop offering" guard: the set of
 * repos the streak below was counted against, and how many *consecutive*
 * steps in a row ran clean across every repo and still found nothing. Kept
 * as an explicit value (not hidden module state) so the guard itself stays a
 * pure, tested function — Feed.svelte is the one place that has to hold this
 * across a remount, in its own `<script module>` block, since a step's
 * result must survive leaving the view and coming back. */
export interface BackfillGuardState {
  readonly repoIds: readonly number[];
  readonly consecutiveEmpty: number;
}

export const INITIAL_BACKFILL_GUARD: BackfillGuardState = { repoIds: [], consecutiveEmpty: 0 };

/** Consecutive "ran clean, found nothing" steps before "Load older activity"
 * stops offering more. Not tripped by a single empty step: a quiet week in
 * every repo at once is plausible and shouldn't read as "no more history
 * exists" — but two in a row spending real requests to confirm the same
 * thing is the request drain to guard against, so the third click never
 * fires. */
export const EMPTY_STEP_LIMIT = 2;

function sameRepoSet(a: readonly number[], b: readonly number[]): boolean {
  if (a.length !== b.length) return false;
  const seen = new Set(a);
  return b.every((id) => seen.has(id));
}

/** Reconciles a persisted guard against the fleet as it stands *right now*,
 * before any new step has run: watching a different set of repos than the
 * streak was counted against means a prior "nothing older" verdict no
 * longer speaks for this fleet (a newly-added repo has history nobody has
 * asked about yet), so the streak clears. Safe to call on every render —
 * it only ever resets to a fresh, unspent streak, never fabricates one. */
export function effectiveBackfillGuard(
  state: BackfillGuardState,
  repoIds: readonly number[],
): BackfillGuardState {
  return sameRepoSet(state.repoIds, repoIds) ? state : { repoIds: [...repoIds], consecutiveEmpty: 0 };
}

/** Clears a tripped streak when the poller stores something new: fresh
 * activity means the fleet is not the same fleet the "nothing older"
 * verdict was reached about, so the control should offer again. Without
 * this the only escapes are changing the watched repos or restarting —
 * a fleet that is genuinely quiet for a fortnight would otherwise disable
 * "Load older activity" for the whole session, which is harder than the
 * soft cap this guard is meant to be. */
export function clearBackfillGuardOnNewEvents(
  state: BackfillGuardState,
  newEventCount: number,
): BackfillGuardState {
  return newEventCount > 0 && state.consecutiveEmpty > 0
    ? { repoIds: [...state.repoIds], consecutiveEmpty: 0 }
    : state;
}

/** Folds one completed step's outcome into the guard. Only a clean,
 * genuinely-empty step (`"nothing-found"`) extends the streak; a skip or a
 * failure says nothing about whether older history exists, so it neither
 * extends nor clears it, and finding something always clears it. `state`
 * should already be `effectiveBackfillGuard`'s result for `repoIds`, so a
 * repo-set change this same step is never lost. */
export function advanceBackfillGuard(
  state: BackfillGuardState,
  outcome: BackfillOutcome,
  repoIds: readonly number[],
): BackfillGuardState {
  const consecutiveEmpty =
    outcome.kind === "nothing-found"
      ? state.consecutiveEmpty + 1
      : outcome.kind === "added"
        ? 0
        : state.consecutiveEmpty;
  return { repoIds: [...repoIds], consecutiveEmpty };
}

/** Whether the guard's current streak means "stop offering another step". */
export function backfillExhausted(state: BackfillGuardState): boolean {
  return state.consecutiveEmpty >= EMPTY_STEP_LIMIT;
}

/** Why the control is disabled when `backfillExhausted` is true. */
export const NOTHING_OLDER_MESSAGE = "Nothing older to fetch";
