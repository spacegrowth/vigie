import { describe, expect, test } from "vitest";
import {
  BACKFILL_STEP_SPAN_SECS,
  EMPTY_STEP_LIMIT,
  INITIAL_BACKFILL_GUARD,
  LOW_BUDGET_THRESHOLD,
  NOTHING_OLDER_MESSAGE,
  advanceBackfillGuard,
  backfillBudgetLow,
  clearBackfillGuardOnNewEvents,
  backfillExhausted,
  backfillStepLabel,
  classifyBackfillResult,
  describeBackfillOutcome,
  describeLowBudget,
  describeNewRepoBackfill,
  describeRequestsSpent,
  effectiveBackfillGuard,
  formatDateRange,
  requestsSpent,
  stepNeedsFirstPoll,
  type BackfillGuardState,
  type BackfillOutcome,
} from "./backfill";
import type { BackfillResult, RepoBackfill } from "./types";

function step(overrides: Partial<RepoBackfill> = {}): RepoBackfill {
  return { repo_id: 1, from: 0, to: 0, inserted: 0, error: null, ...overrides };
}

function result(repos: RepoBackfill[], rate_limit_remaining: number | null = null): BackfillResult {
  return { repos, rate_limit_remaining, rate_limit_reset_at: null, rate_limit_limit: null };
}

describe("BACKFILL_STEP_SPAN_SECS", () => {
  test("is exactly a week", () => {
    expect(BACKFILL_STEP_SPAN_SECS).toBe(7 * 24 * 60 * 60);
  });
});

describe("stepNeedsFirstPoll", () => {
  test("true when the one repo's step is unpolled", () => {
    const r = result([step({ repo_id: 5, error: "not polled yet" })]);
    expect(stepNeedsFirstPoll(r, 5)).toBe(true);
  });

  test("false on a clean step", () => {
    const r = result([step({ repo_id: 5, error: null })]);
    expect(stepNeedsFirstPoll(r, 5)).toBe(false);
  });

  test("false for a different error, and when the repo isn't in the result at all", () => {
    expect(stepNeedsFirstPoll(result([step({ repo_id: 5, error: "signed out" })]), 5)).toBe(false);
    expect(stepNeedsFirstPoll(result([]), 5)).toBe(false);
  });
});

describe("describeNewRepoBackfill", () => {
  test("missing row", () => {
    expect(describeNewRepoBackfill(result([]), 5)).toBe("Couldn't check for older activity.");
  });

  test("signed out", () => {
    const r = result([step({ repo_id: 5, error: "signed out" })]);
    expect(describeNewRepoBackfill(r, 5)).toBe("Signed out — sign in again to fetch its history.");
  });

  test("not polled yet", () => {
    const r = result([step({ repo_id: 5, error: "not polled yet" })]);
    expect(describeNewRepoBackfill(r, 5)).toBe("Couldn't fetch yet — this repo hasn't polled.");
  });

  test("a generic engine error", () => {
    const r = result([step({ repo_id: 5, error: "rate_limited: try later" })]);
    expect(describeNewRepoBackfill(r, 5)).toBe("Couldn't fetch older activity (rate_limited: try later).");
  });

  test("window truncated but still counts what it inserted", () => {
    const r = result([step({ repo_id: 5, error: "window truncated", inserted: 3 })]);
    expect(describeNewRepoBackfill(r, 5)).toBe("Found 3 events from the last 7 days.");
  });

  test("clean step, nothing found", () => {
    const r = result([step({ repo_id: 5, inserted: 0 })]);
    expect(describeNewRepoBackfill(r, 5)).toBe("No activity in the last 7 days.");
  });

  test("clean step, singular vs. plural", () => {
    expect(describeNewRepoBackfill(result([step({ repo_id: 5, inserted: 1 })]), 5)).toBe(
      "Found 1 event from the last 7 days.",
    );
    expect(describeNewRepoBackfill(result([step({ repo_id: 5, inserted: 4 })]), 5)).toBe(
      "Found 4 events from the last 7 days.",
    );
  });
});

describe("classifyBackfillResult", () => {
  test("no repos at all", () => {
    expect(classifyBackfillResult(result([]))).toEqual({ kind: "all-skipped" });
  });

  test("every repo unpolled or signed out", () => {
    const r = result([
      step({ repo_id: 1, error: "not polled yet" }),
      step({ repo_id: 2, error: "signed out" }),
    ]);
    expect(classifyBackfillResult(r)).toEqual({ kind: "all-skipped" });
  });

  test("every repo that ran, failed", () => {
    const r = result([
      step({ repo_id: 1, error: "not polled yet" }),
      step({ repo_id: 2, error: "network unreachable" }),
    ]);
    expect(classifyBackfillResult(r)).toEqual({ kind: "all-failed" });
  });

  test("clean steps that inserted nothing", () => {
    const r = result([step({ repo_id: 1, inserted: 0 }), step({ repo_id: 2, inserted: 0, error: "window truncated" })]);
    expect(classifyBackfillResult(r)).toEqual({ kind: "nothing-found" });
  });

  test("a mix adds up only the clean repos, spanning their widest window", () => {
    const r = result([
      step({ repo_id: 1, from: 1_000, to: 1_500, inserted: 4 }),
      step({ repo_id: 2, from: 500, to: 1_000, inserted: 6, error: "window truncated" }),
      step({ repo_id: 3, error: "signed out" }),
    ]);
    expect(classifyBackfillResult(r)).toEqual({ kind: "added", count: 10, from: 500, to: 1_500 });
  });
});

