// Svelte-store-based app state: events, repos, team, settings, unseen count.
// Views subscribe with `$store`; only api.ts talks to the backend.
//
// Note: this file is plain `.ts` (per the packet layout), so it uses classic
// Svelte stores (writable/derived) rather than `$state` runes — runes only
// compile inside `.svelte` / `.svelte.ts` files.
import { derived, get, writable, type Readable, type Writable } from "svelte/store";
import { api } from "./api";
import type { Account, Event, PollResult, Repo, RepoError, Settings, Team, Watch } from "./types";
import { ALL_EVENT_KINDS, isEngineError } from "./types";
import { COMMENT_KINDS, ISSUE_KINDS, PR_KINDS } from "./threads";
import {
  makeEntry as makeDetailEntry,
  popStack as popDetailStack,
  popToIndex as popDetailStackToIndex,
  pushStack as pushDetailStack,
  replaceStack as replaceDetailStack,
  sameTarget,
  setTopScrollTop as setDetailTopScrollTop,
  setTopTab as setDetailTopTab,
  stepTopTarget as stepDetailTopTarget,
  type DetailNavEntry,
  type DetailTab,
  type DetailTarget,
} from "./detail-nav";

// Re-exported so every existing importer of `DetailTarget`/`sameTarget`/
// `DetailTab` from "./stores" keeps working unchanged — the pure logic they
// name now lives in detail-nav.ts (see that file's own doc comment).
export type { DetailTarget, DetailTab, DetailNavEntry };
export { sameTarget };

const PAGE_SIZE = 50;

export interface EventFilters {
  repoId?: number | null;
  actor?: string | null;
  teamId?: number | null;
  /** The Feed/Pull requests/Comments "Watched" chip. */
  watchedOnly?: boolean;
}

function createEventStore() {
  const items: Writable<Event[]> = writable([]);
  const loading: Writable<boolean> = writable(false);
  const exhausted: Writable<boolean> = writable(false);
  let currentItems: Event[] = [];
  items.subscribe((v) => (currentItems = v));

  async function loadInitial(filters: EventFilters) {
    loading.set(true);
    try {
      const page = await api.listEvents({ ...filters, limit: PAGE_SIZE });
      items.set(page);
      exhausted.set(page.length < PAGE_SIZE);
    } finally {
      loading.set(false);
    }
  }

  async function loadMore(filters: EventFilters) {
    if (currentItems.length === 0) return;
    let isLoading = false;
    loading.update((v) => {
      isLoading = v;
      return v;
    });
    if (isLoading) return;
    loading.set(true);
    try {
      const beforeId = currentItems[currentItems.length - 1].id;
      const page = await api.listEvents({ ...filters, beforeId, limit: PAGE_SIZE });
      items.update((v) => [...v, ...page]);
      if (page.length < PAGE_SIZE) exhausted.set(true);
    } finally {
      loading.set(false);
    }
  }

  function prepend(newEvents: Event[]) {
    if (newEvents.length === 0) return;
    items.update((v) => [...newEvents, ...v]);
  }

  function markSeenLocally(ids: number[]) {
    const idSet = new Set(ids);
    items.update((v) => v.map((e) => (idSet.has(e.id) ? { ...e, seen: true } : e)));
  }

  return { items, loading, exhausted, loadInitial, loadMore, prepend, markSeenLocally };
}

function createReposStore() {
  const items: Writable<Repo[]> = writable([]);

  async function refresh() {
    items.set(await api.listRepos());
  }

  async function add(spec: string, accountLogin?: string | null) {
    const repo = await api.addRepo(spec, accountLogin);
    items.update((v) => [...v, repo]);
    return repo;
  }

  async function remove(repoId: number) {
    await api.removeRepo(repoId);
    items.update((v) => v.filter((r) => r.id !== repoId));
  }

  return { items, refresh, add, remove };
}

/** Signed-in accounts (docs/CONTRACT.md "Accounts"): the Settings Accounts
 * list and the Repos add form's account picker both read this one list. */
