// Pure text-building logic behind StatusBar.svelte's health line (packet
// gm-statusbar-r2, replacing the r1 per-repo dot row). Kept out of the
// component so the severity rules and wording are unit-testable on their
// own.

import type { Repo, RepoError } from "./types";

export type HealthTone = "muted" | "danger" | "warn";

export interface HealthSegment {
  text: string;
  tone: HealthTone;
}

export interface HealthSummary {
  /** Rendered left to right, joined with " · " by the caller. */
  segments: HealthSegment[];
  /** Tooltip: one "owner/name — reason" line per troubled repo, empty when
   * every watched repo is healthy (or there are none to watch). */
  title: string;
}

const RATE_LIMITED_PREFIX = "rate_limited: ";

/** Segments beyond this collapse the least-severe kinds into one "+N more". */
const MAX_SEGMENTS = 3;
/** Repos named in the tooltip before it truncates with a trailing "…" line. */
const MAX_TITLE_LINES = 10;

type Kind = "failing" | "rate-limited" | "signed-out" | "never-polled" | "healthy";

/** Most severe first. Drives both which kinds survive the MAX_SEGMENTS cap
 * and the left-to-right order of the segments and tooltip lines. */
const SEVERITY: Kind[] = ["failing", "rate-limited", "signed-out", "never-polled", "healthy"];

interface Classified {
  repo: Repo;
  kind: Kind;
  /** Human reason for the tooltip line, e.g. the raw last_error text. */
  reason: string;
}

/** Strips the engine's `"<kind>: "` `Display` prefix back off for the
 * human-readable detail (mirrors poller.rs's `rate_limit_notice`). */
function withoutRateLimitPrefix(message: string): string {
  return message.startsWith(RATE_LIMITED_PREFIX) ? message.slice(RATE_LIMITED_PREFIX.length) : message;
}

/**
 * One repo's health bucket. Checked most-specific-first, which is NOT the
 * same order as `SEVERITY` above:
 *  - signed-out and never-polled are checked before any error state, since
 *    while an account is signed out (or a repo has never completed a first
 *    poll) any `last_error`/rate-limit left on it is stale and not the
 *    current story.
 *  - rate-limited is checked before the generic `last_error` check: the
 *    engine's `mark_repo_error` sets `last_error` on every failed poll,
 *    rate limits included, so a rate-limited repo always has `last_error`
 *    set too. Checking `last_error` first would make the `rate-limited`
 *    bucket unreachable.
 */
function classify(repo: Repo, pollErrors: Map<number, RepoError>, signedOutLogins: Set<string>): Classified {
  if (signedOutLogins.has(repo.account_login)) {
    return { repo, kind: "signed-out", reason: "signed out" };
  }
  if (repo.last_polled_at == null) {
    return { repo, kind: "never-polled", reason: "never polled" };
  }
  const err = pollErrors.get(repo.id);
  if (err != null && err.message.startsWith("rate_limited:")) {
    return { repo, kind: "rate-limited", reason: `rate limited: ${withoutRateLimitPrefix(err.message)}` };
  }
  if (repo.last_error != null) {
    return { repo, kind: "failing", reason: repo.last_error };
  }
  return { repo, kind: "healthy", reason: "healthy" };
}

function plural(n: number, singular: string, pluralForm: string): string {
  return n === 1 ? singular : pluralForm;
}

/** Wording when one kind accounts for every watched repo. */
function fullLabel(kind: Kind, count: number): string {
  if (kind === "failing") return `${count} ${plural(count, "repo", "repos")} failing`;
  if (kind === "rate-limited") return `${count} ${plural(count, "repo", "repos")} rate-limited`;
  if (kind === "never-polled") return `${count} ${plural(count, "repo", "repos")} not polled yet`;
  if (kind === "signed-out") return `${count} ${plural(count, "account", "accounts")} signed out`;
  return count === 1 ? "1 repo · healthy" : `${count} repos · all healthy`;
}

/** Wording inside a multi-segment line — drops the "repo(s)"/"account(s)"
 * noun since the neighbouring segments already make the subject clear. */
function compactLabel(kind: Kind, count: number): string {
  if (kind === "failing") return `${count} failing`;
  if (kind === "rate-limited") return `${count} rate-limited`;
  if (kind === "never-polled") return `${count} not polled yet`;
  if (kind === "signed-out") return `${count} signed out`;
  return `${count} healthy`;
}

function tone(kind: Kind): HealthTone {
  if (kind === "failing") return "danger";
  if (kind === "rate-limited") return "warn";
  return "muted";
}

/** The count a segment reports: repos, except `signed-out`, which reports
 * distinct affected accounts ("1 account signed out" rather than a repo
 * tally that could double-count one signed-out account's several repos). */
function segmentCount(kind: Kind, repos: Classified[]): number {
  if (kind === "signed-out") return new Set(repos.map((c) => c.repo.account_login)).size;
  return repos.length;
}

export function healthSummary(
  repos: Repo[],
  pollErrors: Map<number, RepoError>,
  signedOutLogins: Set<string>,
): HealthSummary {
  if (repos.length === 0) {
    return { segments: [{ text: "No repos yet", tone: "muted" }], title: "" };
  }

  const classified = repos.map((repo) => classify(repo, pollErrors, signedOutLogins));
  const byKind = new Map<Kind, Classified[]>();
  for (const c of classified) {
    const list = byKind.get(c.kind);
    if (list) list.push(c);
    else byKind.set(c.kind, [c]);
  }

  const healthyCount = byKind.get("healthy")?.length ?? 0;
  if (healthyCount === repos.length) {
    return { segments: [{ text: fullLabel("healthy", healthyCount), tone: "muted" }], title: "" };
  }

  const presentKinds = SEVERITY.filter((k) => (byKind.get(k)?.length ?? 0) > 0);

  let segments: HealthSegment[];
  if (presentKinds.length === 1) {
    // The single present kind covers every watched repo.
    const kind = presentKinds[0];
    segments = [{ text: fullLabel(kind, segmentCount(kind, byKind.get(kind) ?? [])), tone: tone(kind) }];
  } else if (presentKinds.length <= MAX_SEGMENTS) {
    segments = presentKinds.map((k) => ({
      text: compactLabel(k, segmentCount(k, byKind.get(k) ?? [])),
      tone: tone(k),
    }));
  } else {
    const troubleKinds: Kind[] = presentKinds.filter((k) => k !== "healthy");
    const shown = troubleKinds.slice(0, 2);
    const shownSet = new Set<Kind>(shown);
    // Only troubled repos are counted: "+22 more" beside two trouble segments
    // reads as 22 further problems, so healthy repos must not inflate it.
    const restRepoCount = troubleKinds
      .filter((k) => !shownSet.has(k))
      .reduce((sum, k) => sum + (byKind.get(k)?.length ?? 0), 0);
    segments = [
      ...shown.map((k) => ({ text: compactLabel(k, segmentCount(k, byKind.get(k) ?? [])), tone: tone(k) })),
      { text: `+${restRepoCount} more`, tone: "muted" as HealthTone },
    ];
  }

  const troubled = SEVERITY.filter((k) => k !== "healthy").flatMap((k) => byKind.get(k) ?? []);
  const lines = troubled.slice(0, MAX_TITLE_LINES).map((c) => `${c.repo.owner}/${c.repo.name} — ${c.reason}`);
  if (troubled.length > MAX_TITLE_LINES) lines.push("…");

  return { segments, title: lines.join("\n") };
}
