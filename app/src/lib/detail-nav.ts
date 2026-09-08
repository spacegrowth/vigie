// Pure logic behind the detail pane's navigation stack — no Svelte, no
// stores, so it's directly unit-testable (detail-nav.test.ts). stores.ts
// wraps this in a `Writable<DetailNavEntry[]>`; DetailPane.svelte reads the
// breadcrumb/stepper helpers here so the two agree on what "the current
// level" means.
//
// The bug this exists to fix: opening a commit from inside a PR's Commits
// tab used to *replace* the pane's one-and-only target — the PR (and its
// Conversation/Commits/Files strip) was simply gone, with no way back. Now
// the pane holds a stack of levels: a list (Feed, Commits, a PR row, …)
// REPLACES the stack down to one entry, same as before; opening something
// from *inside* the pane (a commit in a PR's Commits tab) PUSHES a new level
// on top instead, so Back/Esc pop back to the level underneath — restoring
// its tab and scroll position — rather than closing outright.

/** What the pane is looking at. Repeated from stores.ts's own (identical)
 * export rather than the other way around — stores.ts re-exports this one
 * so every existing importer of `DetailTarget`/`sameTarget` from "./stores"
 * keeps working unchanged. */
export type DetailTarget =
  | { kind: "thread"; repoId: number; number: number }
  | { kind: "commit"; repoId: number; sha: string };

/** Structural equality for two detail targets. */
export function sameTarget(a: DetailTarget | null, b: DetailTarget | null): boolean {
  if (a == null || b == null) return false;
  if (a.kind === "thread" && b.kind === "thread") return a.repoId === b.repoId && a.number === b.number;
  if (a.kind === "commit" && b.kind === "commit") return a.repoId === b.repoId && a.sha === b.sha;
  return false;
}

/** The pane's PR-only tab strip (DetailPane.svelte) — tracked per stack
 * level so popping back to a PR restores whichever tab it was left on,
 * rather than always landing on Conversation. Meaningless for a commit or
 * issue level, which render one fixed body regardless of this field. */
export type DetailTab = "conversation" | "commits" | "files";

/** One level of the pane's navigation stack: what's open, the list
 * previous/next steps through, which tab is active, and (best-effort) the
 * scroll position to restore on the way back. */
export interface DetailNavEntry {
  target: DetailTarget;
  /** The origin view's snapshot of its own currently-rendered list, taken
   * at open time — not a live binding. Deduped: several rows (e.g. review
   * comments) can resolve to the same target, and `stepTarget` finds the
   * current item by identity, so a repeated entry would make "next" loop
   * back to just after its first copy. */
  siblings: DetailTarget[];
  tab: DetailTab;
  scrollTop: number;
}

/** Builds a fresh entry — always starts on Conversation, scrolled to the
 * top; `target` itself is folded into the deduped siblings list when the
 * caller supplies none, so stepping is simply disabled both ways (the
 * pre-packet-002 default). */
export function makeEntry(target: DetailTarget, siblings?: DetailTarget[]): DetailNavEntry {
  return { target, siblings: dedupeSiblings(target, siblings), tab: "conversation", scrollTop: 0 };
}

function dedupeSiblings(target: DetailTarget, siblings: DetailTarget[] | undefined): DetailTarget[] {
  const unique: DetailTarget[] = [];
  for (const s of siblings ?? [target]) {
    if (!unique.some((u) => sameTarget(u, s))) unique.push(s);
  }
  return unique;
}

/** Opening from a list (a feed row, Commits view, a PR row, the tray
 * popover, …) — resets the stack to just this one entry. */
export function replaceStack(entry: DetailNavEntry): DetailNavEntry[] {
  return [entry];
}

/** Opening from *inside* the pane (a commit in a PR's Commits tab) — adds a
 * level on top instead of discarding what's underneath. */
export function pushStack(stack: DetailNavEntry[], entry: DetailNavEntry): DetailNavEntry[] {
  return [...stack, entry];
}

/** Drops the top level and returns to the one underneath. A no-op at the
 * root — the caller (stores.ts's `popOne`, App.svelte's Esc handler) is
 * expected to close the pane or step out of reader mode instead once this
 * stops changing anything, per the packet: pop only when there's more than
 * one entry. */