function createAccountsStore() {
  const items: Writable<Account[]> = writable([]);

  /** Optimistic overlay for the instant right after this app's own
   * `signOut`/`noteSignedIn` calls, before the next poll's repos catch up
   * with a fresh `last_error` (see `signedOutLogins` below) — mirrors what
   * Settings.svelte's Accounts section keeps locally as `justSignedOut`. */
  const optimisticSignedOut: Writable<Set<string>> = writable(new Set());

  /** Accounts with no token right now (docs/CONTRACT.md "Accounts"): their
   * repos poll-skip with `last_error: "signed out"` rather than being
   * dropped, since the engine never annotates `Account` itself with live
   * in-memory state. The same rule Settings.svelte's Accounts section
   * computes for its own rows, so the account switcher agrees with it. */
  const signedOutLogins: Readable<Set<string>> = derived(
    [reposStore.items, optimisticSignedOut],
    ([$repos, $optimistic]) =>
      new Set([...$repos.filter((r) => r.last_error === "signed out").map((r) => r.account_login), ...$optimistic]),
  );

  async function refresh() {
    items.set(await api.listAccounts());
  }

  /** Forgets the account (and its repos — the engine cascades that) and
   * drops it locally; the repo list is refreshed separately by the caller,
   * since this store has no view onto `reposStore`. */
  async function remove(login: string) {
    await api.removeAccount(login);
    items.update((v) => v.filter((a) => a.login !== login));
    optimisticSignedOut.update((v) => {
      if (!v.has(login)) return v;
      const next = new Set(v);
      next.delete(login);
      return next;
    });
  }

  /** "Sign out" (docs/CONTRACT.md "Accounts"): drops the keychain item and
   * the engine's in-memory token but keeps the account and its repos —
   * unlike `remove`, above. The caller (Settings or the account switcher)
   * still owns its own busy/error UI around this call. */
  async function signOut(login: string) {
    await api.signOutAccount(login);
    optimisticSignedOut.update((v) => new Set([...v, login]));
  }

  /** Clears the optimistic overlay once `login` has a token again — the
   * device flow already re-registered it by the time a caller's
   * `onSignedIn` fires, so there's nothing left to await here. */
  function noteSignedIn(login: string) {
    optimisticSignedOut.update((v) => {
      if (!v.has(login)) return v;
      const next = new Set(v);
      next.delete(login);
      return next;
    });
  }

  return { items, signedOutLogins, refresh, remove, signOut, noteSignedIn };
}

/** Every team, in display order (CONTRACT.md "Teams storage"): the Teams
 * view, the team switcher and People's grouping all read this one list. */
function createTeamsStore() {
  const items: Writable<Team[]> = writable([]);

  async function refresh() {
    const teams = await api.listTeams();
    items.set(teams);
    // A selection persisted in localStorage may name a team that no longer
    // exists (deleted from another window, DB reset): fall back to "All
    // teams" rather than filtering every view down to nothing.
    const selected = get(teamFilterStore.selected);
    if (selected != null && !teams.some((t) => t.id === selected)) teamFilterStore.select(null);
  }

  async function create(name: string, logins: string[]): Promise<Team> {
    const team = await api.createTeam(name, logins);
    items.update((v) => [...v, team]);
    return team;
  }

  async function update(team: Team) {
    await api.updateTeam(team);
    items.update((v) => v.map((t) => (t.id === team.id ? team : t)));
  }

  async function remove(teamId: number) {
    await api.deleteTeam(teamId);
    items.update((v) => v.filter((t) => t.id !== teamId));
    // Deleting the team the switcher is on would leave every Activity view
    // filtered by a dead id (the engine returns nothing for it).
    if (get(teamFilterStore.selected) === teamId) teamFilterStore.select(null);
  }

  /** Reorders locally first so the UI moves instantly, then persists; a
   * failure re-fetches the authoritative order rather than leaving the two
   * sides disagreeing. */
  async function reorder(ids: number[]) {
    const byId = new Map<number, Team>();
    items.update((v) => {
      for (const t of v) byId.set(t.id, t);
      return ids.map((id) => byId.get(id)!).filter(Boolean);
    });
    try {
      await api.reorderTeams(ids);
    } catch (e) {
      await refresh();
      throw e;
    }
  }

  return { items, refresh, create, update, remove, reorder };
}

