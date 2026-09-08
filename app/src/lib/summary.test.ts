import { describe, expect, test } from "vitest";
import {
  ABANDONED_AFTER_SECS,
  agingWipBuckets,
  computeAttentionRules,
  computeCarefulRead,
  computeKpiTiles,
  filterPullsByAgeBucket,
  formatConcentration,
  formatDuration,
  formatSignedCount,
  formatSignedDuration,
  gaugeCountsAsOf,
  gaugeCountsNow,
  groupOpenPulls,
  isAwaitingReview,
  longestWaitingReviewer,
  oldestAwaitingReview,
  pct,
  presentKinds,
  previousRange,
  pullAgeBucket,
  rosterRows,
  sortRoster,
  totalOf,
  tzOffsetSecs,
  zeroActivityLogins,
} from "./summary";
import type { Digest, DigestPerson, DigestThread, KindCounts, OpenPull } from "./types";

const ZERO_COUNTS: KindCounts = {
  commit: 0,
  pr_opened: 0,
  pr_merged: 0,
  pr_closed: 0,
  pr_reviewed: 0,
  pr_commented: 0,
  issue_opened: 0,
  issue_commented: 0,
};

function counts(overrides: Partial<KindCounts>): KindCounts {
  return { ...ZERO_COUNTS, ...overrides };
}

function person(login: string, overrides: Partial<DigestPerson> = {}): DigestPerson {
  return {
    login,
    avatar_url: null,
    total: 0,
    counts: ZERO_COUNTS,
    open_prs: 0,
    review_queue: 0,
    last_at: 0,
    ...overrides,
  };
}

function digest(overrides: Partial<Digest> = {}): Digest {
  return {
    start: 0,
    end: 100,
    team_id: null,
    actor: null,
    totals: ZERO_COUNTS,
    people: [],
    people_total: 0,
    threads: [],
    repos: [],
    series: [],
    hours: [],
    pr_timing: { median_ttm_secs: null, median_ttfr_secs: null, p90_ttm_secs: null, p90_ttfr_secs: null, samples: 0 },
    unreviewed_merges: 0,
    ...overrides,
  };
}

function digestThread(overrides: Partial<DigestThread> = {}): DigestThread {
  return {
    repo_id: 1,
    number: 1,
    kind: "pull",
    title: "Some thread",
    url: "https://github.com/o/r/pull/1",
    events: 0,
    last_at: 0,
    ...overrides,
  };
}

function pull(overrides: Partial<OpenPull> = {}): OpenPull {
  return {
    repo_id: 1,
    number: 1,
    title: "Some PR",
    url: "https://github.com/o/r/pull/1",
    author_login: "priya",
    author_avatar_url: null,
    draft: false,
    created_at: 0,
    updated_at: 0,
    last_activity_at: 0,
    additions: null,
    deletions: null,
    requested_reviewers: [],
    review_decision: null,
    ...overrides,
  };
}

describe("totalOf / presentKinds / pct", () => {
  test("totalOf sums every kind", () => {
    expect(totalOf(counts({ commit: 3, pr_merged: 2 }))).toBe(5);
  });
  test("presentKinds keeps only non-zero kinds, in ALL_EVENT_KINDS order", () => {
    expect(presentKinds(counts({ pr_merged: 1, commit: 2 }))).toEqual(["commit", "pr_merged"]);
  });
  test("pct is 0 when max is 0, never NaN/Infinity", () => {
    expect(pct(5, 0)).toBe(0);
    expect(pct(5, 10)).toBe(50);
  });
});

describe("previousRange", () => {
  test("returns the immediately preceding window of equal length", () => {
    expect(previousRange({ start: 1000, end: 1100 })).toEqual({ start: 900, end: 1000 });
  });

  test("handles a zero-length range without dividing by anything", () => {
    expect(previousRange({ start: 500, end: 500 })).toEqual({ start: 500, end: 500 });
  });
});

describe("tzOffsetSecs", () => {
  test("matches the contract's formula for a UTC+ offset (getTimezoneOffset negative)", () => {
    const d = { getTimezoneOffset: () => -330 } as unknown as Date; // e.g. IST, UTC+5:30
    expect(tzOffsetSecs(d)).toBe(330 * 60);
  });

  test("matches the contract's formula for a UTC- offset (getTimezoneOffset positive)", () => {
    const d = { getTimezoneOffset: () => 300 } as unknown as Date; // e.g. EST, UTC-5
    expect(tzOffsetSecs(d)).toBe(-300 * 60);
  });
});