describe("describeBackfillOutcome", () => {
  test("all-skipped", () => {
    expect(describeBackfillOutcome({ kind: "all-skipped" })).toBe(
      "Nothing to fetch yet — those repos haven't completed a first poll.",
    );
  });

  test("all-failed", () => {
    expect(describeBackfillOutcome({ kind: "all-failed" })).toBe("Couldn't fetch older activity right now.");
  });

  test("nothing-found", () => {
    expect(describeBackfillOutcome({ kind: "nothing-found" })).toBe("Nothing older found.");
  });

  test("added, singular vs. plural, with the date range folded in", () => {
    // 12 Aug local midnight -> 19 Aug local midnight (exclusive), same month.
    const from = new Date(2024, 7, 12).getTime() / 1000;
    const to = new Date(2024, 7, 19).getTime() / 1000;
    expect(describeBackfillOutcome({ kind: "added", count: 34, from, to })).toBe(
      `Added 34 events from ${formatDateRange(from, to)}.`,
    );
    expect(describeBackfillOutcome({ kind: "added", count: 1, from, to })).toBe(
      `Added 1 event from ${formatDateRange(from, to)}.`,
    );
  });
});

describe("formatDateRange", () => {
  test("same month", () => {
    const from = new Date(2024, 7, 12).getTime() / 1000; // 12 Aug
    const to = new Date(2024, 7, 19).getTime() / 1000; // 19 Aug, exclusive -> prints 18 Aug
    expect(formatDateRange(from, to)).toBe("12–18 Aug");
  });

  test("spans months", () => {
    const from = new Date(2024, 6, 28).getTime() / 1000; // 28 Jul
    const to = new Date(2024, 7, 4).getTime() / 1000; // 4 Aug, exclusive -> prints 3 Aug
    expect(formatDateRange(from, to)).toBe("28 Jul–3 Aug");
  });

  test("a one-second window never prints an end date before its start", () => {
    const from = new Date(2024, 7, 12).getTime() / 1000;
    expect(formatDateRange(from, from)).toBe("12–12 Aug");
  });

  test("under a filter, an added-events line says the step ran across all repos", () => {
    // A step is always fleet-wide; the visible feed may be narrower, so the
    // count must not read as a promise about what this list will show.
    const outcome = { kind: "added", count: 34, from: 1_723_000_000, to: 1_723_600_000 } as const;
    const plain = describeBackfillOutcome(outcome);
    const filtered = describeBackfillOutcome(outcome, true);
    expect(plain).toContain("Added 34 events");
    expect(plain).not.toContain("across all repos");
    expect(filtered).toContain("across all repos");
    expect(filtered).toContain("the current filter may hide them");
  });

  test("under a filter, nothing-found says it looked everywhere", () => {
    expect(describeBackfillOutcome({ kind: "nothing-found" }, true)).toBe("Nothing older found in any repo.");
    expect(describeBackfillOutcome({ kind: "nothing-found" })).toBe("Nothing older found.");
  });
});

describe("backfillStepLabel", () => {
  test("names the fixed span and the exact repo count", () => {
    expect(backfillStepLabel(15)).toBe("Load 7 more days · 15 repos");
  });

  test("singular repo", () => {
    expect(backfillStepLabel(1)).toBe("Load 7 more days · 1 repo");
  });

  test("zero repos still reads as a sentence, not a crash", () => {
    expect(backfillStepLabel(0)).toBe("Load 7 more days · 0 repos");
  });
});

describe("backfillBudgetLow / describeLowBudget", () => {
  test("below the threshold is low", () => {
    expect(backfillBudgetLow(LOW_BUDGET_THRESHOLD - 1)).toBe(true);
  });

  test("at or above the threshold is not low", () => {
    expect(backfillBudgetLow(LOW_BUDGET_THRESHOLD)).toBe(false);
    expect(backfillBudgetLow(5000)).toBe(false);
  });

  test("unknown remaining never gates", () => {
    expect(backfillBudgetLow(null)).toBe(false);
  });

  test("the message names the exact figure the status bar shows", () => {
    expect(describeLowBudget(312)).toBe("Paused — 312 GitHub requests left this hour");
  });
});