const TEAM_FILTER_STORAGE_KEY = "vigie.teamFilter";

/** The team switcher's current selection (docs/CONTRACT.md work item 2):
 * one app-wide, localStorage-backed store shared by every Activity view and
 * the tray popover (a separate webview — a different JS runtime — so
 * localStorage, not an in-memory store, is what actually crosses that
 * boundary; the `storage` event picks up a change made in the *other*
 * window while this one is open). `null` means "All teams". */
function createTeamFilterStore() {
  function readStored(): number | null {
    try {
      const raw = localStorage.getItem(TEAM_FILTER_STORAGE_KEY);
      return raw ? Number(raw) : null;
    } catch {
      return null;
    }
  }

  const selected: Writable<number | null> = writable(readStored());

  function select(teamId: number | null) {
    selected.set(teamId);
    try {
      if (teamId == null) localStorage.removeItem(TEAM_FILTER_STORAGE_KEY);
      else localStorage.setItem(TEAM_FILTER_STORAGE_KEY, String(teamId));
    } catch {
      // Best-effort: the selection still works for the rest of this session.
    }
  }

  try {
    window.addEventListener("storage", (e) => {
      if (e.key === TEAM_FILTER_STORAGE_KEY || e.key === null) selected.set(readStored());
    });
  } catch {
    // No `window` (unit tests, SSR) — the store still works within one process.
  }

  return { selected, select };
}

function defaultSettings(): Settings {
  return {
    poll_interval_secs: 120,
    filter_mode: "team",
    notifications_enabled: true,
    notify_kinds: [...ALL_EVENT_KINDS],
    quiet_hours: null,
    auto_watch: true,
  };
}

function createSettingsStore() {
  const settings: Writable<Settings> = writable(defaultSettings());
  const loaded: Writable<boolean> = writable(false);

  async function refresh() {
    settings.set(await api.getSettings());
    loaded.set(true);
  }

  async function save(next: Settings) {
    await api.setSettings(next);
    settings.set(next);
  }

  return { settings, loaded, refresh, save };
}

function createUnseenStore() {
  const count: Writable<number> = writable(0);

  async function refresh() {
    count.set(await api.unseenCount());
  }

  function setCount(n: number) {
    count.set(n);
  }

  return { count, refresh, setCount };
}

/** What the rate-limit banner shows. `reset_at` is a real timestamp on both
 * paths — `RepoError.reset_at` for a limit the background poller hit, and
 * `EngineError.reset_at` for one a command surfaced — and is null only when
 * GitHub sent no reset header. */
export interface RateLimitNotice {
  message: string;
  reset_at: number | null;
}

