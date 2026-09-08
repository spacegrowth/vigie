<script lang="ts">
  // Content of the tray-popover window (docs/design/Tray.dc.html). Runs in
  // its own webview — a separate JS runtime from the main window, so it
  // can't just call the main window's `detailStore` directly. Instead it
  // emits a global Tauri event that the main window listens for (see
  // App.svelte), which opens its own detail pane and brings itself
  // forward; this window then hides itself.
  import { onMount, onDestroy } from "svelte";
  import { emit, listen } from "@tauri-apps/api/event";
  import { getVersion } from "@tauri-apps/api/app";
  import { check as checkForUpdate } from "@tauri-apps/plugin-updater";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { api } from "../lib/api";
  import { groupThreads, type EventGroup } from "../lib/threads";
  import { teamFilterStore, settingsStore, type DetailTarget } from "../lib/stores";
  import { repoFilterStore, feedModeStore, effectiveFilterMode } from "../lib/feed-filters";
  import type { Event, PollResult } from "../lib/types";
  import ThreadBlock from "../components/ThreadBlock.svelte";
  import EventRow from "../components/EventRow.svelte";
  import Icon from "../components/Icon.svelte";

  /** Shown in the footer instead of a one-off instruction: the app's own
   * version is the thing worth having in a panel you glance at every day. */
  let appVersion = $state<string | null>(null);
  /** Someone who lives in this popover and rarely opens the window would
   * otherwise never learn an update exists — the banner announcing one is in
   * the main window. Checked here too, quietly: a version and a way through,
   * never a download or an install from a menu. */
  let updateVersion = $state<string | null>(null);

  let events = $state<Event[]>([]);
  let lastPolledAt = $state<number | null>(null);
  let polling = $state(false);
  let now = $state(Date.now() / 1000);

  // The tray popover has no chips of its own — it uses whatever the main
  // window's team switcher, repo filter and "My team / Everyone" mode last
  // selected (docs/CONTRACT.md work item 2; packet gm-scope-r1 adds the
  // latter two), read from the same localStorage-backed stores.
  const { selected: teamFilter } = teamFilterStore;
  const { selected: repoFilter } = repoFilterStore;
  const { mode: feedMode } = feedModeStore;
  const { settings } = settingsStore;

  /** Same fallback Feed.svelte computes inline, and every other view now
   * reads via `effectiveFilterMode` — this window needs its own real
   * `Settings.filter_mode` (not the placeholder default `settingsStore`
   * starts with) for that fallback to mean anything before an explicit
   * choice has ever synced in over `storage`, hence the `settingsStore.refresh()`
   * below. */
  let effectiveMode = $derived(effectiveFilterMode($feedMode, $settings.filter_mode));

  async function load() {
    events = await api.listEvents({ limit: 30, teamId: $teamFilter, repoId: $repoFilter, mode: effectiveMode });
  }

  async function pollNow() {
    polling = true;
    try {
      await api.pollNow();
      lastPolledAt = Date.now() / 1000;
      await load();
    } finally {
      polling = false;
    }
  }

  function openMainWindow() {
    void emit("focus-main-window");
    void getCurrentWindow().hide();
  }

  /** Escape closes the popover like any other menu (this packet, Work item
   * 5). Losing key-window focus is what normally hides this window
   * (lib.rs's `WindowEvent::Focused(false)` handler) — but pressing
   * Escape inside the webview doesn't move focus anywhere on its own, so
   * that handler never sees it; this window has to close itself. */
  function handleKeydown(event: KeyboardEvent) {
    if (event.key === "Escape") {
      void getCurrentWindow().hide();
    }
  }

  /** Passed as ThreadBlock's onOpen and EventRow's onOpen override — both
   * resolve down to one detail target, which this hands to the main
   * window (see module doc) rather than this window's own (unused,
   * unmounted) DetailPane. */
  function openInMainWindow(target: DetailTarget) {
    void emit("open-main-detail", target);
    void getCurrentWindow().hide();
  }

  let unlisten: (() => void) | undefined;
  let clockInterval: ReturnType<typeof setInterval> | undefined;
  onMount(async () => {
    getVersion()
      .then((v) => (appVersion = v))
      .catch(() => (appVersion = null));
    checkForUpdate()
      .then((u) => (updateVersion = u?.version ?? null))
      .catch(() => (updateVersion = null));
    lastPolledAt = Date.now() / 1000;
    unlisten = await listen<PollResult>("new-events", () => load());
    clockInterval = setInterval(() => (now = Date.now() / 1000), 15_000);
    window.addEventListener("keydown", handleKeydown);
    settingsStore.refresh();
  });
  onDestroy(() => {
    unlisten?.();
    if (clockInterval) clearInterval(clockInterval);
    window.removeEventListener("keydown", handleKeydown);
  });

  // Loads on mount, and again on a team, repo, or mode change made in the
  // main window while this popover stays alive (hidden, not unmounted,
  // between openings) — each of the three syncs in over its own store's
  // `storage` listener.
  $effect(() => {
    $teamFilter;
    $repoFilter;
    effectiveMode;
    load();
  });

  interface Row {
    sortKey: number;
    thread: EventGroup | null;
    single: Event | null;
  }
  let rows = $derived.by(() => {
    const { threads, standalone } = groupThreads(events);
    const combined: Row[] = [
      ...threads.map((t) => ({ sortKey: t.sortKey, thread: t, single: null })),
      ...standalone.map((e) => ({ sortKey: e.occurred_at, thread: null, single: e })),
    ];
    combined.sort((a, b) => b.sortKey - a.sortKey);
    return combined.slice(0, 8);
  });

  /** Doubles as the switch for the rows' unseen-dot column below (this
   * packet, "collapse the whole column when nothing in the list is
   * unseen") — when nothing polled is unseen, every row's dot would render
   * off anyway, so hiding the column outright removes the dead 16px gutter
   * (dot width + its flex gap) that a "0 new" popover otherwise shows down
   * its whole list. */
  let newCount = $derived(events.filter((e) => !e.seen).length);
  let polledAgo = $derived.by(() => {
    if (lastPolledAt == null) return "not polled yet";
    const secs = Math.max(0, Math.round(now - lastPolledAt));
    return secs < 60 ? "just now" : `${Math.round(secs / 60)} min ago`;
  });