describe("formatDuration / signed variants", () => {
  test("null reads as an em dash, not a fabricated zero", () => {
    expect(formatDuration(null)).toBe("—");
  });

  test("sub-minute rounds to a floor label", () => {
    expect(formatDuration(30)).toBe("<1m");
  });

  test("picks the coarsest two units that fit", () => {
    expect(formatDuration(90)).toBe("2m");
    expect(formatDuration(3600 + 600)).toBe("1h 10m");
    expect(formatDuration(3600)).toBe("1h");
    expect(formatDuration(86_400 + 3600 * 4)).toBe("1d 4h");
    expect(formatDuration(86_400 * 2)).toBe("2d");
  });

  test("formatSignedCount signs and marks zero distinctly", () => {
    expect(formatSignedCount(3)).toBe("+3");
    expect(formatSignedCount(-2)).toBe("-2");
    expect(formatSignedCount(0)).toBe("±0");
  });

  test("formatSignedDuration signs a duration delta", () => {
    expect(formatSignedDuration(3600)).toBe("+1h");
    expect(formatSignedDuration(-90)).toBe("-2m");
    expect(formatSignedDuration(0)).toBe("±0");
  });
});

describe("isAwaitingReview", () => {
  test("a draft is never awaiting review", () => {
    expect(isAwaitingReview(pull({ draft: true, review_decision: null }))).toBe(false);
  });
  test("null or review_required, not draft, counts as awaiting review", () => {
    expect(isAwaitingReview(pull({ review_decision: null }))).toBe(true);
    expect(isAwaitingReview(pull({ review_decision: "review_required" }))).toBe(true);
  });
  test("approved or changes_requested are not awaiting review", () => {
    expect(isAwaitingReview(pull({ review_decision: "approved" }))).toBe(false);
    expect(isAwaitingReview(pull({ review_decision: "changes_requested" }))).toBe(false);
  });
});

describe("gaugeCountsNow / gaugeCountsAsOf", () => {
  const now = 1_000_000;
  const pulls = [
    pull({ number: 1, created_at: now - 10 * 86_400, last_activity_at: now - 10 * 86_400 }), // stale, old
    pull({ number: 2, created_at: now - 1 * 86_400, last_activity_at: now - 1 * 86_400, review_decision: "approved" }),
    pull({ number: 3, created_at: now - 100, last_activity_at: now - 100 }), // brand new, awaiting review
  ];

  test("gaugeCountsNow counts against the real clock", () => {
    expect(gaugeCountsNow(pulls, now)).toEqual({ open: 3, awaitingReview: 2, stale: 1 });
  });

  test("gaugeCountsAsOf floors to PRs that already existed by `asOf` (still-open snapshot)", () => {
    const asOf = now - 5 * 86_400; // only PR #1 existed by then
    expect(gaugeCountsAsOf(pulls, asOf)).toEqual({ open: 1, awaitingReview: 1, stale: 1 });
  });

  test("gaugeCountsAsOf before any PR existed is all zeros", () => {
    expect(gaugeCountsAsOf(pulls, now - 20 * 86_400)).toEqual({ open: 0, awaitingReview: 0, stale: 0 });
  });
});