function createPollStore() {
  const polling: Writable<boolean> = writable(false);
  const lastPolledAt: Writable<number | null> = writable(null);
  const rateLimitRemaining: Writable<number | null> = writable(null);
  /** Unix seconds `rateLimitRemaining`'s budget refills, and the budget's own
   * size — StatusBar.svelte's meter (`lib/rateLimitMeter.ts`) needs both
   * beside the count. Set only by a real poll's `PollResult`: a `backfill`'s
   * own reading updates `rateLimitRemaining` alone (see
   * `setRateLimitRemaining`), so these two stay whatever the last poll saw
   * rather than a stale story updated out of step with them. */
  const rateLimitResetAt: Writable<number | null> = writable(null);
  const rateLimitLimit: Writable<number | null> = writable(null);
  const lastError: Writable<string | null> = writable(null);
  /** Non-null while the banner is up; cleared by the next clean poll. */
  const rateLimit: Writable<RateLimitNotice | null> = writable(null);
  /** The most recent poll's `RepoError`s, keyed by `repo_id` — replaced
   * wholesale on every poll (this app's own, or a scheduled one relayed via
   * App.svelte's `new-events` listener), never merged, so a repo that comes
   * back clean drops out immediately rather than showing a stale failure.
   * StatusBar.svelte reads this to tell a repo's dot "rate limited" (message
   * starts with `rate_limited:`) apart from a plain error. */
  const lastPollErrors: Writable<Map<number, RepoError>> = writable(new Map());
  let isPolling = false;

  /** Shared by `pollNow` and App.svelte's `new-events` listener (a scheduled
   * background poll) so both paths keep `lastPolledAt` / `rateLimitRemaining`
   * / `lastPollErrors` / the rate-limit banner in sync the same way. */
  function applyPollResult(result: PollResult) {
    lastPolledAt.set(Date.now() / 1000);
    rateLimitRemaining.set(result.rate_limit_remaining);
    rateLimitResetAt.set(result.rate_limit_reset_at);
    rateLimitLimit.set(result.rate_limit_limit);
    lastPollErrors.set(new Map(result.errors.map((e) => [e.repo_id, e])));
    // The Rust poller emits `poll-rate-limited` for the banner; a poll
    // that comes back with no rate-limit failure takes it back down.
    if (!result.errors.some((e) => e.message.startsWith("rate_limited:"))) {
      rateLimit.set(null);
    }
  }

  async function pollNow(onResult?: (r: PollResult) => void) {
    if (isPolling) return;
    isPolling = true;
    polling.set(true);
    lastError.set(null);
    try {
      const result = await api.pollNow();
      applyPollResult(result);
      onResult?.(result);
      return result;
    } catch (e) {
      lastError.set(e instanceof Error ? e.message : String(e));
      if (isEngineError(e) && e.kind === "rate_limited") {
        rateLimit.set({ message: e.message, reset_at: e.reset_at });
      }
      throw e;
    } finally {
      isPolling = false;
      polling.set(false);
    }
  }

  function setRateLimit(notice: RateLimitNotice | null) {
    rateLimit.set(notice);
  }

  /** Updates just the "N requests left" reading (StatusBar/Feed header)
   * without touching `lastPolledAt` or the per-repo error map — for a
   * `backfill` call, which returns its own `rate_limit_remaining` but isn't
   * a poll and shouldn't be reported as one. */
  function setRateLimitRemaining(n: number | null) {
    rateLimitRemaining.set(n);
  }

  return {
    polling,
    lastPolledAt,
    rateLimitRemaining,
    rateLimitResetAt,
    rateLimitLimit,
    lastError,
    rateLimit,
    lastPollErrors,
    pollNow,
    applyPollResult,
    setRateLimit,
    setRateLimitRemaining,
  };
}

/** Per-nav-item unseen counts (Sidebar badges): computed client-side from a
 * page of events rather than a dedicated backend endpoint — the mock's
 * event volume is small enough that fetching up to `list_events`'s own
 * 500-row clamp and bucketing by kind-group is simpler than adding a new
 * contract command for it. */
export interface BadgeCounts {
  feed: number;
  prs: number;
  commits: number;
  comments: number;
  issues: number;
}

function createBadgeStore() {
  const counts: Writable<BadgeCounts> = writable({ feed: 0, prs: 0, commits: 0, comments: 0, issues: 0 });

  async function refresh() {
    const events: Event[] = await api.listEvents({ limit: 500 });
    const unseen = events.filter((e) => !e.seen);
    counts.set({
      feed: unseen.length,
      prs: unseen.filter((e) => PR_KINDS.includes(e.kind)).length,
      commits: unseen.filter((e) => e.kind === "commit").length,
      comments: unseen.filter((e) => COMMENT_KINDS.includes(e.kind)).length,
      issues: unseen.filter((e) => ISSUE_KINDS.includes(e.kind)).length,
    });
  }

  return { counts, refresh };
}

/** Watched threads (docs/CONTRACT.md "Watched threads"): the Watched view's
 * list, and what the detail pane's Watch/Watching toggle reads and writes.
 * One app-wide list — App.svelte refreshes it at startup alongside the other
 * stores, so the toggle knows a thread's watched state before it's opened. */
