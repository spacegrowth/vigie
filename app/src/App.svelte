<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { check as checkForUpdate, type Update } from "@tauri-apps/plugin-updater";
  import { api } from "./lib/api";
  import type { PollResult } from "./lib/types";
  import { clockTime } from "./lib/time";
  import {
    reposStore,
    teamsStore,
    settingsStore,
    unseenStore,
    badgeStore,
    pollStore,
    detailStore,
    watchesStore,
    PANE_WIDTH_DEFAULT,
    type DetailTarget,
    type RateLimitNotice,
  } from "./lib/stores";
  import { repoFilterStore, feedModeStore, effectiveFilterMode } from "./lib/feed-filters";
  import Sidebar, { type ViewName } from "./components/Sidebar.svelte";
  import StatusBar from "./components/StatusBar.svelte";
  import DetailPane from "./components/DetailPane.svelte";
  import Onboarding from "./views/Onboarding.svelte";
  import Feed from "./views/Feed.svelte";
  import PullRequests from "./views/PullRequests.svelte";
  import Commits from "./views/Commits.svelte";
  import Comments from "./views/Comments.svelte";
  import Issues from "./views/Issues.svelte";
  import Summary from "./views/Summary.svelte";
  import People from "./views/People.svelte";
  import Repos from "./views/Repos.svelte";
  import Teams from "./views/Teams.svelte";
  import Watched from "./views/Watched.svelte";
  import Settings from "./views/Settings.svelte";
  import UpdatePrompt from "./components/UpdatePrompt.svelte";

  let checkingToken = $state(true);
  let needsOnboarding = $state(false);
  // The quiet on-launch update check (docs/RELEASING.md); UpdatePrompt.svelte
  // renders the actual "install or dismiss" prompt once one is found — see
  // `checkForUpdatesQuietly` below for why any failure here stays silent.
  let pendingUpdate = $state<Update | null>(null);
  let view = $state<ViewName>("feed");
  let feedActorFilter = $state<string | null>(null);
  let summaryActorFilter = $state<string | null>(null);

  // Ticks so the sidebar's "polled Xm ago" / countdown stays fresh even
  // when nothing else re-renders it. Passed down as a plain prop (not a
  // `{#key}` remount) so it drives ordinary fine-grained reactivity.
  let now = $state(Date.now() / 1000);

  // The split-layout divider (below): live-drags `detailStore.paneWidth`,
  // snaps on release, and hands off to reader mode past its far edge.
  let contentEl: HTMLDivElement | undefined = $state();
  let dividerDragging = $state(false);
  const PANE_WIDTH_SNAP_POINTS = [0.35, 0.5, 0.65];
  const PANE_WIDTH_SNAP_TOLERANCE = 0.04;
  /** Past this fraction, a divider drag hands off to reader mode instead of
   * clamping at `paneWidth`'s own 0.7 max. */
  const READER_HANDOFF_FRACTION = 0.85;
  const PANE_WIDTH_ARROW_STEP = 0.02;

  const { settings } = settingsStore;
  const { counts: badgeCounts } = badgeStore;
  const { target: detailTarget, layout: detailLayout, paneWidth, stack: detailStack } = detailStore;
  const { lastPolledAt, rateLimit } = pollStore;
  const { items: repoItems } = reposStore;

  // The app-wide repo filter and "My team / Everyone" mode (packet
  // gm-scope-r1): one control, rendered once here, so picking a repo in
  // Commits and switching to Issues shows it still applied. Feed.svelte
  // already renders its own equivalent copy of this exact same control
  // (same two stores) inline in its own filter row — out of this packet's
  // boundary, so left untouched rather than duplicated here too — hence
  // `SCOPE_FILTER_VIEWS` excludes "feed". `repos`/`teams`/`settings` aren't
  // Activity views at all, and `summary` is the deliberate exception this
  // control can't honestly apply to (see Summary.svelte's own note) — so all
  // four stay out too.
  const SCOPE_FILTER_VIEWS: ViewName[] = ["prs", "commits", "comments", "issues", "people", "watched"];
  let showScopeFilter = $derived(SCOPE_FILTER_VIEWS.includes(view));
  // `Watch` (Watched.svelte) carries no actor to filter by, so the mode
  // toggle has nothing to do there — showing it anyway would be a control
  // that visibly does nothing, which the packet calls out by name. See
  // Watched.svelte's own comment.
  let showModeToggle = $derived(showScopeFilter && view !== "watched");
  let scopeRepoOptions = $derived([...$repoItems].sort((a, b) => `${a.owner}/${a.name}`.localeCompare(`${b.owner}/${b.name}`)));

  const { selected: scopeRepoFilter, select: selectScopeRepo } = repoFilterStore;
  const { mode: scopeFeedMode, select: selectScopeMode } = feedModeStore;
  let scopeEffectiveMode = $derived(effectiveFilterMode($scopeFeedMode, $settings.filter_mode));

  // `e` left untyped, matching Feed.svelte's identical handler.
  function handleScopeRepoChange(e: { currentTarget: HTMLSelectElement }) {
    const value = e.currentTarget.value;
    selectScopeRepo(value === "" ? null : Number(value));
  }

  let unlistenNewEvents: (() => void) | undefined;
  let unlistenRateLimited: (() => void) | undefined;
  let unlistenSignedOut: (() => void) | undefined;
  let unlistenOpenDetail: (() => void) | undefined;
  let unlistenCheckUpdates: (() => void) | undefined;
  let unlistenFocusMain: (() => void) | undefined;
  let clockInterval: ReturnType<typeof setInterval> | undefined;

  onMount(async () => {
    try {
      // Two independent signs of "someone is signed in": the legacy
      // keychain item (not yet migrated, on a launch racing `lib.rs`'s
      // startup migration) and the accounts list (every launch after).
      // Either one is enough — see docs/CONTRACT.md "Accounts".
      const [token, accounts] = await Promise.all([api.loadToken(), api.listAccounts()]);
      needsOnboarding = token === null && accounts.length === 0;
    } catch {
      needsOnboarding = true;
    } finally {
      checkingToken = false;
    }

    await Promise.all([
      reposStore.refresh(),
      teamsStore.refresh(),
      settingsStore.refresh(),
      unseenStore.refresh(),
      badgeStore.refresh(),
      watchesStore.refresh(),
    ]);

    unlistenNewEvents = await listen<PollResult>("new-events", (event) => {
      // A scheduled background poll (poller.rs's timer) relays its result
      // here rather than through `pollStore.pollNow` — feed it through the
      // same `applyPollResult` so the status bar's "Polled Xm ago" and its
      // per-repo dots stay current even when the user never pressed Cmd+R.
      pollStore.applyPollResult(event.payload);
      unseenStore.refresh();
      badgeStore.refresh();
    });

    // Being throttled is a state of the app, not an event: it raises a
    // banner here rather than an OS notification (see poller.rs).
    unlistenRateLimited = await listen<RateLimitNotice>("poll-rate-limited", (event) => {
      pollStore.setRateLimit(event.payload);
    });

    // Startup found the stored credential dead (see lib.rs) — the app is
    // signed out, so go back to the sign-in screen rather than showing a
    // feed nothing can refresh.
    unlistenSignedOut = await listen("signed-out", () => {
      handleSignedOut();
    });

    // The tray popover runs in its own webview and can't reach this
    // window's stores directly — it asks by emitting these two events
    // (see TrayPopover.svelte) instead.
    unlistenOpenDetail = await listen<DetailTarget>("open-main-detail", (event) => {
      detailStore.open(event.payload);
      view = "feed";
      void getCurrentWindow().show();
      void getCurrentWindow().setFocus();
    });
    // The menu bar's "Check for updates…" — it shows the window and asks here,
    // so a check started from the menu ends up in the same banner, with the
    // same consent, as the quiet one on launch.
    unlistenCheckUpdates = await listen("check-for-updates", () => {
      void getCurrentWindow().show();
      void getCurrentWindow().setFocus();
      void checkForUpdatesQuietly();
    });
    unlistenFocusMain = await listen("focus-main-window", () => {
      void getCurrentWindow().show();
      void getCurrentWindow().setFocus();
    });

    clockInterval = setInterval(() => (now = Date.now() / 1000), 30_000);

    window.addEventListener("keydown", handleKeydown);

    // Fire-and-forget: never awaited, so a slow or offline check never
    // delays the rest of startup above.
    checkForUpdatesQuietly();
  });

  /** The launch-time half of docs/RELEASING.md's update flow (the other
   * half is Settings.svelte's explicit "Check for updates…", which calls
   * the same `check()` on its own). "Quietly": no dialog and no banner
   * unless an update actually exists — a machine offline and "you're
   * already current" must look identical to the user; a failure is logged
   * (for anyone reading the console) but never surfaced any louder than
   * that. Only finding a real update ever sets `pendingUpdate` below. */
  async function checkForUpdatesQuietly() {
    try {
      const update = await checkForUpdate();
      if (update) pendingUpdate = update;
    } catch (e) {
      console.error("vigie: update check failed (quiet, non-blocking):", e);
    }
  }

  onDestroy(() => {
    unlistenNewEvents?.();
    unlistenRateLimited?.();
    unlistenSignedOut?.();
    unlistenOpenDetail?.();
    unlistenCheckUpdates?.();
    unlistenFocusMain?.();
    if (clockInterval) clearInterval(clockInterval);
    window.removeEventListener("keydown", handleKeydown);
  });

  function handleKeydown(e: KeyboardEvent) {
    const meta = e.metaKey;
    if (meta && e.key === ",") {
      e.preventDefault();
      view = "settings";
    } else if (meta && (e.key === "r" || e.key === "R")) {
      e.preventDefault();
      pollStore.pollNow().catch(() => {});
    } else if (meta && e.key === "Enter") {
      // ⌘Enter toggles reader/split while a detail is open (documented in
      // the pane's Back arrow title) — a no-op with no detail open.
      if ($detailTarget) {
        e.preventDefault();
        if ($detailLayout === "reader") detailStore.exitReader();
        else detailStore.enterReader();
      }
    } else if (e.key === "Escape") {
      if ($detailTarget) {
        // Esc always steps back exactly one level, never two in one press
        // (packet 007, "the Back button and Esc pop one level when the
        // stack has more than one entry, and only close/step out of reader
        // mode when it does not"): a level pushed on top of the one the
        // pane opened to (a commit opened from inside a PR's Commits tab)
        // pops first; only once there's nothing left to pop does Esc fall
        // back to exiting reader mode, then finally closing.
        if ($detailStack.length > 1) detailStore.popOne();
        else if ($detailLayout === "reader") detailStore.exitReader();
        else detailStore.close();
      } else {
        getCurrentWindow()
          .hide()
          .catch(() => {});
      }
    } else if ($detailTarget && !meta && !e.ctrlKey && !e.altKey && !isTypingTarget(e.target) && (e.key === "j" || e.key === "ArrowDown")) {
      // Step through `detailStore.siblings` in either layout (packet 002) —
      // the list's own open-row highlight (Feed.svelte/ThreadBlock.svelte's
      // `selected`) picks up the new target automatically. A no-op with no
      // detail open, or while typing somewhere (a filter box, say).
      e.preventDefault();
      detailStore.stepNext();
    } else if ($detailTarget && !meta && !e.ctrlKey && !e.altKey && !isTypingTarget(e.target) && (e.key === "k" || e.key === "ArrowUp")) {
      e.preventDefault();
      detailStore.stepPrevious();
    }
  }

  /** Whether `target` is somewhere j/k · ↑/↓ should type instead of step —
   * an input, a textarea, or anything `contenteditable`. None of those
   * exist in this app today (this guard is a forward-looking no-op), but a
   * global keydown listener stepping the pane while someone is typing "j"
   * into a text field would be a real bug the moment one is added, so this
   * checks for it now rather than waiting to be asked. */
  function isTypingTarget(target: EventTarget | null): boolean {
    if (!(target instanceof HTMLElement)) return false;
    return target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable;
  }

  /** The divider's fraction of `.content`'s width that a pointer at
   * `clientX` represents — the pane sits to the *right* of the divider, so
   * this is measured from the content area's right edge. */
  function paneFractionFromClientX(clientX: number): number | null {
    const rect = contentEl?.getBoundingClientRect();
    if (!rect || rect.width === 0) return null;
    return (rect.right - clientX) / rect.width;
  }

  function handleDividerPointerDown(e: PointerEvent) {
    e.preventDefault();
    dividerDragging = true;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }

  function handleDividerPointerMove(e: PointerEvent) {
    if (!dividerDragging) return;
    const fraction = paneFractionFromClientX(e.clientX);
    if (fraction == null) return;
    if (fraction > READER_HANDOFF_FRACTION) {
      // Reader mode unmounts the divider itself, so no further pointer
      // events will reach it this drag — stop tracking now rather than
      // leaving `dividerDragging` (and its cursor/no-select styling) stuck.
      dividerDragging = false;
      detailStore.enterReader();
      return;
    }
    detailStore.setPaneWidth(fraction);
  }

  function handleDividerPointerUp(e: PointerEvent) {
    if (!dividerDragging) return;
    dividerDragging = false;
    (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    const nearest = PANE_WIDTH_SNAP_POINTS.find((p) => Math.abs($paneWidth - p) <= PANE_WIDTH_SNAP_TOLERANCE);
    if (nearest !== undefined) detailStore.setPaneWidth(nearest);
  }

  function handleDividerDoubleClick() {
    detailStore.setPaneWidth(PANE_WIDTH_DEFAULT);
  }

  function handleDividerKeydown(e: KeyboardEvent) {
    // Left moves the divider left (pane grows); Right moves it right (pane
    // shrinks) — the divider's own position, not the pane's size, is what
    // the arrow key visually drives.
    if (e.key === "ArrowLeft") {
      e.preventDefault();
      detailStore.setPaneWidth($paneWidth + PANE_WIDTH_ARROW_STEP);
    } else if (e.key === "ArrowRight") {
      e.preventDefault();
      detailStore.setPaneWidth($paneWidth - PANE_WIDTH_ARROW_STEP);
    }
  }

  function navigate(v: ViewName) {
    if (v !== "feed") feedActorFilter = null;
    if (v !== "summary") summaryActorFilter = null;
    view = v;
  }

  function openPerson(login: string) {
    feedActorFilter = login;
    view = "feed";
  }

  function openPersonInSummary(login: string) {
    summaryActorFilter = login;
    view = "summary";
  }

  function finishOnboarding() {
    needsOnboarding = false;
    unseenStore.refresh();
    badgeStore.refresh();
  }

  function handleSignedOut() {
    detailStore.close();
    view = "feed";
    needsOnboarding = true;
  }
</script>

{#if checkingToken}
  <div class="loading-screen">Loading…</div>
{:else if needsOnboarding}
  <Onboarding onDone={finishOnboarding} />
{:else}
  <div class="shell">
    <Sidebar
      active={view}
      badges={$badgeCounts}
      lastPolledAt={$lastPolledAt}
      pollIntervalSecs={$settings.poll_interval_secs}
      {now}
      onNavigate={navigate}
    />
    <div class="main-column">
      <div class="content" class:dragging={dividerDragging} bind:this={contentEl}>
        <main class="main" class:hidden={$detailLayout === "reader" && $detailTarget != null}>
          {#if pendingUpdate}
            <UpdatePrompt update={pendingUpdate} onDismiss={() => (pendingUpdate = null)} />
          {/if}
          {#if $rateLimit}
            <div class="banner" role="status">
              <span class="banner-text">
                GitHub is rate limiting Vigie. {$rateLimit.reset_at
                  ? `Polling resumes at ${clockTime($rateLimit.reset_at)}.`
                  : "Polling resumes on its own."}
              </span>
              <button class="banner-dismiss" onclick={() => pollStore.setRateLimit(null)} aria-label="Dismiss"
                >&times;</button
              >
            </div>
          {/if}
          {#if showScopeFilter}
            <div class="scope-filter-row">
              {#if showModeToggle}
                <div class="scope-mode-toggle" role="group" aria-label="Show activity from">
                  <button
                    class="scope-mode-btn"
                    class:on={scopeEffectiveMode === "team"}
                    aria-pressed={scopeEffectiveMode === "team"}
                    onclick={() => selectScopeMode("team")}
                  >
                    My team
                  </button>
                  <button
                    class="scope-mode-btn"
                    class:on={scopeEffectiveMode === "all"}
                    aria-pressed={scopeEffectiveMode === "all"}
                    onclick={() => selectScopeMode("all")}
                  >
                    Everyone
                  </button>
                </div>
              {/if}
              {#if scopeRepoOptions.length > 1}
                <select
                  class="scope-repo-select"
                  value={$scopeRepoFilter ?? ""}
                  onchange={handleScopeRepoChange}
                  aria-label="Filter by repo"
                >
                  <option value="">All repos</option>
                  {#each scopeRepoOptions as repo (repo.id)}
                    <option value={repo.id}>{repo.owner}/{repo.name}</option>
                  {/each}
                </select>
              {/if}
            </div>
          {/if}
          {#if view === "feed"}
            {#key feedActorFilter}
              <Feed initialActor={feedActorFilter} onGoToTeams={() => (view = "teams")} />
            {/key}
          {:else if view === "prs"}
            <PullRequests />
          {:else if view === "commits"}
            <Commits />
          {:else if view === "comments"}
            <Comments />
          {:else if view === "issues"}
            <Issues />
          {:else if view === "summary"}
            {#key summaryActorFilter}
              <Summary initialActor={summaryActorFilter} onOpenPerson={openPerson} />
            {/key}
          {:else if view === "people"}
            <People onOpenPerson={openPerson} onOpenPersonInSummary={openPersonInSummary} onGoToTeams={() => (view = "teams")} />
          {:else if view === "repos"}
            <Repos />
          {:else if view === "teams"}
            <Teams />
          {:else if view === "watched"}
            <Watched />
          {:else if view === "settings"}
            <Settings onSignedOut={handleSignedOut} />
          {/if}
        </main>
        {#if $detailTarget}
          {#if $detailLayout === "split"}
            <!-- A resizable-pane separator is the WAI-ARIA APG's own documented
                 exception to "role=separator isn't interactive" — svelte-check's
                 a11y linter doesn't special-case it, hence the two ignores. -->
            <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
            <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
            <div
              class="divider"
              role="separator"
              aria-orientation="vertical"
              aria-label="Resize detail pane"
              title="Drag to resize, double-click to reset"
              tabindex="0"
              onpointerdown={handleDividerPointerDown}
              onpointermove={handleDividerPointerMove}
              onpointerup={handleDividerPointerUp}
              onpointercancel={handleDividerPointerUp}
              ondblclick={handleDividerDoubleClick}
              onkeydown={handleDividerKeydown}
            ></div>
          {/if}
          <DetailPane target={$detailTarget} onClose={detailStore.close} />
        {/if}
      </div>
      <StatusBar {now} onNavigate={navigate} />
    </div>
  </div>
{/if}

<style>
  .loading-screen {
    display: flex;
    align-items: center;
    justify-content: center;
    height: 100vh;
    color: var(--muted);
    font-size: 13px;
  }
  .shell {
    display: flex;
    flex-direction: row;
    height: 100vh;
    width: 100vw;
  }
  /* Sits beside the sidebar (a `.shell` row child) so the sidebar keeps its
     own full height; stacks `.content` above StatusBar.svelte's 24px bar so
     the bar spans this column's full width rather than the content area's
     row alone. */
  .main-column {
    flex-grow: 1;
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
    overflow: hidden;
  }
  .content {
    flex-grow: 1;
    display: flex;
    flex-direction: row;
    min-width: 0;
    min-height: 0;
    overflow: hidden;
  }
  /* While the divider (below) is being dragged: suppress text selection
     over the list/pane the pointer crosses, and keep the resize cursor even
     when the pointer strays off the 6px divider itself (pointer capture
     keeps the drag events coming; this just keeps the cursor honest). */
  .content.dragging {
    user-select: none;
  }
  .content.dragging * {
    cursor: col-resize;
  }
  /* The draggable list/pane divider (DetailPane.svelte's old Expand/
     Collapse toggle is gone — this plus a double-click/Enter on a row are
     the new gestures). Visible only while a detail is open in split layout;
     reader mode hides it along with `<main>`. */
  .divider {
    width: 6px;
    flex-shrink: 0;
    cursor: col-resize;
    background: transparent;
    touch-action: none;
  }
  .divider:hover,
  .divider:focus-visible {
    background: var(--accent);
    outline: none;
  }
  .banner {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 16px;
    background: var(--surface-sunken);
    border-bottom: 1px solid var(--border);
    color: var(--text);
    font-size: 12px;
  }
  .banner-text {
    flex-grow: 1;
  }
  .banner-dismiss {
    border: 0;
    background: none;
    color: var(--muted);
    font-size: 16px;
    line-height: 1;
    cursor: pointer;
    padding: 0 2px;
    border-radius: 4px;
    transition: background 0.12s, color 0.12s;
  }
  .banner-dismiss:hover {
    color: var(--text);
    background: var(--surface-active);
  }
  /* The app-wide repo/mode filter row (packet gm-scope-r1) — same visual
     vocabulary as Feed.svelte's own `.filter-row`/`.mode-toggle`/
     `.repo-select` (out of this packet's boundary, so not shared via import;
     duplicated here deliberately, once, rather than eight times per view). */
  .scope-filter-row {
    display: flex;
    flex-direction: row;
    align-items: center;
    flex-wrap: wrap;
    gap: 10px;
    padding: 10px 24px;
    border-bottom: 1px solid var(--divider);
    flex-shrink: 0;
  }
  .scope-mode-toggle {
    display: flex;
    flex-direction: row;
    border: 1px solid var(--border);
    border-radius: 8px;
    overflow: hidden;
  }
  .scope-mode-btn {
    font-size: 12px;
    padding: 4px 10px;
    border: none;
    background: none;
    color: var(--text);
    cursor: pointer;
    font-family: inherit;
    transition: background 0.12s, color 0.12s;
  }
  .scope-mode-btn + .scope-mode-btn {
    border-left: 1px solid var(--border);
  }
  .scope-mode-btn.on {
    background: var(--accent);
    color: var(--surface);
    font-weight: 500;
  }
  .scope-mode-btn:hover:not(.on) {
    background: var(--surface-active);
  }
  .scope-repo-select {
    font-size: 12px;
    padding: 4px 8px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--surface);
    color: var(--text);
    font-family: inherit;
    cursor: pointer;
  }
  .main {
    flex-grow: 1;
    display: flex;
    flex-direction: column;
    min-width: 0;
    overflow: hidden;
  }
  /* Reader mode (detailStore.layout — entered by double-click/Enter on a
     row, the divider dragged past its far edge, or ⌘Enter) hides the list
     this way rather than an `{#if}` — staying mounted keeps Feed's scroll
     position and its local actor filter across a reader/split round trip. */
  .main.hidden {
    display: none;
  }
</style>