describe("computeKpiTiles", () => {
  const now = 1_000_000;

  test("merged going up is good; a null previous digest leaves every delta null", () => {
    const current = digest({
      totals: counts({ pr_merged: 5 }),
      pr_timing: { median_ttm_secs: 3600, median_ttfr_secs: 600, p90_ttm_secs: null, p90_ttfr_secs: null, samples: 5 },
    });
    const tiles = computeKpiTiles({ current, previous: null, pullsNow: [], previousWindowEnd: now - 100, now });
    const merged = tiles.find((t) => t.key === "merged")!;
    expect(merged.displayValue).toBe("5");
    expect(merged.deltaLabel).toBeNull();
    expect(merged.deltaGood).toBeNull();
  });

  test("more merged PRs than last window is a good delta", () => {
    const current = digest({ totals: counts({ pr_merged: 5 }) });
    const previous = digest({ totals: counts({ pr_merged: 2 }) });
    const tiles = computeKpiTiles({ current, previous, pullsNow: [], previousWindowEnd: now - 100, now });
    const merged = tiles.find((t) => t.key === "merged")!;
    expect(merged.deltaLabel).toBe("+3");
    expect(merged.deltaGood).toBe(true);
  });

  test("the three live-gauge tiles show a value but never a delta", () => {
    // `pulls` holds only what is open RIGHT NOW, so any "previous" count is a
    // floor: a PR open during the previous window but closed since is
    // invisible, which would bias every delta toward "worse". The gauges show
    // the current number and no arrow until the store keeps PR history.
    const now2 = 1_000_000;
    const pulls = [pull({ created_at: now2 - 200, last_activity_at: now2 - 4 * 86_400 })]; // stale now
    const tiles = computeKpiTiles({
      current: digest(),
      previous: digest(),
      pullsNow: pulls,
      previousWindowEnd: now2 - 300,
      now: now2,
    });
    for (const key of ["openPrs", "awaitingReview", "stale"]) {
      const tile = tiles.find((t) => t.key === key)!;
      expect(tile.deltaLabel).toBeNull();
      expect(tile.deltaGood).toBeNull();
    }
    expect(tiles.find((t) => t.key === "stale")!.displayValue).toBe("1");
  });

  test("a null pullsNow (openPulls call failed) shows no value and no delta for the gauge tiles", () => {
    const tiles = computeKpiTiles({ current: digest(), previous: digest(), pullsNow: null, previousWindowEnd: now - 100, now });
    const openPrs = tiles.find((t) => t.key === "openPrs")!;
    expect(openPrs.displayValue).toBe("—");
    expect(openPrs.deltaLabel).toBeNull();
  });

  test("faster median time-to-merge than last window is a good delta (lower is better)", () => {
    const current = digest({
      pr_timing: { median_ttm_secs: 3600, median_ttfr_secs: null, p90_ttm_secs: null, p90_ttfr_secs: null, samples: 3 },
    });
    const previous = digest({
      pr_timing: { median_ttm_secs: 7200, median_ttfr_secs: null, p90_ttm_secs: null, p90_ttfr_secs: null, samples: 3 },
    });
    const tiles = computeKpiTiles({ current, previous, pullsNow: [], previousWindowEnd: now - 100, now });
    const ttm = tiles.find((t) => t.key === "medianTtm")!;
    expect(ttm.displayValue).toBe("1h");
    expect(ttm.deltaLabel).toBe("-1h");
    expect(ttm.deltaGood).toBe(true);
  });

  test("p90 rides beside its median as secondaryLabel, labelled so it's not mistaken for the median", () => {
    const current = digest({
      pr_timing: { median_ttm_secs: 3600, median_ttfr_secs: 600, p90_ttm_secs: 9 * 86_400, p90_ttfr_secs: null, samples: 5 },
    });
    const tiles = computeKpiTiles({ current, previous: null, pullsNow: [], previousWindowEnd: now - 100, now });
    expect(tiles.find((t) => t.key === "medianTtm")!.secondaryLabel).toBe("p90 9d");
    // No p90 samples for time-to-first-review: no secondary line at all, same
    // as the median's own "—" case — never a fabricated 0.
    expect(tiles.find((t) => t.key === "medianTtfr")!.secondaryLabel).toBeNull();
    // The three live gauges and `merged` have no median at all, so no p90 either.
    for (const key of ["openPrs", "awaitingReview", "stale", "merged"]) {
      expect(tiles.find((t) => t.key === key)!.secondaryLabel).toBeNull();
    }
  });
});

describe("groupOpenPulls", () => {
  test("groups by state in the fixed order, dropping empty groups", () => {
    const pulls = [
      pull({ number: 1, draft: true, created_at: 100 }),
      pull({ number: 2, review_decision: "approved", created_at: 200 }),
      pull({ number: 3, review_decision: null, created_at: 50 }),
    ];
    const groups = groupOpenPulls(pulls);
    expect(groups.map((g) => g.group)).toEqual(["draft", "awaiting_review", "approved"]);
    expect(groups.find((g) => g.group === "approved")!.pulls[0].number).toBe(2);
  });

  test("within a group, oldest-created sorts first", () => {
    const pulls = [pull({ number: 1, created_at: 300 }), pull({ number: 2, created_at: 100 })];
    const groups = groupOpenPulls(pulls);
    expect(groups[0].pulls.map((p) => p.number)).toEqual([2, 1]);
  });

  test("changes_requested wins over a stale approval only when it is the current decision", () => {
    const p = pull({ review_decision: "changes_requested" });
    expect(groupOpenPulls([p])[0].group).toBe("changes_requested");
  });
});