function createWatchesStore() {
  const items: Writable<Watch[]> = writable([]);

  async function refresh() {
    items.set(await api.listWatches());
  }

  async function watch(repoId: number, number: number): Promise<Watch> {
    const w = await api.watchThread(repoId, number);
    items.update((v) => [w, ...v.filter((x) => !(x.repo_id === repoId && x.number === number))]);
    return w;
  }

  async function unwatch(repoId: number, number: number) {
    await api.unwatchThread(repoId, number);
    items.update((v) => v.filter((w) => !(w.repo_id === repoId && w.number === number)));
  }

  /** Unwatches every closed watch and returns how many; re-fetches rather
   * than trusting the count locally, since "closed" covers both `closed` and
   * `merged`. */
  async function clearClosed(): Promise<number> {
    const removed = await api.clearClosedWatches();
    await refresh();
    return removed;
  }

  return { items, refresh, watch, unwatch, clearClosed };
}

/** The Feed/Pull requests/Comments "Watched" chip: one app-wide toggle (not
 * persisted — it's a quick filter, not a preference) that, when on, passes
 * `watched_only: true` to `list_events` alongside whatever team filter is
 * already selected. */
export const watchedOnlyStore: Writable<boolean> = writable(false);

/** Whether the user has run "Load older activity" (Feed.svelte) at least once
 * this session. The first time costs API requests, so reaching the end of
 * the feed only offers the control; every end-of-feed after that first
 * click may fire the same step automatically. Session-only, like
 * `collapsedThreadsStore` below — not persisted, so a fresh launch asks
 * again. */
export const hasLoadedOlderActivityStore: Writable<boolean> = writable(false);

const PANE_WIDTH_STORAGE_KEY = "vigie.paneWidth";
/** Default and clamp range for the split-pane divider, as a fraction of the
 * content area's width — the divider (App.svelte) never lets a drag or
 * arrow-key nudge push `paneWidth` outside this range; going past it is
 * instead a reader-mode entry (see App.svelte's pointermove handler) or a
 * no-op. */
export const PANE_WIDTH_DEFAULT = 0.45;
const PANE_WIDTH_MIN = 0.3;
const PANE_WIDTH_MAX = 0.7;

function clampPaneWidth(fraction: number): number {
  return Math.min(PANE_WIDTH_MAX, Math.max(PANE_WIDTH_MIN, fraction));
}

/** Options an `open*` caller can hand the pane along with the target — the
 * reader-mode breadcrumb's first crumb (`origin`), the ordered list
 * previous/next steps through (`siblings`, a snapshot of the origin view's
 * currently-rendered openable items at open-time, not a live binding — a
 * filter changing under an open pane doesn't reshuffle the list mid-step),
 * and (packet 007) whether this open pushes a level or replaces the whole
 * stack. All optional so every pre-packet-007 call site (an old two-arg
 * `openThread`/`openCommit`, or a plain `open(target)`) still compiles and
 * degrades sensibly: `origin` defaults to "Feed", `siblings` to just the
 * one target (so stepping is simply disabled both ways), `push` to false.
 *
 * `push`: opening from a list — a feed row, Commits/Issues/PullRequests
 * /Comments/Watched's own rows, the tray popover — REPLACES the pane's
 * whole navigation stack down to one entry, exactly as `open` always did
 * before this option existed. Opening from *inside* the pane instead —
 * today, only a commit in a PR's Commits tab (DetailPane.svelte's
 * `openCommitItem`) — PUSHES a level on top, so Back/Esc return to the
 * level underneath (its tab and scroll position restored) instead of
 * closing outright. Every call site says explicitly which it means; there
 * is no inferring one from the other. */
export interface DetailOpenOptions {
  origin?: string;
  siblings?: DetailTarget[];
  push?: boolean;
}

