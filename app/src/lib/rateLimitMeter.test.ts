import { describe, expect, test } from "vitest";
import { DEFAULT_RATE_LIMIT, formatResetIn, rateLimitMeter } from "./rateLimitMeter";

describe("formatResetIn", () => {
  test("minutes: rounds to the nearest whole minute", () => {
    expect(formatResetIn(1_000 + 23 * 60, 1_000)).toBe("resets in 23 min");
    expect(formatResetIn(1_000 + 90, 1_000)).toBe("resets in 2 min"); // 90s rounds up
  });

  test("sub-minute: under 60s reads as 'under a minute', not '0 min'", () => {
    expect(formatResetIn(1_000 + 59, 1_000)).toBe("resets in under a minute");
    expect(formatResetIn(1_000 + 1, 1_000)).toBe("resets in under a minute");
  });

  test("a reset time already in the past reads as 'any moment', not negative minutes", () => {
    expect(formatResetIn(1_000 - 5, 1_000)).toBe("resets any moment");
    expect(formatResetIn(1_000, 1_000)).toBe("resets any moment");
  });

  test("unknown reset: null in, empty string out", () => {
    expect(formatResetIn(null, 1_000)).toBe("");
  });
});

describe("rateLimitMeter", () => {
  test("comfortable budget is muted, not a status colour", () => {
    const m = rateLimitMeter(4_019, 5_000, 1_000 + 23 * 60, 1_000);
    expect(m.tone).toBe("muted");
    expect(m.fraction).toBeCloseTo(4_019 / 5_000);
    expect(m.countText).toBe("4,019");
    expect(m.title).toBe("GitHub's hourly budget for this account · 4,019 of 5,000 left · resets in 23 min");
  });

  test("low-remaining: just under 25% takes warn, exactly 25% stays muted", () => {
    expect(rateLimitMeter(1_249, 5_000, null, 1_000).tone).toBe("warn");
    expect(rateLimitMeter(1_250, 5_000, null, 1_000).tone).toBe("muted");
  });

  test("low-remaining: just under 10% takes danger, exactly 10% stays warn", () => {
    expect(rateLimitMeter(499, 5_000, null, 1_000).tone).toBe("danger");
    expect(rateLimitMeter(500, 5_000, null, 1_000).tone).toBe("warn");
  });

  test("no x-ratelimit-limit header: the fill falls back, the sentence does not claim a limit", () => {
    // The fill needs a denominator, so the documented 5,000 stands in — but
    // the tooltip must not present that fallback as something GitHub said.
    const m = rateLimitMeter(2_500, null, null, 1_000);
    expect(m.fraction).toBeCloseTo(2_500 / DEFAULT_RATE_LIMIT);
    expect(m.title).toBe("GitHub's hourly budget for this account · 2,500 left");
    expect(m.title).not.toContain("of 5,000");
  });

  test("unknown reset: the tooltip drops the countdown clause entirely, not a blank one", () => {
    const m = rateLimitMeter(4_019, 5_000, null, 1_000);
    expect(m.title).toBe("GitHub's hourly budget for this account · 4,019 of 5,000 left");
    expect(m.title).not.toContain("resets");
  });

  test("fraction never exceeds 1 even if remaining somehow reports over the limit", () => {
    expect(rateLimitMeter(5_100, 5_000, null, 1_000).fraction).toBe(1);
  });
});
