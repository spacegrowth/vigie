<script lang="ts">
  // Watched (docs/design/Watched.dc.html): every thread `list_watches`
  // returns, newest `since` first — the sidebar's other half of "Watch" in
  // the detail pane header.
  import { onMount } from "svelte";
  import { openUrl } from "@tauri-apps/plugin-opener";
  import { watchesStore, reposStore, detailStore, type DetailTarget } from "../lib/stores";
  import { repoFilterStore, scopeEmptyMessage } from "../lib/feed-filters";
  import { scroller } from "../lib/scroller";
  import { relativeTime } from "../lib/time";
  import type { Watch, WatchSource } from "../lib/types";
  import Icon from "../components/Icon.svelte";

  const { items } = watchesStore;
  const { items: repoItems } = reposStore;
  const { selected: repoFilter } = repoFilterStore;

  /** The app-wide repo filter, applied client-side: `Watch` (unlike `Event`)
   * carries no actor, so the "My team / Everyone" mode has nothing to filter
   * by here and is left out entirely — showing it would imply a scoping this
   * view can't actually do (packet gm-scope-r1, "not showing one that
   * lies"). `list_watches` itself also takes no `repo_id` (or any filter) to
   * push this down to the engine — see docs/CONTRACT.md's watches section —
   * so this narrows the same list `watchesStore` already loaded in full. */
  let filteredItems = $derived($repoFilter == null ? $items : $items.filter((w) => w.repo_id === $repoFilter));

  /** The selected repo's own label, for the "nothing silently empty" message
   * below. */
  let filterRepoLabel = $derived.by(() => {
    const r = $repoFilter != null ? $repoItems.find((r) => r.id === $repoFilter) : null;
    return r ? `${r.owner}/${r.name}` : null;
  });

  let loading = $state(true);
  let clearing = $state(false);
  let busyKey = $state<string | null>(null);

  onMount(async () => {
    try {
      await watchesStore.refresh();
    } finally {
      loading = false;
    }
  });

  function repoLabel(repoId: number): string {
    const r = $repoItems.find((r) => r.id === repoId);
    return r ? `${r.owner}/${r.name}` : "";
  }

  /** `null` when the repo isn't known yet — the button is hidden rather
   * than link to a guessed URL. */
  function watchUrl(w: Watch): string | null {
    const r = $repoItems.find((r) => r.id === w.repo_id);
    if (!r) return null;
    return `https://github.com/${r.owner}/${r.name}/${w.kind === "pull" ? "pull" : "issues"}/${w.number}`;
  }

  /** Sibling of `.wtitle`/`.unwatch`, never nested inside either — see
   * PullRequests.svelte's identical recipe. */
  function openOnGitHub(e: MouseEvent, url: string) {
    e.stopPropagation();
    openUrl(url).catch(() => {
      // best-effort — same as DetailPane's openOnGitHub
    });
  }

  function key(w: Watch): string {
    return `${w.repo_id}:${w.number}`;
  }

  // `Watch.state` is "as last seen" (docs/CONTRACT.md "Watched threads"):
  // open|closed|merged for a PR, open|closed for an issue — anything else
  // falls back to the plain state string rather than hiding it.
  const STATE_LABEL: Record<string, string> = { open: "Open", merged: "Merged", closed: "Closed" };
  const STATE_CLASS: Record<string, string> = { open: "st-open", merged: "st-merged", closed: "st-closed" };
  const SOURCE_LABEL: Record<WatchSource, string> = {
    manual: "Manual",
    author: "You authored",
    reviewer: "Review requested",
  };

  // `clearClosed` below (`clearClosedWatches`) has no `repo_id` to scope by —
  // it always sweeps every closed watch, fleet-wide — so whether to show the
  // button reads the unfiltered `$items`, matching what a click actually
  // does; only its label says "across every repo" once a repo filter would
  // otherwise make that scope a surprise (same "say so when filtered" rule
  // Feed.svelte's own backfill button follows).
  let hasClosed = $derived($items.some((w) => w.state !== "open"));

  /** The reader-mode stepper's list (packet 002): every watched thread
   * *currently shown* (i.e. narrowed by the app-wide repo filter), in the
   * list's own rendered order. */
  let watchedSiblings = $derived(
    filteredItems.map((w): DetailTarget => ({ kind: "thread", repoId: w.repo_id, number: w.number })),
  );

  async function unwatch(w: Watch) {
    busyKey = key(w);
    try {
      await watchesStore.unwatch(w.repo_id, w.number);
    } catch {
      // best-effort — the row simply stays if the call failed
    } finally {
      busyKey = null;
    }
  }

  async function clearClosed() {
    clearing = true;
    try {
      await watchesStore.clearClosed();
    } finally {
      clearing = false;
    }
  }
</script>