/** The right-hand detail pane's open/closed state — shared across every
 * view that can open a row (Feed, Pull requests, Commits, Comments,
 * Issues, and the tray popover's "Open" button), so App.svelte can host one
 * pane regardless of which view is active.
 *
 * `stack` (packet 007, "give detailStore a navigation stack") holds one
 * `DetailNavEntry` per level — see detail-nav.ts's own doc comment for why.
 * `target`/`siblings`/`activeTab` below are read-only views derived from
 * its top (current) entry, so every existing reader of
 * `detailStore.target`/`.siblings` keeps working unchanged. `origin` stays
 * its own single field: the breadcrumb's first crumb is the view the whole
 * pane session was opened from, and doesn't change as levels are pushed on
 * top of it. */
function createDetailStore() {
  const stack: Writable<DetailNavEntry[]> = writable<DetailNavEntry[]>([]);
  let lastTarget: DetailTarget | null = null;

  /** Deduped on purpose. `derived` re-emits on EVERY `stack` update, and the
   * stack is written on every scroll (`setScrollTop` remembers each level's
   * offset). DetailPane reloads whenever `target` fires, so an undeduped
   * store meant scrolling a long diff refetched it on every scroll event —
   * the pane flashed "Loading…" and never got anywhere. Only emit when the
   * thing being looked at actually changes. */
  const target: Readable<DetailTarget | null> = derived<typeof stack, DetailTarget | null>(
    stack,
    (s, set) => {
      const next = s[s.length - 1]?.target ?? null;
      if (next === lastTarget || (next !== null && sameTarget(next, lastTarget))) return;
      lastTarget = next;
      set(next);
    },
    null,
  );
  const siblings: Readable<DetailTarget[]> = derived(stack, (s) => s[s.length - 1]?.siblings ?? []);
  const activeTab: Readable<DetailTab> = derived(stack, (s) => s[s.length - 1]?.tab ?? "conversation");
  // Split vs. full-width "reader" layout (replaces the old `wide` boolean)
  // — an in-memory session preference, not persisted to disk, that survives
  // switching the pane to a different row but always starts in "split" on a
  // fresh app launch.
  const layout: Writable<"split" | "reader"> = writable("split");
  // The reader-mode breadcrumb's first crumb — see DetailOpenOptions above.
  const origin: Writable<string> = writable("Feed");

  function readStoredPaneWidth(): number {
    try {
      const raw = localStorage.getItem(PANE_WIDTH_STORAGE_KEY);
      const n = raw ? Number(raw) : NaN;
      return Number.isFinite(n) ? clampPaneWidth(n) : PANE_WIDTH_DEFAULT;
    } catch {
      return PANE_WIDTH_DEFAULT;
    }
  }

  // The split-layout pane's width, as a fraction of the content area —
  // unlike `layout`, this *is* persisted (App.svelte's divider drag, its
  // arrow-key nudge, and its double-click reset all go through
  // `setPaneWidth` below), so the width the user last dragged to survives a
  // relaunch.
  const paneWidth: Writable<number> = writable(readStoredPaneWidth());

  function open(t: DetailTarget, opts: DetailOpenOptions = {}) {
    const entry = makeDetailEntry(t, opts.siblings);
    if (opts.push && get(stack).length > 0) {
      stack.update((s) => pushDetailStack(s, entry));
      return;
    }
    origin.set(opts.origin ?? "Feed");
    stack.set(replaceDetailStack(entry));
  }
  function openThread(repoId: number, number: number, opts?: DetailOpenOptions) {
    open({ kind: "thread", repoId, number }, opts);
  }
  function openCommit(repoId: number, sha: string, opts?: DetailOpenOptions) {
    open({ kind: "commit", repoId, sha }, opts);
  }
  function close() {
    stack.set([]);
    // A single click always opens split; only a double-click (or ⌘Enter)
    // enters reader, so closing from reader must not leave it armed.
    layout.set("split");
  }
  function enterReader() {
    layout.set("reader");
  }
  function exitReader() {
    layout.set("split");
  }
  /** Whether Back/Esc should pop a level rather than close/exit reader —
   * true once something has been pushed on top of the level the pane was
   * originally opened to (packet 007, "only close/step out of reader mode
   * when [the stack] does not" have more than one entry). */
  function canPop(): boolean {
    return get(stack).length > 1;
  }
  /** Drops the top level, returning to the one underneath — its tab and
   * scroll position come back exactly as left, since neither was touched
   * while this level sat on top. A no-op at the root; callers gate on
   * `canPop()` first to fall back to close/exitReader there instead. */
  function popOne() {
    stack.update((s) => popDetailStack(s));
  }
  /** A breadcrumb click on an earlier level — pops straight back to it. */
  function popToLevel(index: number) {
    stack.update((s) => popDetailStackToIndex(s, index));
  }
  /** Records which tab the pane's current level is showing
   * (DetailPane.svelte's Conversation/Commits/Files tabstrip) — restored
   * automatically the next time this level comes back to the top of the
   * stack (a pop, or a breadcrumb click past it). */
  function setTab(tab: DetailTab) {
    stack.update((s) => setDetailTopTab(s, tab));
  }
  /** Records the current level's scroll offset, best-effort — same
   * restore-on-return contract as `setTab`. */
  function setScrollTop(scrollTop: number) {
    stack.update((s) => setDetailTopScrollTop(s, scrollTop));
  }
  /** The current level's own remembered scroll offset — read once, after
   * its content has (re)loaded, to restore it (DetailPane.svelte). */
  function getScrollTop(): number {
    const s = get(stack);
    return s[s.length - 1]?.scrollTop ?? 0;
  }
  /** Steps the top level to the previous (`-1`) / next (`1`) item in *its
   * own* siblings snapshot — "the stepper follows the level" (packet 007):
   * after drilling into a commit from a PR's Commits tab, this steps
   * through that PR's own commits, not whatever list the PR itself came
   * from. Reader mode's arrow buttons and App.svelte's j/k · ↑/↓ keys both
   * go through this. A no-op with nothing open, at either end of the
   * level's list, or if its target has somehow fallen out of its own
   * snapshot. */
  function step(delta: -1 | 1) {
    stack.update((s) => stepDetailTopTarget(s, delta));
  }
  function stepNext() {
    step(1);
  }
  function stepPrevious() {
    step(-1);
  }
  function setPaneWidth(fraction: number) {
    const clamped = clampPaneWidth(fraction);
    paneWidth.set(clamped);
    try {
      localStorage.setItem(PANE_WIDTH_STORAGE_KEY, String(clamped));
    } catch {
      // Best-effort: the width still works for the rest of this session.
    }
  }

  return {
    stack,
    target,
    siblings,
    activeTab,
    layout,
    paneWidth,
    origin,
    openThread,
    openCommit,
    open,
    close,
    enterReader,
    exitReader,
    canPop,
    popOne,
    popToLevel,
    setTab,
    setScrollTop,
    getScrollTop,
    setPaneWidth,
    stepNext,
    stepPrevious,
  };
}

