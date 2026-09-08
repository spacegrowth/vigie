// App-wide state for the "My team / Everyone" view filter and the repo
// selector (packet gm-feed-r1, work items 2-3; promoted from feed-only to
// app-wide by packet gm-scope-r1), plus Feed's own events-loading store,
// which needs both passed through to `list_events`.
//
// Despite the filename (unchanged — see below), `repoFilterStore` and
// `feedModeStore` were never actually feed-scoped: both are module-level
// singletons, exactly like `stores.ts`'s `teamFilterStore`, so every
// importer already shares one selection. gm-feed-r1 just wired that one
// selection into Feed.svelte alone; gm-scope-r1 is the packet that wires it
// into every other view too (Commits, Issues, Comments, Pull requests,
// Watched, People and the tray popover — see those files) via the same two
// stores exported below, so there is nothing to "promote": importing them
// elsewhere already puts every view on the same selection. `Summary` is the
// deliberate exception — its `digest`/`open_pulls` calls have no repo or
// mode parameter to pass either store's value to, so its control is left out
// rather than shown lying about what it does (see Summary.svelte's own note).
//
// NOT renamed, despite the misleading filename: `Feed.svelte` (out of this
// packet's boundary, do-not-touch) imports this module by this exact path,
// and a rename would require editing that import to keep compiling. The
// stores' own names (`repoFilterStore`, `feedModeStore`) already read fine
// from any caller — only the *file*'s name still says "feed".
//
// This is a NEW file rather than an addition to `stores.ts`: that file is
// contested by another executor mid-rewrite, and `stores.ts`'s own
// `eventStore` / `EventFilters` have no `mode` field to extend without
// editing it. `eventStore` is used only by Feed.svelte, so `feedEventStore`
// below simply replaces it there — same pagination shape, one more filter.
import { get, writable, type Writable } from "svelte/store";
import { api } from "./api";
import type { Event, FilterMode } from "./types";

const PAGE_SIZE = 50;

export interface FeedEventFilters {
  repoId?: number | null;
  actor?: string | null;
  teamId?: number | null;
  /** The "My team / Everyone" control (docs/CONTRACT.md "Filter"). */
  mode: FilterMode;
  watchedOnly?: boolean;
}