export function popStack(stack: DetailNavEntry[]): DetailNavEntry[] {
  return stack.length > 1 ? stack.slice(0, -1) : stack;
}

/** Drops back to exactly `index` — a breadcrumb click on an earlier level.
 * A no-op for an out-of-range index or the already-current top level, so a
 * stale click (the stack shrank from under it) can't push levels back on. */
export function popToIndex(stack: DetailNavEntry[], index: number): DetailNavEntry[] {
  if (index < 0 || index >= stack.length - 1) return stack;
  return stack.slice(0, index + 1);
}

function updateTop(stack: DetailNavEntry[], fn: (top: DetailNavEntry) => DetailNavEntry): DetailNavEntry[] {
  if (stack.length === 0) return stack;
  const top = stack[stack.length - 1];
  const next = fn(top);
  if (next === top) return stack;
  return [...stack.slice(0, -1), next];
}

/** Records which tab the current (top) level is showing — restored the next
 * time this level comes back to the top (a pop, or a breadcrumb click). */
export function setTopTab(stack: DetailNavEntry[], tab: DetailTab): DetailNavEntry[] {
  return updateTop(stack, (top) => (top.tab === tab ? top : { ...top, tab }));
}

/** Records the current (top) level's scroll offset, best-effort — same
 * restore-on-return contract as `setTopTab`. */
export function setTopScrollTop(stack: DetailNavEntry[], scrollTop: number): DetailNavEntry[] {
  return updateTop(stack, (top) => (top.scrollTop === scrollTop ? top : { ...top, scrollTop }));
}

/** Where `entry.target` sits in its own `siblings` snapshot — `-1` when the
 * frozen snapshot never contained this exact target (e.g. a level with no
 * siblings recorded at all). */
export function siblingIndex(entry: DetailNavEntry): number {
  return entry.siblings.findIndex((t) => sameTarget(t, entry.target));
}

/** The previous (`-1`) / next (`1`) item in `entry`'s own siblings list, or
 * `null` at either end (or if the target has fallen out of its own
 * snapshot) — a no-op for the caller to leave the entry untouched. */
export function stepTarget(entry: DetailNavEntry, delta: -1 | 1): DetailTarget | null {
  const i = siblingIndex(entry);
  if (i === -1) return null;
  return entry.siblings[i + delta] ?? null;
}

/** Steps the top level to the next/previous item in *its own* siblings —
 * "the stepper follows the level": after drilling into a commit from a PR's
 * Commits tab, this steps through that PR's commits, not the PR's own
 * siblings underneath. Stepping lands on a new item, so — like a fresh
 * `open` — it starts back on Conversation, scrolled to the top; the
 * siblings list itself is left alone, since stepping moves within the list
 * it started from rather than starting a new one. A no-op if there's
 * nothing to step to. */
export function stepTopTarget(stack: DetailNavEntry[], delta: -1 | 1): DetailNavEntry[] {
  return updateTop(stack, (top) => {
    const next = stepTarget(top, delta);
    if (!next) return top;
    return { target: next, siblings: top.siblings, tab: "conversation", scrollTop: 0 };
  });
}

/** One level in the breadcrumb chain (excluding the leading origin crumb,
 * which stays a separate, single string per stores.ts's `origin`). */
export interface BreadcrumbLevel {
  /** Index into the full stack — what a click on this level pops to. */
  index: number;
  target: DetailTarget;
  /** True only for the last (deepest, currently open) level — the pane
   * renders that one as plain text, everything else as a clickable link. */
  current: boolean;
}

/** The breadcrumb's post-origin chain, kept to the last `maxVisible` stack
 * entries (default 3) so a long drill-down doesn't run the header out of
 * room — `truncated` tells the caller to render a leading "…" for the
 * levels dropped off the front. */
export function breadcrumbLevels(stack: DetailNavEntry[], maxVisible = 3): { levels: BreadcrumbLevel[]; truncated: boolean } {
  if (stack.length === 0) return { levels: [], truncated: false };
  const start = Math.max(0, stack.length - maxVisible);
  const levels = stack.slice(start).map((entry, i) => ({
    index: start + i,
    target: entry.target,
    current: start + i === stack.length - 1,
  }));
  return { levels, truncated: start > 0 };
}
