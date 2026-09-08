<script lang="ts">
  // A thread's header line ("title #N · repo") plus its indented child event
  // rows — the shape docs/design/Main.dc.html and Tray.dc.html share, so
  // both Feed and the tray popover render threads through this. The
  // header's chevron toggles the thread collapsed/expanded (state lives in
  // lib/stores.ts's collapsedThreadsStore, not component state, so it
  // survives Feed remounts); clicking anywhere else on the header opens the
  // detail pane split, same as today — double-click or Enter opens it
  // straight into reader mode instead (see `openReader` below).
  import { childActionText, type EventGroup } from "../lib/threads";
  import { collapsedThreadsStore, detailStore, sameTarget, type DetailTarget } from "../lib/stores";
  import EventRow from "./EventRow.svelte";
  import Icon from "./Icon.svelte";

  let {
    thread,
    repoLabel,
    onOpen,
    onOpenChild,
    onSeen,
    onFilterActor,
    showUnseenDot = true,
    showPreview = true,
    fullPreview = false,
    iconOnly = false,
  }: {
    thread: EventGroup;
    repoLabel?: string;
    onOpen: (repoId: number, number: number) => void;
    /** Forwarded to each child row's own `onOpen` override — see
     * EventRow's doc: the tray popover needs this since its `detailStore`
     * isn't the same one the main window renders a pane from. Feed omits
     * it and gets the normal in-window pane. */
    onOpenChild?: (target: DetailTarget) => void;
    onSeen?: (id: number) => void;
    onFilterActor?: (login: string) => void;
    /** Forwarded to the header's own dot and every child EventRow — see
     * EventRow's doc (packet gm-popover-r2). */
    showUnseenDot?: boolean;
    /** Forwarded to every child EventRow — see EventRow's doc (packet
     * gm-popover-r2). The thread header itself never had a body preview. */
    showPreview?: boolean;
    /** Forwarded to every child EventRow — see EventRow's own doc. The
     * thread header has no body preview of its own to switch modes on. */
    fullPreview?: boolean;
    /** Forwarded to every child EventRow's KindBadge — see EventRow's own
     * doc. The thread header has no KindBadge of its own. */
    iconOnly?: boolean;
  } = $props();

  let hasUnseen = $derived(thread.events.some((e) => !e.seen));
  // A thread's watched-ness is per (repo_id, number), so every member event
  // agrees — checking one is exact, not a heuristic.
  let watched = $derived(thread.events.some((e) => e.watched));

  // Whether *this* thread is the one open in the detail pane — every child
  // row maps to the same thread target (they share repoId+number), so the
  // highlight lives on the thread's own header rather than trying to pick
  // one ambiguous "open" child (docs/CONTRACT.md item 2: "only one row is
  // ever selected"). Reads `detailStore` directly rather than a prop: in
  // the tray popover's separate webview this is a different, always-null
  // store instance, so the highlight there naturally never lights up
  // (that window never keeps its own pane open — see TrayPopover.svelte).
  const { target: openTarget } = detailStore;
  let selected = $derived(sameTarget($openTarget, { kind: "thread", repoId: thread.repoId, number: thread.number }));

  const { collapsed: collapsedThreads, key: threadKey, toggle: toggleCollapsed } = collapsedThreadsStore;
  let isCollapsed = $derived($collapsedThreads.has(threadKey(thread.repoId, thread.number)));

  // A single click on the header (or Space) keeps opening split, via
  // `onOpen` alone, as before. Double-click or Enter opens straight into
  // reader mode: `onOpen` itself has no reader-mode equivalent (the tray
  // popover's override just forwards the target to the main window's
  // default pane), so this reaches into the local `detailStore` directly
  // afterward — in the tray popover that's its own unmounted pane's store,
  // a harmless no-op there (see EventRow's `onOpen` doc for the same case).
  function openReader() {
    onOpen(thread.repoId, thread.number);
    detailStore.enterReader();
  }

  // Enter's default button activation would otherwise fire the same click
  // `onOpen` handles (opening split) before we get a chance to look at it —
  // preventDefault suppresses that synthetic click so only this fires.
  function handleTitleKeydown(e: KeyboardEvent) {
    if (e.key === "Enter") {
      e.preventDefault();
      openReader();
    }
  }
</script>

