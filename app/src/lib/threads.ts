// Shared "group flat events into PR/issue threads" logic, used by Feed,
// Pull requests, Comments and Issues (docs/CONTRACT.md's events are one flat
// store; the mockups group them by kind and, in Feed, by thread).
import type { Event, EventKind } from "./types";
import type { DetailTarget } from "./stores";

export const PR_KINDS: EventKind[] = ["pr_opened", "pr_merged", "pr_closed", "pr_reviewed", "pr_commented"];
export const COMMENT_KINDS: EventKind[] = ["pr_reviewed", "pr_commented", "issue_commented"];
export const ISSUE_KINDS: EventKind[] = ["issue_opened", "issue_commented"];

// SHORTCUT: PullRequests/Commits/Comments/Issues each fetch `list_events`
// with the 500-row clamp and filter client-side by kind-group, rather than
// a shared loader — `list_events` has no kind filter to push this
// server-side, and each view's render shape differs enough (grouped-by-PR,
// grouped-by-day-then-repo, flat cards) that a generic hook would mostly
// just move the duplication rather than remove it. Fine at the mock's
// event volume (a few hundred rows); if the real engine's event count
// grows past what a single 500-row page covers, this needs either a
// server-side kind filter added to the contract's `list_events`, or a
// shared `loadKindEvents(kinds)` helper (blocked today on Svelte 5 runes
// not compiling in a plain `.ts` file — would need a `.svelte.ts` module).

export interface EventGroup {
  repoId: number;
  number: number;
  events: Event[]; // chronological (oldest first) — a conversation reads top to bottom
  sortKey: number; // most recent member's occurred_at, for placement in a feed
  title: string;
}

/** Derives a thread's display title the same way the mock engine's
 * `thread_title` does: prefer the opening event's title (it *is* the PR/
 * issue title in this contract), else strip a "Comment on #N " prefix, else
 * fall back to the earliest event's raw title. */
export function threadTitle(events: Event[]): string {
  const opener = events.find((e) => e.kind === "pr_opened" || e.kind === "issue_opened");
  if (opener) return opener.title;
  const earliest = events[0];
  const m = earliest.title.match(/^Comment on #\d+ (.+)$/);
  if (m) return m[1];
  return earliest.title;
}

/** Groups events sharing a repo+number into threads (2+ members) or leaves
 * them as standalone rows (commits always; a PR/issue with exactly one
 * event seen so far, per the packet's "PRs with no children in view render
 * as single lines"). Returns groups sorted newest-activity-first. */
export function groupThreads(events: Event[]): { threads: EventGroup[]; standalone: Event[] } {
  const byKey = new Map<string, Event[]>();
  const standalone: Event[] = [];
  for (const e of events) {
    if (e.number == null) {
      standalone.push(e);
      continue;
    }
    const key = `${e.repo_id}:${e.number}`;
    if (!byKey.has(key)) byKey.set(key, []);
    byKey.get(key)!.push(e);
  }

  const threads: EventGroup[] = [];
  for (const [key, members] of byKey) {
    if (members.length < 2) {
      standalone.push(...members);
      continue;
    }
    const sorted = [...members].sort((a, b) => a.occurred_at - b.occurred_at);
    const [repoId, number] = key.split(":").map(Number);
    threads.push({
      repoId,
      number,
      events: sorted,
      sortKey: Math.max(...members.map((e) => e.occurred_at)),
      title: threadTitle(sorted),
    });
  }
  threads.sort((a, b) => b.sortKey - a.sortKey);
  return { threads, standalone };
}

/** The short verb phrase a thread's PR/issue-opened child row shows in
 * place of its (redundant-with-the-header) title; other kinds show nothing
 * extra here since their quoted `body_preview` carries the content. */
export function childActionText(kind: EventKind): string | null {
  switch (kind) {
    case "pr_opened":
      return "opened this pull request";
    case "pr_merged":
      return "merged this pull request";
    case "pr_closed":
      return "closed this pull request";
    case "issue_opened":
      return "opened this issue";
    default:
      return null;
  }
}

export interface PrThreadSummary {
  repoId: number;
  number: number;
  title: string;
  state: "open" | "changes_requested" | "merged" | "closed";
  openedBy: string;
  openedAt: number;
  lastActivityAt: number;
  approvals: number;
  comments: number;
  changesRequestedBy: string | null;
}

/** One row per PR number (docs/design/PRs.dc.html) — every `pr_*` event
 * sharing a repo+number becomes one summarized thread, unlike Feed's
 * threading which only groups 2+ events (a lone `pr_opened` is still its
 * own Pull requests row; Feed shows that one as a standalone line). */
export function summarizePrThreads(events: Event[]): PrThreadSummary[] {
  const byKey = new Map<string, Event[]>();
  for (const e of events) {
    if (!PR_KINDS.includes(e.kind) || e.number == null) continue;
    const key = `${e.repo_id}:${e.number}`;
    if (!byKey.has(key)) byKey.set(key, []);
    byKey.get(key)!.push(e);
  }

  const out: PrThreadSummary[] = [];
  for (const [key, members] of byKey) {
    const sorted = [...members].sort((a, b) => a.occurred_at - b.occurred_at);
    const [repoId, number] = key.split(":").map(Number);
    const opener = sorted.find((e) => e.kind === "pr_opened") ?? sorted[0];
    const reviews = sorted.filter((e) => e.kind === "pr_reviewed");
    const lastChangesRequested = [...reviews].reverse().find((e) => e.title.includes("changes requested"));

    let state: PrThreadSummary["state"] = "open";
    if (sorted.some((e) => e.kind === "pr_merged")) state = "merged";
    else if (sorted.some((e) => e.kind === "pr_closed")) state = "closed";
    else if (lastChangesRequested) state = "changes_requested";

    out.push({
      repoId,
      number,
      title: threadTitle(sorted),
      state,
      openedBy: opener.actor_login,
      openedAt: opener.occurred_at,
      lastActivityAt: Math.max(...sorted.map((e) => e.occurred_at)),
      approvals: reviews.filter((e) => e.title.includes("approved")).length,
      comments: sorted.filter((e) => e.kind === "pr_commented").length,
      changesRequestedBy: state === "changes_requested" ? (lastChangesRequested?.actor_login ?? null) : null,
    });
  }
  return out;
}

/** Where a row's click should open the detail pane: a commit event's sha is
 * the last URL segment (build_event_url on the Rust side always puts it
 * there); anything else with a number opens its thread. Bare events with
 * neither (shouldn't happen in practice) open nothing. */
export function eventDetailTarget(event: Event): DetailTarget | null {
  if (event.kind === "commit") {
    const sha = event.url.split("/").pop();
    return sha ? { kind: "commit", repoId: event.repo_id, sha } : null;
  }
  return event.number != null ? { kind: "thread", repoId: event.repo_id, number: event.number } : null;
}

/** "on #412 Add retry budget" for a Comments/Issues row — strips the
 * "Comment on " prefix seed/fabricated titles use for bare comment events,
 * falling back to a bare "on #N" when the fabricated title has no short
 * title embedded (`poll_now`'s generic "Comment on the open pull request"). */
export function commentRefLabel(event: Event): string {
  const stripped = event.title.replace(/^Comment on /, "");
  if (/^#\d+/.test(stripped)) return `on ${stripped}`;
  return event.number != null ? `on #${event.number}` : stripped;
}