<div class="head">
  <h1>Watched</h1>
  {#if !loading}<span class="muted">{filteredItems.length} thread{filteredItems.length === 1 ? "" : "s"}</span>{/if}
  <span class="spacer"></span>
  {#if hasClosed}
    <button class="tbtn" onclick={clearClosed} disabled={clearing}>
      {clearing ? "Clearing…" : $repoFilter != null ? "Clear closed (all repos)" : "Clear closed"}
    </button>
  {/if}
</div>

<div class="body" use:scroller>
  {#if loading}
    <p class="muted">Loading…</p>
  {:else if $items.length === 0}
    <div class="zero-state">
      <p class="muted">Nothing watched yet — open a pull request or issue and click Watch.</p>
    </div>
  {:else if filteredItems.length === 0}
    <div class="zero-state">
      <p class="muted">{scopeEmptyMessage("watched threads", { repoLabel: filterRepoLabel, mode: "all" }) ?? "Nothing watched yet."}</p>
    </div>
  {:else}
    {#each filteredItems as w (key(w))}
      <div class="wrow">
        <div class="row">
          <span class="badge {STATE_CLASS[w.state] ?? 'st-open'}">{STATE_LABEL[w.state] ?? w.state}</span>
          <button
            class="wtitle"
            onclick={() => detailStore.openThread(w.repo_id, w.number, { origin: "Watched", siblings: watchedSiblings })}
            title={w.title}
          >
            {w.title}
          </button>
          <span class="muted ref">{repoLabel(w.repo_id)} #{w.number}</span>
          <span class="spacer"></span>
          {#if watchUrl(w)}
            <button class="ext-link" onclick={(e) => openOnGitHub(e, watchUrl(w)!)} title="Open on GitHub">
              <Icon name="external" size={13} />
            </button>
          {/if}
          <button class="unwatch" onclick={() => unwatch(w)} disabled={busyKey === key(w)}>
            {busyKey === key(w) ? "Unwatching…" : "Unwatch"}
          </button>
        </div>
        <span class="muted">{SOURCE_LABEL[w.source]} · {relativeTime(w.since)}</span>
      </div>
    {/each}
  {/if}
</div>

<style>
  .head {
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 12px;
    padding: 18px 24px 12px 24px;
  }
  .head h1 {
    font-size: 17px;
    font-weight: 600;
    margin: 0;
  }
  .spacer {
    flex-grow: 1;
  }
  .tbtn {
    font-size: 13px;
    color: var(--accent);
    background: none;
    border: none;
    padding: 4px 8px;
    border-radius: 6px;
    cursor: pointer;
    transition: background 0.12s;
  }
  .tbtn:hover:not(:disabled) {
    background: var(--surface-active);
  }
  .tbtn:disabled {
    opacity: 0.7;
    cursor: default;
  }
  .body {
    padding: 0 24px 24px 24px;
    overflow-y: auto;
    flex-grow: 1;
  }
  .zero-state {
    padding: 40px 0;
  }
  .wrow {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 12px 10px;
    margin: 0 -10px;
    border-radius: 8px;
    border-bottom: 1px solid var(--divider);
    transition: background 0.12s;
  }
  .wrow:last-child {
    border-bottom: none;
  }
  .wrow:hover {
    background: var(--surface-active);
    border-bottom-color: transparent;
  }
  .row {
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 10px;
  }
  .badge {
    font-size: 11px;
    padding: 2px 7px;
    border-radius: 4px;
    white-space: nowrap;
    font-weight: 500;
    flex-shrink: 0;
  }
  .st-open {
    background: var(--badge-open-bg);
    color: var(--badge-open-fg);
  }
  .st-merged {
    background: var(--badge-merged-bg);
    color: var(--badge-merged-fg);
  }
  .st-closed {
    background: var(--badge-closed-bg);
    color: var(--badge-closed-fg);
  }
  .wtitle {
    font-size: 13px;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex-grow: 1;
    min-width: 0;
    text-align: left;
    background: none;
    border: none;
    padding: 0;
    color: inherit;
    cursor: pointer;
    font-family: inherit;
  }
  .wtitle:hover {
    color: var(--accent);
  }
  .ref {
    flex-shrink: 0;
  }
  /* Secondary to `.wtitle`'s in-app open — muted until the row is hovered
     or the button itself gets focus, same reveal-on-hover recipe as
     `.unwatch` right below (this packet's "same treatment audit"). A
     sibling of `.wtitle`, never nested inside it, so it can never also
     trigger the row's own open. */
  .ext-link {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    padding: 0;
    background: none;
    border: none;
    border-radius: 6px;
    color: var(--muted);
    cursor: pointer;
    flex-shrink: 0;
    opacity: 0;
    transition: opacity 0.12s, background 0.12s, color 0.12s;
  }
  .wrow:hover .ext-link,
  .ext-link:focus-visible {
    opacity: 1;
  }
  .ext-link:hover {
    background: var(--surface-sunken);
    color: var(--text);
  }
  /* Hidden until the row is hovered or the button itself gets focus, per the
     packet's "row hover shows Unwatch" — a keyboard user still reaches it via
     Tab, since :focus-visible overrides the hidden state too. */
  .unwatch {
    font-size: 12px;
    color: var(--danger);
    background: none;
    border: none;
    padding: 2px 8px;
    border-radius: 6px;
    cursor: pointer;
    flex-shrink: 0;
    opacity: 0;
    transition: opacity 0.12s, background 0.12s;
  }
  .wrow:hover .unwatch,
  .unwatch:focus-visible {
    opacity: 1;
  }
  .unwatch:hover:not(:disabled) {
    background: var(--surface-sunken);
  }
  .unwatch:disabled {
    cursor: default;
  }
</style>