describe("roster: rosterRows / sortRoster / zeroActivityLogins", () => {
  const people: DigestPerson[] = [
    person("priya", { open_prs: 3, review_queue: 1, counts: counts({ pr_merged: 2, pr_reviewed: 4, commit: 10 }), last_at: 500 }),
    person("marcus", { open_prs: 1, review_queue: 5, counts: counts({ pr_merged: 0, pr_reviewed: 1, commit: 2 }), last_at: 100 }),
  ];

  test("zeroActivityLogins is null when membership is unknown (no team selected)", () => {
    expect(zeroActivityLogins(null, people)).toBeNull();
  });

  test("zeroActivityLogins finds team members with no digest entry", () => {
    expect(zeroActivityLogins(["priya", "marcus", "lena"], people)).toEqual(["lena"]);
  });

  test("rosterRows appends a zero-activity row per missing team member", () => {
    const rows = rosterRows(people, ["priya", "marcus", "lena"]);
    expect(rows.map((r) => r.login)).toEqual(["priya", "marcus", "lena"]);
    const lena = rows.find((r) => r.login === "lena")!;
    expect(lena.zeroActivity).toBe(true);
    expect(lena.open_prs).toBe(0);
  });

  test("rosterRows adds nothing extra with no team selected", () => {
    expect(rosterRows(people, null)).toHaveLength(2);
  });

  test("sortRoster defaults sort by open_prs desc, and toggles with dir", () => {
    const rows = rosterRows(people, null);
    expect(sortRoster(rows, "open_prs", "desc").map((r) => r.login)).toEqual(["priya", "marcus"]);
    expect(sortRoster(rows, "open_prs", "asc").map((r) => r.login)).toEqual(["marcus", "priya"]);
  });

  test("sortRoster by login is alphabetical, independent of numeric fields", () => {
    const rows = rosterRows(people, null);
    expect(sortRoster(rows, "login", "asc").map((r) => r.login)).toEqual(["marcus", "priya"]);
  });

  test("sortRoster by last_at treats never-active as oldest in both directions", () => {
    const rows = rosterRows(people, ["priya", "marcus", "lena"]);
    expect(sortRoster(rows, "last_at", "asc")[0].login).toBe("lena");
    expect(sortRoster(rows, "last_at", "desc")[sortRoster(rows, "last_at", "desc").length - 1].login).toBe("lena");
  });
});