function createFeedEventStore() {
  const items: Writable<Event[]> = writable([]);
  const loading: Writable<boolean> = writable(false);
  const exhausted: Writable<boolean> = writable(false);
  let currentItems: Event[] = [];
  items.subscribe((v) => (currentItems = v));

  async function loadInitial(filters: FeedEventFilters) {
    loading.set(true);
    try {
      const page = await api.listEvents({ ...filters, limit: PAGE_SIZE });
      items.set(page);
      exhausted.set(page.length < PAGE_SIZE);
    } finally {
      loading.set(false);
    }
  }

  async function loadMore(filters: FeedEventFilters) {
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

  function markSeenLocally(ids: number[]) {
    const idSet = new Set(ids);
    items.update((v) => v.map((e) => (idSet.has(e.id) ? { ...e, seen: true } : e)));
  }

  return { items, loading, exhausted, loadInitial, loadMore, markSeenLocally };
}

export const feedEventStore = createFeedEventStore();

const REPO_FILTER_STORAGE_KEY = "vigie.feedRepoFilter";

/** The app-wide repo selector (gm-feed-r1 work item 3; wired into every view
 * by gm-scope-r1): `null` means "All repos", exactly like `teamFilterStore`'s
 * "All teams". localStorage-backed the same way, for the same reason — the
 * tray popover is a separate webview / JS runtime.
 * Exported (unlike `stores.ts`'s equivalent factories) so `feed-filters.test.ts`
 * can exercise a fresh instance per test rather than the one shared singleton. */
export function createRepoFilterStore() {
  function readStored(): number | null {
    try {
      const raw = localStorage.getItem(REPO_FILTER_STORAGE_KEY);
      return raw ? Number(raw) : null;
    } catch {
      return null;
    }
  }

  const selected: Writable<number | null> = writable(readStored());

  function select(repoId: number | null) {
    selected.set(repoId);
    try {
      if (repoId == null) localStorage.removeItem(REPO_FILTER_STORAGE_KEY);
      else localStorage.setItem(REPO_FILTER_STORAGE_KEY, String(repoId));
    } catch {
      // Best-effort: the selection still works for the rest of this session.
    }
  }

  try {
    window.addEventListener("storage", (e) => {
      if (e.key === REPO_FILTER_STORAGE_KEY || e.key === null) selected.set(readStored());
    });
  } catch {
    // No `window` (unit tests, SSR) — the store still works within one process.
  }

  return { selected, select };
}

export const repoFilterStore = createRepoFilterStore();

const FEED_MODE_STORAGE_KEY = "vigie.feedFilterMode";

/** The app-wide "My team / Everyone" control (gm-feed-r1 work item 2; wired
 * into every view by gm-scope-r1 — Summary excepted, see this file's header
 * comment).
 *
 * `null` means "no explicit choice yet in this browser" — `seedFromSettings`
 * fills that in from `Settings.filter_mode` the first time it is called
 * (App.svelte's settings load), which is exactly the packet's "an existing
 * install keeps behaving as its owner expects on first launch after the
 * update" requirement: `filter_mode` used to gate ingestion and is now this
 * control's starting value instead. Once a login has chosen explicitly (here
 * or restored from localStorage), the seed never overwrites it. Exported for
 * `feed-filters.test.ts`, same reason as `createRepoFilterStore` above. */
export function createFeedModeStore() {
  function readStored(): FilterMode | null {
    try {
      const raw = localStorage.getItem(FEED_MODE_STORAGE_KEY);
      return raw === "all" || raw === "team" ? raw : null;
    } catch {
      return null;
    }
  }

  const mode: Writable<FilterMode | null> = writable(readStored());

  function select(next: FilterMode) {
    mode.set(next);
    try {
      localStorage.setItem(FEED_MODE_STORAGE_KEY, next);
    } catch {
      // Best-effort: the selection still works for the rest of this session.
    }
  }

  function seedFromSettings(defaultMode: FilterMode) {
    if (get(mode) == null) mode.set(defaultMode);
  }

  try {
    window.addEventListener("storage", (e) => {
      if (e.key !== FEED_MODE_STORAGE_KEY) return;
      const v = readStored();
      if (v != null) mode.set(v);
    });
  } catch {
    // No `window` (unit tests, SSR) — the store still works within one process.
  }

  return { mode, select, seedFromSettings };
}

export const feedModeStore = createFeedModeStore();

/** `feedModeStore.mode`'s explicit choice (or `null`, before one has been
 * made) resolved against `Settings.filter_mode`'s starting value — the same
 * one-line fallback `Feed.svelte` computes inline as `effectiveMode`, pulled
 * out here so every other view wires it the same way instead of retyping the
 * `??` six more times. */
export function effectiveFilterMode(explicit: FilterMode | null, settingsDefault: FilterMode): FilterMode {
  return explicit ?? settingsDefault;
}

/** "Nothing silently empty" (packet gm-scope-r1, work item 5): every
 * filterable list view calls this before falling back to its own plain
 * empty-state copy, so a filter combination that matches nothing says why
 * instead of reading like a broken poll. `subject` is the plural noun for
 * what's missing ("commits", "comments", "watched threads", ...).
 * `teamLabel` — the selected team's own name, when a view has a team
 * switcher and one is picked — takes priority over the coarser `mode`
 * wording since it says more ("for Platform" beats "for your team"); when
 * neither a team nor `mode: "team"` narrows anything, the mode clause is
 * dropped entirely rather than stating the default. Returns `null` when
 * nothing narrows the view at all, so the caller's own generic "no activity
 * yet" message stays in charge — this only speaks up once a filter is
 * plausibly *why* the list came back empty. */
export function scopeEmptyMessage(
  subject: string,
  opts: { repoLabel?: string | null; teamLabel?: string | null; mode: FilterMode },
): string | null {
  const clauses: string[] = [];
  if (opts.repoLabel) clauses.push(`in ${opts.repoLabel}`);
  if (opts.teamLabel) clauses.push(`for ${opts.teamLabel}`);
  else if (opts.mode === "team") clauses.push("for your team");
  if (clauses.length === 0) return null;
  return `No ${subject} ${clauses.join(" ")}.`;
}
