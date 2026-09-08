import { describe, expect, test } from "vitest";
import { healthSummary } from "./status";
import type { Repo, RepoError } from "./types";

let nextId = 1;

function repo(overrides: Partial<Repo> = {}): Repo {
  const id = nextId++;
  return {
    id,
    owner: "acme",
    name: `repo-${id}`,
    url: `https://github.com/acme/repo-${id}`,
    account_login: "alice",
    default_branch: "main",
    last_polled_at: 1000,
    last_error: null,
    hidden: false,
    ...overrides,
  };
}

function err(repo_id: number, message: string, reset_at: number | null = null): RepoError {
  return { repo_id, message, reset_at };
}

const NO_ERRORS = new Map<number, RepoError>();
const NO_SIGNED_OUT = new Set<string>();

describe("healthSummary", () => {
  test("no repos watched at all", () => {
    const result = healthSummary([], NO_ERRORS, NO_SIGNED_OUT);
    expect(result.segments).toEqual([{ text: "No repos yet", tone: "muted" }]);
    expect(result.title).toBe("");
  });

  test("all healthy, plural", () => {
    const repos = Array.from({ length: 11 }, () => repo());
    const result = healthSummary(repos, NO_ERRORS, NO_SIGNED_OUT);
    expect(result.segments).toEqual([{ text: "11 repos · all healthy", tone: "muted" }]);
    expect(result.title).toBe("");
  });

  test("all healthy, singular", () => {
    const repos = [repo()];
    const result = healthSummary(repos, NO_ERRORS, NO_SIGNED_OUT);
    expect(result.segments).toEqual([{ text: "1 repo · healthy", tone: "muted" }]);
    expect(result.title).toBe("");
  });

  test("every repo failing (plural, single segment)", () => {
    const a = repo({ last_error: "500 Internal Server Error" });
    const b = repo({ last_error: "connection reset" });
    const result = healthSummary([a, b], NO_ERRORS, NO_SIGNED_OUT);
    expect(result.segments).toEqual([{ text: "2 repos failing", tone: "danger" }]);
    expect(result.title).toBe(`${a.owner}/${a.name} — 500 Internal Server Error\n${b.owner}/${b.name} — connection reset`);
  });

  test("every repo rate-limited (singular, single segment)", () => {
    const a = repo({ last_error: "rate_limited: budget exhausted" });
    const errors = new Map([[a.id, err(a.id, "rate_limited: budget exhausted", 1234)]]);
    const result = healthSummary([a], errors, NO_SIGNED_OUT);
    expect(result.segments).toEqual([{ text: "1 repo rate-limited", tone: "warn" }]);
    expect(result.title).toBe(`${a.owner}/${a.name} — rate limited: budget exhausted`);
  });

  test("every repo never polled (plural, single segment)", () => {
    const repos = [repo({ last_polled_at: null }), repo({ last_polled_at: null }), repo({ last_polled_at: null })];
    const result = healthSummary(repos, NO_ERRORS, NO_SIGNED_OUT);
    expect(result.segments).toEqual([{ text: "3 repos not polled yet", tone: "muted" }]);
    expect(result.title).toContain("never polled");
    expect(result.title.split("\n")).toHaveLength(3);
  });

  test("signed-out segment counts distinct accounts, not repos", () => {
    const a = repo({ account_login: "alice" });
    const b = repo({ account_login: "alice" });
    const result = healthSummary([a, b], NO_ERRORS, new Set(["alice"]));
    expect(result.segments).toEqual([{ text: "1 account signed out", tone: "muted" }]);
    expect(result.title.split("\n")).toHaveLength(2);
  });

  test("signed-out segment counts multiple distinct accounts", () => {
    const a = repo({ account_login: "alice" });
    const b = repo({ account_login: "other" });
    const result = healthSummary([a, b], NO_ERRORS, new Set(["alice", "other"]));
    expect(result.segments).toEqual([{ text: "2 accounts signed out", tone: "muted" }]);
  });

  test("mixed: healthy + one failing (two segments, most-severe first)", () => {
    const healthy = Array.from({ length: 9 }, () => repo());
    const failing = repo({ last_error: "boom" });
    const result = healthSummary([...healthy, failing], NO_ERRORS, NO_SIGNED_OUT);
    expect(result.segments).toEqual([
      { text: "1 failing", tone: "danger" },
      { text: "9 healthy", tone: "muted" },
    ]);
    expect(result.title).toBe(`${failing.owner}/${failing.name} — boom`);
  });

  test("mixed: healthy + rate-limited + failing (three segments, most-severe first)", () => {
    const healthy = Array.from({ length: 9 }, () => repo());
    const rateLimited = repo();
    const failing = repo({ last_error: "boom" });
    const errors = new Map([[rateLimited.id, err(rateLimited.id, "rate_limited: try later")]]);
    const result = healthSummary([...healthy, rateLimited, failing], errors, NO_SIGNED_OUT);
    expect(result.segments).toEqual([
      { text: "1 failing", tone: "danger" },
      { text: "1 rate-limited", tone: "warn" },
      { text: "9 healthy", tone: "muted" },
    ]);
  });

  test("more than three kinds present collapses to the two most severe plus +N more", () => {
    const failing = repo({ last_error: "boom" });
    const rateLimited = repo();
    const signedOut = repo({ account_login: "ghost" });
    const neverPolled = repo({ last_polled_at: null });
    const errors = new Map([[rateLimited.id, err(rateLimited.id, "rate_limited: try later")]]);
    const result = healthSummary(
      [failing, rateLimited, signedOut, neverPolled],
      errors,
      new Set(["ghost"]),
    );
    expect(result.segments).toEqual([
      { text: "1 failing", tone: "danger" },
      { text: "1 rate-limited", tone: "warn" },
      { text: "+2 more", tone: "muted" },
    ]);
  });

  test("healthy repos are never folded into +N more", () => {
    const failing = repo({ last_error: "boom" });
    const rateLimited = repo();
    const signedOut = repo({ account_login: "ghost" });
    const neverPolled = repo({ last_polled_at: null });
    const healthy = repo();
    const errors = new Map([[rateLimited.id, err(rateLimited.id, "rate_limited: try later")]]);
    const result = healthSummary(
      [failing, rateLimited, signedOut, neverPolled, healthy],
      errors,
      new Set(["ghost"]),
    );
    // Two troubled kinds are hidden (signed-out, never-polled); the healthy
    // repo must NOT be counted — "+N more" sits beside trouble segments and
    // is read as N further problems.
    expect(result.segments).toEqual([
      { text: "1 failing", tone: "danger" },
      { text: "1 rate-limited", tone: "warn" },
      { text: "+2 more", tone: "muted" },
    ]);
  });

  test("+N more counts only troubled repos even when healthy repos dominate", () => {
    const failing = repo({ last_error: "boom" });
    const rateLimited = repo();
    const signedOut = repo({ account_login: "ghost" });
    const neverPolled = repo({ last_polled_at: null });
    const errors = new Map([[rateLimited.id, err(rateLimited.id, "rate_limited: try later")]]);
    const healthy = Array.from({ length: 20 }, () => repo());
    const result = healthSummary(
      [failing, rateLimited, signedOut, neverPolled, ...healthy],
      errors,
      new Set(["ghost"]),
    );
    expect(result.segments[2]).toEqual({ text: "+2 more", tone: "muted" });
  });

  test("title truncates at 10 repos with a trailing ellipsis line", () => {
    const repos = Array.from({ length: 11 }, (_, i) => repo({ last_error: `err-${i}` }));
    const result = healthSummary(repos, NO_ERRORS, NO_SIGNED_OUT);
    const lines = result.title.split("\n");
    expect(lines).toHaveLength(11);
    expect(lines[10]).toBe("…");
  });

  test("title does not truncate at exactly 10 repos", () => {
    const repos = Array.from({ length: 10 }, (_, i) => repo({ last_error: `err-${i}` }));
    const result = healthSummary(repos, NO_ERRORS, NO_SIGNED_OUT);
    const lines = result.title.split("\n");
    expect(lines).toHaveLength(10);
    expect(lines[9]).not.toBe("…");
  });

  test("signed-out overrides a stale last_error on the same repo", () => {
    const a = repo({ account_login: "alice", last_error: "stale error from before sign-out" });
    const result = healthSummary([a], NO_ERRORS, new Set(["alice"]));
    expect(result.segments).toEqual([{ text: "1 account signed out", tone: "muted" }]);
  });

  test("never-polled overrides a stale last_error left from a first failed attempt", () => {
    const a = repo({ last_polled_at: null, last_error: "first attempt failed" });
    const result = healthSummary([a], NO_ERRORS, NO_SIGNED_OUT);
    expect(result.segments).toEqual([{ text: "1 repo not polled yet", tone: "muted" }]);
  });

  test("a rate-limited repo is not double-counted as failing (last_error is set on every failed poll)", () => {
    const a = repo({ last_error: "rate_limited: budget exhausted" });
    const errors = new Map([[a.id, err(a.id, "rate_limited: budget exhausted")]]);
    const result = healthSummary([a], errors, NO_SIGNED_OUT);
    expect(result.segments).toEqual([{ text: "1 repo rate-limited", tone: "warn" }]);
  });
});