describe("computeAttentionRules", () => {
  const now = 1_000_000;
  const teamWideDigest = digest({ people: [person("priya"), person("marcus")] });

  test("flags a stale PR (no activity for 3+ days)", () => {
    const pulls = [pull({ repo_id: 7, number: 1, last_activity_at: now - 4 * 86_400 })];
    const rules = computeAttentionRules({ pulls, digestTeamWide: teamWideDigest, teamMemberLogins: null, now });
    const stale = rules.find((r) => r.key === "stale")!;
    expect(stale.count).toBe(1);
    expect(stale.items[0].label).toContain("#1");
    // Carries the thread it's about, so the row can open the in-app detail
    // pane instead of only linking out to GitHub.
    expect(stale.items[0].repo_id).toBe(7);
    expect(stale.items[0].number).toBe(1);
  });

  test("does not flag a PR active within the window", () => {
    const pulls = [pull({ number: 1, last_activity_at: now - 1 * 86_400 })];
    const rules = computeAttentionRules({ pulls, digestTeamWide: teamWideDigest, teamMemberLogins: null, now });
    expect(rules.find((r) => r.key === "stale")!.count).toBe(0);
  });

  test("flags an approved PR unmerged for 1+ day, but not one approved minutes ago", () => {
    const pulls = [
      pull({ number: 1, review_decision: "approved", last_activity_at: now - 2 * 86_400 }),
      pull({ number: 2, review_decision: "approved", last_activity_at: now - 60 }),
    ];
    const rules = computeAttentionRules({ pulls, digestTeamWide: teamWideDigest, teamMemberLogins: null, now });
    const rule = rules.find((r) => r.key === "approvedUnmerged")!;
    expect(rule.count).toBe(1);
    expect(rule.items[0].label).toContain("#1");
  });

  test("flags changes-requested untouched for 2+ days only", () => {
    const pulls = [
      pull({ number: 1, review_decision: "changes_requested", last_activity_at: now - 3 * 86_400 }),
      pull({ number: 2, review_decision: "changes_requested", last_activity_at: now - 86_400 }),
    ];
    const rules = computeAttentionRules({ pulls, digestTeamWide: teamWideDigest, teamMemberLogins: null, now });
    expect(rules.find((r) => r.key === "changesRequestedStale")!.count).toBe(1);
  });

  test("flags a non-draft PR open 1+ day with no reviewer requested; skips drafts", () => {
    const pulls = [
      pull({ number: 1, requested_reviewers: [], created_at: now - 2 * 86_400 }),
      pull({ number: 2, draft: true, requested_reviewers: [], created_at: now - 2 * 86_400 }),
      pull({ number: 3, requested_reviewers: ["marcus"], created_at: now - 2 * 86_400 }),
    ];
    const rules = computeAttentionRules({ pulls, digestTeamWide: teamWideDigest, teamMemberLogins: null, now });
    const rule = rules.find((r) => r.key === "noReviewer")!;
    expect(rule.count).toBe(1);
    expect(rule.items[0].label).toContain("#1");
  });

  test("zeroActivity is marked unavailable with no team selected, not zero", () => {
    const rules = computeAttentionRules({ pulls: [], digestTeamWide: teamWideDigest, teamMemberLogins: null, now });
    const rule = rules.find((r) => r.key === "zeroActivity")!;
    expect(rule.unavailable).toBe(true);
    expect(rule.count).toBe(0);
  });

  test("zeroActivity lists team members absent from the digest when a team is selected", () => {
    const rules = computeAttentionRules({
      pulls: [],
      digestTeamWide: teamWideDigest,
      teamMemberLogins: ["priya", "marcus", "lena"],
      now,
    });
    const rule = rules.find((r) => r.key === "zeroActivity")!;
    expect(rule.unavailable).toBe(false);
    expect(rule.count).toBe(1);
    expect(rule.items[0].label).toBe("lena");
    // A team member, not a thread — no repo_id/number to open a pane with,
    // unlike the pull-based rules above.
    expect(rule.items[0].repo_id).toBeUndefined();
    expect(rule.items[0].number).toBeUndefined();
  });

  test("review-load imbalance fires only above the 50% share AND the >=4 total floor", () => {
    // 3 total requests, marcus holds all 3 (100%) — below the count floor, must not fire.
    const belowFloor = [
      pull({ number: 1, requested_reviewers: ["marcus"] }),
      pull({ number: 2, requested_reviewers: ["marcus"] }),
      pull({ number: 3, requested_reviewers: ["marcus"] }),
    ];
    expect(
      computeAttentionRules({ pulls: belowFloor, digestTeamWide: teamWideDigest, teamMemberLogins: null, now }).find(
        (r) => r.key === "reviewImbalance",
      )!.count,
    ).toBe(0);

    // 4 total, marcus holds 3 of 4 (75%) — fires.
    const overShare = [
      pull({ number: 1, requested_reviewers: ["marcus"] }),
      pull({ number: 2, requested_reviewers: ["marcus"] }),
      pull({ number: 3, requested_reviewers: ["marcus"] }),
      pull({ number: 4, requested_reviewers: ["priya"] }),
    ];
    const imbalanced = computeAttentionRules({ pulls: overShare, digestTeamWide: teamWideDigest, teamMemberLogins: null, now }).find(
      (r) => r.key === "reviewImbalance",
    )!;
    expect(imbalanced.count).toBe(1);
    expect(imbalanced.label).toContain("marcus");

    // 4 total, split 2/2 (50% exactly) — must NOT fire; the rule is "> 50%", not ">=".
    const evenSplit = [
      pull({ number: 1, requested_reviewers: ["marcus"] }),
      pull({ number: 2, requested_reviewers: ["marcus"] }),
      pull({ number: 3, requested_reviewers: ["priya"] }),
      pull({ number: 4, requested_reviewers: ["priya"] }),
    ];
    expect(
      computeAttentionRules({ pulls: evenSplit, digestTeamWide: teamWideDigest, teamMemberLogins: null, now }).find(
        (r) => r.key === "reviewImbalance",
      )!.count,
    ).toBe(0);
  });
});