</script>

<div class="popover">
  <div class="head">
    <span class="count">{newCount} new</span>
    <span class="muted">· polled {polledAgo}</span>
    <span class="spacer"></span>
    <button class="tbtn" onclick={pollNow} disabled={polling}>
      {#if polling}<span class="spin"><Icon name="spinner" size={12} /></span>{/if}
      {polling ? "Polling…" : "Poll now"}
    </button>
    <button class="tbtn" onclick={openMainWindow}>Open</button>
  </div>

  <!-- No scroll indicator here on purpose: this is a menu, and it was asked
       that it never show one. The list still scrolls. -->
      <div class="list">
    {#if rows.length === 0}
      <p class="muted empty">Nothing yet — poll to check for activity.</p>
    {/if}
    {#each rows as row (row.thread ? `t${row.thread.repoId}:${row.thread.number}` : `s${row.single!.id}`)}
      {#if row.thread}
        <ThreadBlock
          thread={row.thread}
          onOpen={(repoId, number) => openInMainWindow({ kind: "thread", repoId, number })}
          onOpenChild={openInMainWindow}
          showUnseenDot={newCount > 0}
          fullPreview
          iconOnly
        />
      {:else}
        <EventRow event={row.single!} onOpen={openInMainWindow} showUnseenDot={newCount > 0} fullPreview iconOnly />
      {/if}
    {/each}
  </div>

  <div class="foot muted">
    {#if updateVersion}
      <button class="foot-update" onclick={openMainWindow} title="Open Vigie to install {updateVersion}">
        Update to {updateVersion}
      </button>
    {/if}
    <span class="foot-name">Vigie</span>{#if appVersion}<span class="foot-version">{appVersion}</span>{/if}
  </div>
</div>

<style>
  /* Checked every border-radius in app/src for a main-window convention to
     match: nothing comparable exists. The main window has no floating
     "outermost card" of its own to line up with — it's a normally
     decorated OS window whose corner rounding macOS draws natively, not
     this stylesheet — and every existing radius in the codebase belongs
     to a small control (chips/pills at 12-14px, a settings toggle at
     10px, buttons at 4-8px), none of them a large panel like this one.
     Kept at 10px: close to macOS's own native window-corner radius, and
     already what this file had. `overflow: hidden` here (not on `.list`
     — see its own comment below) is what actually clips everything
     inside to this radius: `.head`/`.list`/`.foot` are plain flow
     children with no absolute positioning that could escape it, and
     `.list`'s own scroll-clip buffer stays well inside this box's edges,
     not out at its rounded corners. This card is only genuinely visible
     over a transparent window background — see tray.rs's `setup()` doc
     comment for why that part is currently blocked. */
  /* Fills the window rather than sizing to its content. The window is a
     fixed 400x560; a card that shrank to fit its rows left the rest of the
     window painted as dead space below it. No radius or shadow either: the
     window itself is square and opaque, so a rounded card just draws its
     own corners against the window's, and the shadow lands inside the
     panel instead of around it. */
  .popover {
    width: 100%;
    height: 100vh;
    background: var(--surface-card);
    display: flex;
    flex-direction: column;
    padding: 12px 16px;
    box-sizing: border-box;
    overflow: hidden;
  }
  .head {
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 6px;
    padding-bottom: 4px;
    flex-shrink: 0;
  }
  .count {
    font-size: 13px;
    font-weight: 600;
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
    display: flex;
    align-items: center;
    gap: 4px;
    transition: background 0.12s;
  }
  .tbtn:hover:not(:disabled) {
    background: var(--surface-active);
  }
  .tbtn:active:not(:disabled) {
    background: var(--surface-sunken);
  }
  .spin {
    display: inline-flex;
    animation: spin 0.8s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  /* overflow-y: auto here means overflow-x computes to auto too, not
     visible (the CSS overflow spec coerces a `visible` axis to `auto`
     once the other axis isn't `visible`) — so EventRow's agent badge,
     which overhangs the avatar by 2px via absolute positioning, would
     get clipped at .list's own edges even though nothing here scrolls
     horizontally. Padding pushes the clip boundary (the padding edge)
     8px past the rows' content edge; the matching negative margin pulls
     .list's own box back in by the same amount so the rows — stretched
     to .list's content width by the flex parent — end up exactly the
     width they were before. Vertical scrolling is untouched. */
  .list {
    overflow-y: auto;
    /* The rows size themselves with container queries (EventRow.svelte),
       and a container query needs an ancestor that declares itself one.
       Only Feed.svelte's body did, so inside this 400px panel every row
       still laid out at main-window widths — repo label, 90px login, 120px
       minimum title, 70px time — roughly 500px of content in ~348px of
       room. Each row overflowed sideways, and because a box that scrolls on
       one axis becomes scrollable on the other, a trackpad gesture dragged
       the whole list left. Declaring the container is the actual repair;
       `overflow-x: hidden` is the belt to go with it. */
    container-type: inline-size;
    overflow-x: hidden;
    /* Rows sit on the panel here, not on the app's `--surface`, so the
       agent badge's cut-out disc must match the panel or it draws a pale
       ring on white. */
    --row-bg: var(--surface-card);
    overscroll-behavior: contain;
    flex-grow: 1;
    /* No negative margin here. It used to pull the list 8px outside
       `.popover`'s content box to give EventRow's agent badge room to
       overhang the avatar — but `.popover` is `overflow: hidden`, so the
       list's left edge was clipped instead and every avatar lost its left
       8px. The badge only overhangs 2px, to the RIGHT and BOTTOM of the
       avatar, and the avatar sits well inside the row, so it needs no
       extra room on the left at all. */
    padding-right: 4px;
  }
  .empty {
    padding: 20px 0;
    text-align: center;
  }
  .foot {
    padding-top: 10px;
    flex-shrink: 0;
    display: flex;
    justify-content: flex-end;
    align-items: baseline;
    gap: 5px;
    font-size: 11px;
  }
  .foot-update {
    margin-right: auto;
    background: none;
    border: none;
    padding: 0;
    font: inherit;
    color: var(--accent);
    font-weight: 600;
    cursor: pointer;
  }
  .foot-name {
    font-weight: 600;
  }
</style>