<div class="thread" class:watched>
  <div class="thread-head" class:selected>
    <button
      class="chevron-btn"
      onclick={() => toggleCollapsed(thread.repoId, thread.number)}
      aria-label={isCollapsed ? "Expand thread" : "Collapse thread"}
      aria-expanded={!isCollapsed}
    >
      <span class="chevron-icon" class:collapsed={isCollapsed}>
        <Icon name="chevron" size={12} />
      </span>
    </button>
    <button
      class="thread-main"
      onclick={() => onOpen(thread.repoId, thread.number)}
      ondblclick={openReader}
      onkeydown={handleTitleKeydown}
    >
      {#if showUnseenDot}<span class="dot" class:on={hasUnseen}></span>{/if}
      {#if watched}
        <span class="watch-eye" title="Watched"><Icon name="eye_filled" size={12} /></span>
      {/if}
      <span class="thread-title">{thread.title}</span>
      <span class="muted">#{thread.number}</span>
      {#if repoLabel}
        <span class="muted repo">· {repoLabel}</span>
      {/if}
      {#if isCollapsed}
        <span class="muted">· {thread.events.length} event{thread.events.length === 1 ? "" : "s"}</span>
      {/if}
      <span class="spacer"></span>
    </button>
  </div>
  {#if !isCollapsed}
    <div class="thread-children">
      {#each thread.events as child (child.id)}
        <EventRow
          event={child}
          compact
          titleOverride={childActionText(child.kind)}
          onOpen={onOpenChild}
          {showUnseenDot}
          {showPreview}
          {fullPreview}
          {iconOnly}
          {onSeen}
          {onFilterActor}
        />
      {/each}
    </div>
  {/if}
</div>

<style>
  .thread {
    border-bottom: 1px solid var(--divider);
    padding-bottom: 4px;
  }
  .thread-head {
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 8px;
    /* Symmetric, so the title sits in the middle of the `.selected`
       highlight rather than low in it — the padding used to be 12px above
       and 4px below, which only showed once the row could be highlighted.
       The space that used to come from the extra top padding is a margin
       now, so it stays outside the highlight where it belongs. */
    margin-top: 8px;
    /* Matches EventRow's `.row` padding so a thread's header and the rows
       beneath it share one left edge inside the highlight. */
    padding: 6px 10px;
    border-radius: 6px;
  }
  /* The thread whose target is open in the detail pane — see EventRow's
     `.row.selected` for the standalone-row equivalent. */
  .thread-head.selected {
    background: var(--surface-active);
  }
  .dot {
    width: 6px;
    height: 6px;
    border-radius: 3px;
    background: transparent;
    flex-shrink: 0;
  }
  .dot.on {
    background: var(--accent);
  }
  /* Same responsive treatment as EventRow's `.title` (this packet, "keep
     the two aligned"): wins at least 120px so a long title never reads as
     four characters when the pane is open and the row is narrow. */
  .thread-title {
    font-size: 13px;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex: 1 1 40%;
    min-width: 120px;
  }
  /* Hidden under the same 640px container query as EventRow's `.repo` —
     the repo already shows in the pane header and breadcrumb. Split from
     the "#123" span (which always stays) so only this part disappears. */
  .repo {
    flex-shrink: 0;
  }
  @container (max-width: 640px) {
    .repo {
      display: none;
    }
  }
  .spacer {
    flex-grow: 1;
  }
  .chevron-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
    background: none;
    border: none;
    padding: 2px;
    cursor: pointer;
    color: var(--muted);
  }
  .chevron-icon {
    display: flex;
    transition: transform 0.15s ease;
  }
  .chevron-icon.collapsed {
    transform: rotate(-90deg);
  }
  /* The rest of the header (everything but the chevron) — a button so the
     whole row, not just the title text, opens the detail pane; the hover
     treatment mirrors EventRow's `.title:hover .title-text`. */
  .thread-main {
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 8px;
    flex: 1 1 auto;
    min-width: 0;
    background: none;
    border: none;
    padding: 0;
    cursor: pointer;
    color: var(--text);
    text-align: left;
  }
  .thread-main:hover .thread-title {
    color: var(--accent);
  }
  .thread-children {
    display: flex;
    flex-direction: column;
    padding-left: 14px;
  }
</style>