describe("requestsSpent / describeRequestsSpent", () => {
  test("the difference between the before and after readings", () => {
    expect(requestsSpent(5000, 4988)).toBe(12);
  });

  test("null when either reading is unknown", () => {
    expect(requestsSpent(null, 4988)).toBeNull();
    expect(requestsSpent(5000, null)).toBeNull();
    expect(requestsSpent(null, null)).toBeNull();
  });

  test("null rather than a negative or zero figure when nothing was spent", () => {
    // The hourly window can roll over mid-step and the count goes back up;
    // that is not a spend of -12 requests.
    expect(requestsSpent(10, 22)).toBeNull();
    expect(requestsSpent(10, 10)).toBeNull();
  });

  test("singular vs. plural wording", () => {
    expect(describeRequestsSpent(1)).toBe("1 request used.");
    expect(describeRequestsSpent(12)).toBe("12 requests used.");
  });
});

describe("effectiveBackfillGuard / advanceBackfillGuard / backfillExhausted", () => {
  const outcome = (kind: BackfillOutcome["kind"]): BackfillOutcome =>
    kind === "added" ? { kind, count: 3, from: 0, to: 1 } : { kind };

  test("starts unexhausted", () => {
    expect(backfillExhausted(INITIAL_BACKFILL_GUARD)).toBe(false);
  });

  test("one nothing-found step does not trip it — a quiet week is plausible", () => {
    const g1 = advanceBackfillGuard(INITIAL_BACKFILL_GUARD, outcome("nothing-found"), [1, 2]);
    expect(g1.consecutiveEmpty).toBe(1);
    expect(backfillExhausted(g1)).toBe(false);
  });

  test(`${EMPTY_STEP_LIMIT} consecutive nothing-found steps trip it`, () => {
    let g: BackfillGuardState = INITIAL_BACKFILL_GUARD;
    for (let i = 0; i < EMPTY_STEP_LIMIT; i++) {
      g = advanceBackfillGuard(effectiveBackfillGuard(g, [1, 2]), outcome("nothing-found"), [1, 2]);
    }
    expect(backfillExhausted(g)).toBe(true);
  });

  test("finding something always clears the streak", () => {
    let g: BackfillGuardState = { repoIds: [1, 2], consecutiveEmpty: EMPTY_STEP_LIMIT };
    expect(backfillExhausted(g)).toBe(true);
    g = advanceBackfillGuard(g, outcome("added"), [1, 2]);
    expect(g.consecutiveEmpty).toBe(0);
    expect(backfillExhausted(g)).toBe(false);
  });

  test("a skip or a failure neither extends nor clears the streak — it says nothing about older history", () => {
    const g0: BackfillGuardState = { repoIds: [1, 2], consecutiveEmpty: 1 };
    const afterSkip = advanceBackfillGuard(g0, outcome("all-skipped"), [1, 2]);
    expect(afterSkip.consecutiveEmpty).toBe(1);
    const afterFail = advanceBackfillGuard(g0, outcome("all-failed"), [1, 2]);
    expect(afterFail.consecutiveEmpty).toBe(1);
  });

  test("watching a different set of repos clears an already-tripped streak", () => {
    const tripped: BackfillGuardState = { repoIds: [1, 2], consecutiveEmpty: EMPTY_STEP_LIMIT };
    expect(backfillExhausted(tripped)).toBe(true);
    const reconciled = effectiveBackfillGuard(tripped, [1, 2, 3]); // a repo was added
    expect(backfillExhausted(reconciled)).toBe(false);
    expect(reconciled.repoIds).toEqual([1, 2, 3]);
  });

  test("watching the same set of repos (any order) leaves a tripped streak alone", () => {
    const tripped: BackfillGuardState = { repoIds: [1, 2], consecutiveEmpty: EMPTY_STEP_LIMIT };
    expect(backfillExhausted(effectiveBackfillGuard(tripped, [2, 1]))).toBe(true);
  });

  test("removing a repo also clears the streak — it was counted against a wider fleet", () => {
    const tripped: BackfillGuardState = { repoIds: [1, 2, 3], consecutiveEmpty: EMPTY_STEP_LIMIT };
    expect(backfillExhausted(effectiveBackfillGuard(tripped, [1, 2]))).toBe(false);
  });
});

describe(NOTHING_OLDER_MESSAGE, () => {
  test("is the disabled-control copy, not a log message", () => {
    expect(NOTHING_OLDER_MESSAGE).toBe("Nothing older to fetch");
  });

  test("a poll that stores new events clears a tripped guard", () => {
    // Otherwise the only escapes are changing the watched repos or
    // restarting: a fleet quiet for a fortnight would disable the control
    // for the whole session, which is harder than a soft cap should be.
    const tripped = { repoIds: [1, 2], consecutiveEmpty: 2 };
    expect(clearBackfillGuardOnNewEvents(tripped, 3)).toEqual({ repoIds: [1, 2], consecutiveEmpty: 0 });
  });

  test("a poll that stores nothing leaves the guard exactly as it was", () => {
    const tripped = { repoIds: [1, 2], consecutiveEmpty: 2 };
    expect(clearBackfillGuardOnNewEvents(tripped, 0)).toBe(tripped);
    const fresh = { repoIds: [1], consecutiveEmpty: 0 };
    expect(clearBackfillGuardOnNewEvents(fresh, 5)).toBe(fresh);
  });
});