/** Session-only collapsed/expanded state for thread blocks
 * (ThreadBlock.svelte, shared by Feed and the tray popover) — keyed by
 * `${repoId}#${number}` and held here rather than as component state, so a
 * thread stays collapsed across the Feed's `{#key}` remounts (switching
 * views and back) within a session. Not persisted to disk: every thread
 * starts expanded again on a fresh app launch. */
function createCollapsedThreadsStore() {
  const collapsed: Writable<Set<string>> = writable(new Set());

  function key(repoId: number, number: number): string {
    return `${repoId}#${number}`;
  }

  function toggle(repoId: number, number: number) {
    const k = key(repoId, number);
    collapsed.update((set) => {
      const next = new Set(set);
      if (next.has(k)) next.delete(k);
      else next.add(k);
      return next;
    });
  }

  return { collapsed, key, toggle };
}

export const eventStore = createEventStore();
export const reposStore = createReposStore();
export const accountsStore = createAccountsStore();
export const teamsStore = createTeamsStore();
export const teamFilterStore = createTeamFilterStore();
export const settingsStore = createSettingsStore();
export const unseenStore = createUnseenStore();
export const pollStore = createPollStore();
export const badgeStore = createBadgeStore();
export const detailStore = createDetailStore();
export const collapsedThreadsStore = createCollapsedThreadsStore();
export const watchesStore = createWatchesStore();