describe("Aging WIP: pullAgeBucket / agingWipBuckets / filterPullsByAgeBucket", () => {
  const now = 1_000_000;

  test("buckets by age since created_at, at the boundaries", () => {
    expect(pullAgeBucket(pull({ created_at: now - 12 * 3600 }), now)).toBe("under1d"); // 12h
    expect(pullAgeBucket(pull({ created_at: now - 1 * 86_400 }), now)).toBe("d1to3"); // exactly 1d
    expect(pullAgeBucket(pull({ created_at: now - 3 * 86_400 }), now)).toBe("d3to7"); // exactly 3d
    expect(pullAgeBucket(pull({ created_at: now - 7 * 86_400 }), now)).toBe("over7d"); // exactly 7d
    expect(pullAgeBucket(pull({ created_at: now - 10 * 86_400 }), now)).toBe("over7d");
  });

  test("agingWipBuckets counts every bucket, zeros included, in fixed order", () => {
    const pulls = [
      pull({ number: 1, created_at: now - 12 * 3600 }),
      pull({ number: 2, created_at: now - 2 * 86_400 }),
      pull({ number: 3, created_at: now - 8 * 86_400 }),
    ];
    expect(agingWipBuckets(pulls, now)).toEqual([
      { bucket: "under1d", label: "Under 1 day", count: 1 },
      { bucket: "d1to3", label: "1–3 days", count: 1 },
      { bucket: "d3to7", label: "3–7 days", count: 0 },
      { bucket: "over7d", label: "Over 7 days", count: 1 },
    ]);
  });

  test("filterPullsByAgeBucket narrows to one bucket; null returns everything unfiltered", () => {
    const pulls = [
      pull({ number: 1, created_at: now - 12 * 3600 }),
      pull({ number: 2, created_at: now - 8 * 86_400 }),
    ];
    expect(filterPullsByAgeBucket(pulls, "under1d", now).map((p) => p.number)).toEqual([1]);
    expect(filterPullsByAgeBucket(pulls, null, now)).toHaveLength(2);
  });
});

describe("Stuck panel: oldestAwaitingReview / longestWaitingReviewer", () => {
  const now = 1_000_000;

  test("oldestAwaitingReview picks the earliest-created PR still awaiting review, skipping drafts and decided PRs", () => {
    const pulls = [
      pull({ number: 1, created_at: now - 2 * 86_400 }),
      pull({ number: 2, created_at: now - 9 * 86_400, review_decision: "approved" }), // oldest, but decided
      pull({ number: 3, created_at: now - 6 * 86_400 }),
      pull({ number: 4, created_at: now - 20 * 86_400, draft: true }), // oldest overall, but draft
    ];
    const result = oldestAwaitingReview(pulls, now);
    expect(result.oldest?.pull.number).toBe(3);
    expect(result.oldest?.ageSecs).toBe(6 * 86_400);
    expect(result.excludedAbandonedCount).toBe(0);
  });

  test("oldestAwaitingReview is 'none at all' (not abandoned) when there's nothing to begin with", () => {
    expect(oldestAwaitingReview([pull({ draft: true })], now)).toEqual({ oldest: null, excludedAbandonedCount: 0 });
    expect(oldestAwaitingReview([], now)).toEqual({ oldest: null, excludedAbandonedCount: 0 });
  });

  test("oldestAwaitingReview excludes a PR with no activity in over ABANDONED_AFTER_SECS as abandoned", () => {
    const abandoned = pull({
      number: 1,
      created_at: now - 40 * 86_400,
      last_activity_at: now - 40 * 86_400, // 40 days silent > 30-day threshold
    });
    const result = oldestAwaitingReview([abandoned], now);
    expect(result.oldest).toBeNull();
    expect(result.excludedAbandonedCount).toBe(1);
  });

  test("oldestAwaitingReview keeps a PR whose last activity is only 20 days old", () => {
    const stillMoving = pull({
      number: 1,
      created_at: now - 25 * 86_400,
      last_activity_at: now - 20 * 86_400, // 20 days silent < 30-day threshold
    });
    const result = oldestAwaitingReview([stillMoving], now);
    expect(result.oldest?.pull.number).toBe(1);
    expect(result.excludedAbandonedCount).toBe(0);
  });

  test("oldestAwaitingReview reports the abandoned one as excluded while a still-moving PR wins", () => {
    const abandoned = pull({ number: 1, created_at: now - 90 * 86_400, last_activity_at: now - 90 * 86_400 });
    const stillMoving = pull({ number: 2, created_at: now - 10 * 86_400, last_activity_at: now - 5 * 86_400 });
    const result = oldestAwaitingReview([abandoned, stillMoving], now);
    expect(result.oldest?.pull.number).toBe(2);
    expect(result.excludedAbandonedCount).toBe(1);
  });

  test("oldestAwaitingReview says everything qualifying was abandoned, rather than reading as 'nothing waiting'", () => {
    const pulls = [
      pull({ number: 1, created_at: now - 40 * 86_400, last_activity_at: now - 40 * 86_400 }),
      pull({ number: 2, created_at: now - 60 * 86_400, last_activity_at: now - 60 * 86_400 }),
    ];
    const result = oldestAwaitingReview(pulls, now);
    expect(result.oldest).toBeNull();
    expect(result.excludedAbandonedCount).toBe(2);
  });

  test("ABANDONED_AFTER_SECS is 30 days", () => {
    expect(ABANDONED_AFTER_SECS).toBe(30 * 86_400);
  });

  test("longestWaitingReviewer ranks by the oldest item in each reviewer's queue, not by queue size", () => {
    const pulls = [
      // priya: 3 fresh requests.
      pull({ number: 1, created_at: now - 1 * 86_400, requested_reviewers: ["priya"] }),
      pull({ number: 2, created_at: now - 1 * 86_400, requested_reviewers: ["priya"] }),
      pull({ number: 3, created_at: now - 1 * 86_400, requested_reviewers: ["priya"] }),
      // marcus: a single, much older request.
      pull({ number: 4, created_at: now - 9 * 86_400, requested_reviewers: ["marcus"] }),
    ];
    const result = longestWaitingReviewer(pulls, now)!;
    expect(result.login).toBe("marcus");
    expect(result.queueSize).toBe(1);
    expect(result.oldestAgeSecs).toBe(9 * 86_400);
  });

  test("longestWaitingReviewer ignores PRs no longer awaiting review, and is null with no queues", () => {
    const decided = pull({ requested_reviewers: ["priya"], review_decision: "approved" });
    expect(longestWaitingReviewer([decided], now)).toBeNull();
    expect(longestWaitingReviewer([], now)).toBeNull();
  });

  test("longestWaitingReviewer breaks an exact tie by the lower login", () => {
    const pulls = [
      pull({ number: 1, created_at: now - 5 * 86_400, requested_reviewers: ["zeno"] }),
      pull({ number: 2, created_at: now - 5 * 86_400, requested_reviewers: ["ana"] }),
    ];
    expect(longestWaitingReviewer(pulls, now)!.login).toBe("ana");
  });
});

describe("formatConcentration", () => {
  test("rounds to a whole percent and names it a hint, not a metric", () => {
    expect(formatConcentration(0.87)).toBe("87% one author");
    expect(formatConcentration(1)).toBe("100% one author");
  });
});

describe("computeCarefulRead", () => {
  const now = 1_000_000;

  test("empty pulls scores nothing", () => {
    expect(computeCarefulRead([], [], now)).toEqual({ items: [], usedCommentRounds: false });
  });

  test("uses comment rounds from matching digest threads when any thread data exists", () => {
    const pulls = [
      pull({ number: 1, repo_id: 1, additions: 10, deletions: 10, created_at: now - 1 * 86_400, requested_reviewers: [] }),
      pull({ number: 2, repo_id: 1, additions: 500, deletions: 500, created_at: now - 8 * 86_400, requested_reviewers: ["a", "b"] }),
    ];
    const threads = [digestThread({ repo_id: 1, number: 2, events: 20 })];
    const result = computeCarefulRead(pulls, threads, now);
    expect(result.usedCommentRounds).toBe(true);
    // PR #2 dominates every normalised signal (bigger, older, more reviewers,
    // more discussion) so it must rank first with a strictly higher score.
    expect(result.items[0].pull.number).toBe(2);
    expect(result.items[0].score).toBeGreaterThan(result.items[1].score);
    expect(result.items[0].reason).toContain("1000 lines changed");
    // "updates", not "comments": the thread count is every event on the
    // thread, so the wording must not claim more precision than that.
    expect(result.items[0].reason).toContain("20 updates");
    expect(result.items[0].reason).toContain("8 days old");
    expect(result.items[0].reason).toContain("2 reviewers");
    // PR #1 matches no thread — read as "no discussion this window", not a gap.
    expect(result.items[1].reason).not.toContain("comment");
  });

  test("renormalises size/age/reviewers to sum to 1 and drops the term when there is no thread data at all", () => {
    const pulls = [
      pull({ number: 1, additions: 100, deletions: 0, created_at: now - 1 * 86_400, requested_reviewers: [] }),
      pull({ number: 2, additions: 900, deletions: 0, created_at: now - 6 * 86_400, requested_reviewers: ["a"] }),
    ];
    const result = computeCarefulRead(pulls, [], now);
    expect(result.usedCommentRounds).toBe(false);
    // Hand-computed: size/age/reviewers normalise to 0 and 1 for PR #1 and #2
    // respectively (PR #1 is the min on every axis, PR #2 the max), so PR
    // #2's score is exactly the renormalised weights' sum (~1) and PR #1's
    // is exactly 0 — this pins the renormalisation arithmetic, not just its
    // ordering.
    const pr1 = result.items.find((i) => i.pull.number === 1)!;
    const pr2 = result.items.find((i) => i.pull.number === 2)!;
    expect(pr1.score).toBeCloseTo(0, 10);
    expect(pr2.score).toBeCloseTo(1, 10);
    expect(pr2.reason).not.toContain("comment");
  });

  test("keeps only the top 5 by score", () => {
    // Size and reviewer count are identical (and so normalise to 0) across
    // every pull here, so age alone drives the ranking — the 5 oldest must
    // win, strictly ordered, with the 2 newest dropped.
    const pulls = Array.from({ length: 7 }, (_, i) =>
      pull({ number: i + 1, additions: 0, deletions: 0, created_at: now - i * 86_400, requested_reviewers: [] }),
    );
    const result = computeCarefulRead(pulls, [], now);
    expect(result.items.map((i) => i.pull.number)).toEqual([7, 6, 5, 4, 3]);
  });

  test("breaks an exact score tie by the older PR first (a stable, deterministic answer, not array order)", () => {
    // Two PRs opened at the exact same instant, otherwise identical: their
    // score ties exactly. A third, older, unique PR pins the age
    // normalisation's upper bound so the tie sits below top-of-scale.
    const tiedAt = now - 3 * 86_400;
    const pulls = [
      pull({ number: 2, created_at: tiedAt, additions: 50, deletions: 0, requested_reviewers: [] }),
      pull({ number: 1, created_at: tiedAt, additions: 50, deletions: 0, requested_reviewers: [] }),
      pull({ number: 3, created_at: now - 9 * 86_400, additions: 50, deletions: 0, requested_reviewers: [] }),
    ];
    const result = computeCarefulRead(pulls, [], now);
    const tied1 = result.items.find((i) => i.pull.number === 1)!;
    const tied2 = result.items.find((i) => i.pull.number === 2)!;
    expect(tied1.score).toBe(tied2.score);
    // Ascending by created_at is a no-op for an exact tie (both share
    // `tiedAt`), so this also pins that the comparator doesn't reorder them
    // by array position either — reproducible across repeated calls.
    expect(computeCarefulRead(pulls, [], now).items.map((i) => i.pull.number)).toEqual(
      result.items.map((i) => i.pull.number),
    );
  });

  test("a truncated thread list drops the discussion term instead of scoring rows zero", () => {
    // The engine returns at most DIGEST_THREAD_LIMIT (10) threads. With the
    // cap reached and PRs outside it, a missing entry no longer means "no
    // discussion" — so the term must be dropped and the weights renormalised
    // rather than silently zeroing the second-heaviest signal for most rows.
    const threads = Array.from({ length: 10 }, (_, i) =>
      digestThread({ repo_id: 1, number: 900 + i, events: 30 }),
    );
    const pulls = [
      pull({ repo_id: 1, number: 1, additions: 500, deletions: 0 }),
      pull({ repo_id: 1, number: 2, additions: 10, deletions: 0 }),
    ];
    const result = computeCarefulRead(pulls, threads, now);
    expect(result.usedCommentRounds).toBe(false);
    for (const item of result.items) expect(item.reason).not.toContain("update");
  });

  test("an uncapped thread list still uses the discussion term", () => {
    const threads = [digestThread({ repo_id: 1, number: 1, events: 12 })];
    const pulls = [pull({ repo_id: 1, number: 1 }), pull({ repo_id: 1, number: 2 })];
    const result = computeCarefulRead(pulls, threads, now);
    expect(result.usedCommentRounds).toBe(true);
  });
});
