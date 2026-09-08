<script module lang="ts">
  import { INITIAL_BACKFILL_GUARD, type BackfillGuardState } from "../lib/backfill";

  // Persists across a Feed.svelte remount (leaving the view and coming back,
  // or the actor-filter `{#key}` in App.svelte tearing the component down) —
  // module scope, not component `$state`, is what makes "Load older
  // activity" honor a guard for the life of the app session rather than
  // forgetting it every time the view is recreated. See backfill.ts's
  // `BackfillGuardState` doc for why the decision logic itself stays pure
  // and lives there instead of inline here.
  let guardState = $state<BackfillGuardState>(INITIAL_BACKFILL_GUARD);
</script>

<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import {
    reposStore,
    pollStore,
    unseenStore,
    badgeStore,
    teamsStore,
    teamFilterStore,
    settingsStore,
    detailStore,
    watchedOnlyStore,
    sameTarget,
    type DetailTarget,
  } from "../lib/stores";
  import { feedEventStore, repoFilterStore, feedModeStore } from "../lib/feed-filters";
  import { api } from "../lib/api";
  import { scroller } from "../lib/scroller";
  import {
    BACKFILL_STEP_SPAN_SECS,
    backfillBudgetLow,
    backfillExhausted,
    backfillStepLabel,
    classifyBackfillResult,
    describeBackfillOutcome,
    describeLowBudget,
    describeRequestsSpent,
    effectiveBackfillGuard,
    advanceBackfillGuard,
    clearBackfillGuardOnNewEvents,
    requestsSpent,
    NOTHING_OLDER_MESSAGE,
  } from "../lib/backfill";
  import { dayLabel } from "../lib/time";
  import { groupThreads, eventDetailTarget, type EventGroup } from "../lib/threads";
  import { isEngineError, engineErrorMessage, type Event, type PollResult } from "../lib/types";
  import EventRow from "../components/EventRow.svelte";
  import ThreadBlock from "../components/ThreadBlock.svelte";
  import Avatar from "../components/Avatar.svelte";
  import Icon from "../components/Icon.svelte";
  import TeamSwitcher from "../components/TeamSwitcher.svelte";

  let { initialActor = null, onGoToTeams }: { initialActor?: string | null; onGoToTeams?: () => void } = $props();

  let filterActor = $state<string | null>(initialActor);
  let sentinel: HTMLDivElement | undefined = $state();

  // "Load older activity" (packet item 2): a `backfill` step across every
  // repo, run from the end-of-feed sentinel or the empty-state hint below.
  let backfillingOlder = $state(false);
  let olderActivityStatus = $state<string | null>(null);

  const { selected: teamFilter } = teamFilterStore;
  const { selected: repoFilter, select: selectRepo } = repoFilterStore;
  const { mode: feedMode, select: selectFeedMode } = feedModeStore;
  const { settings, loaded: settingsLoaded } = settingsStore;

  /** The "My team / Everyone" control's effective value: the explicit choice
   * once one has been made (here or restored from localStorage), or
   * `Settings.filter_mode` while none has — the same seed
   * `feedModeStore.seedFromSettings` writes in below, read directly here too
   * so the very first render (before that effect has run) already matches
   * what an existing install expects instead of a placeholder default. */
  let effectiveMode = $derived($feedMode ?? $settings.filter_mode);

  function currentFilters() {
    return {
      actor: filterActor,
      teamId: $teamFilter,
      repoId: $repoFilter,
      mode: effectiveMode,
      watchedOnly: $watchedOnlyStore,
    };
  }

  /** Whether the feed is showing a narrower slice than a backfill step
   * fetches: a step always runs across every watched repo, so under any of
   * these the step's count can be real while this list gains nothing. */
  function isFeedFiltered(): boolean {
    return (
      filterActor != null ||
      $teamFilter != null ||
      $repoFilter != null ||
      effectiveMode === "team" ||
      $watchedOnlyStore
    );
  }

  async function reload() {
    await feedEventStore.loadInitial(currentFilters());
  }

  $effect(() => {
    filterActor; // re-run whenever the actor, team, repo, mode or watched-only filter changes
    $teamFilter;
    $repoFilter;
    effectiveMode;
    $watchedOnlyStore;
    reload();
  });

  // Seeds the "My team / Everyone" control from `Settings.filter_mode` once
  // real settings have loaded (packet gm-feed-r1, work item 1): that setting
  // used to gate ingestion and is now this control's starting value instead,
  // so an existing install's feed keeps showing what its owner expects on
  // first launch after the update. Gated on `settingsLoaded` rather than
  // just `$settings` — that store's own default placeholder loads
  // synchronously before the real value comes back, and seeding from it
  // first would lock in "team" even for an install that had chosen "all".
  $effect(() => {
    if ($settingsLoaded) feedModeStore.seedFromSettings($settings.filter_mode);
  });

  $effect(() => {
    if (!sentinel) return;
    const target = sentinel;
    const observer = new IntersectionObserver((entries) => {
      for (const e of entries) {
        if (!e.isIntersecting) continue;
        if ($eventExhausted) {
          // The stored feed has nothing more; reaching further back means a
          // network fetch, and every fetch is a genuine spend across every
          // watched repo. That must always be an explicit click on the
          // button below — reaching this sentinel never fires it on its
          // own, on the first visit or any later one (packet gm-guard-r1,
          // work item 2 — this reverses an earlier packet's "auto-load after
          // the first click" behaviour, which was the wrong call).
        } else {
          feedEventStore.loadMore(currentFilters());
        }
      }
    });
    observer.observe(target);
    return () => observer.disconnect();
  });

  // Live-update while this view is mounted: the Rust-side poll timer emits
  // "new-events" on every non-empty background poll.
  let unlistenNewEvents: (() => void) | undefined;
  onMount(async () => {
    unlistenNewEvents = await listen<PollResult>("new-events", (event) => {
      // New activity means the fleet is no longer the one a "nothing older"
      // verdict was reached about, so a tripped guard offers again — without
      // this, a fortnight of quiet disables "Load older activity" until the
      // app restarts.
      guardState = clearBackfillGuardOnNewEvents(effectiveGuardState, event.payload.new_events.length);
      reload();
    });
    await teamsStore.refresh();
  });
  onDestroy(() => unlistenNewEvents?.());

  const { items: eventItems, loading: eventLoading, exhausted: eventExhausted } = feedEventStore;
  const { items: repoItems } = reposStore;
  const { polling, rateLimitRemaining, lastError: pollError } = pollStore;
  const { items: teams } = teamsStore;
  const { target: detailTarget } = detailStore;

  /** The quick actor-filter strip: the selected team's members, or every
   * team's members (deduped) when "All teams" is selected — there is no
   * single team's roster to fall back to any more. */
  let stripLogins = $derived.by(() => {
    if ($teamFilter != null) {
      return $teams.find((t) => t.id === $teamFilter)?.logins ?? [];
    }
    return [...new Set($teams.flatMap((t) => t.logins))];
  });

  // Empty state (packet item 6): with the "My team" view filter on and no
  // teams at all, nothing can match — so the empty feed gets a hint pointing
  // at the fix instead of reading like a poll problem.
  let noTeamsBlockFeed = $derived(effectiveMode === "team" && $teams.length === 0);

  /** True only for a genuinely empty feed — no actor/team/repo/mode/watched
   * filter narrowing it — as opposed to a filter that happens to match
   * nothing. Backs the empty-state honesty fix (packet item 4): a filtered
   * "no results" reading is still correct as the plain "No activity yet…"
   * message below, but an unfiltered empty feed with repos already added is
   * most often just `FIRST_POLL_LOOKBACK_SECS` (24h) not having reached far
   * enough back yet, which deserves its own explanation and the same
   * "Load older activity" action as the end of a populated feed. */
  let looksEmptyNotFiltered = $derived(!noTeamsBlockFeed && $repoItems.length > 0 && !isFeedFiltered());

  /** The repo selector (work item 3): hidden entirely with 0 or 1 repo
   * configured, `owner/name` per option, "All repos" as the default. */
  let repoOptions = $derived(
    [...$repoItems].sort((a, b) => `${a.owner}/${a.name}`.localeCompare(`${b.owner}/${b.name}`)),
  );

  // `e` is left untyped (matching this codebase's other `currentTarget`
  // sites) rather than annotated `Event`, which here would resolve to the
  // domain type imported above, not the DOM one.
  function handleRepoFilterChange(e: { currentTarget: HTMLSelectElement }) {
    const value = e.currentTarget.value;
    selectRepo(value === "" ? null : Number(value));
  }

  interface FeedRow {
    sortKey: number;
    thread: EventGroup | null; // null for a standalone row
    single: Event | null;
  }

  let feedRows = $derived.by(() => {
    const { threads, standalone } = groupThreads($eventItems);
    const rows: FeedRow[] = [
      ...threads.map((t) => ({ sortKey: t.sortKey, thread: t, single: null })),
      ...standalone.map((e) => ({ sortKey: e.occurred_at, thread: null, single: e })),
    ];
    rows.sort((a, b) => b.sortKey - a.sortKey);
    return rows;
  });

  let groups = $derived.by(() => {
    const map = new Map<string, FeedRow[]>();
    for (const row of feedRows) {
      const label = dayLabel(row.sortKey);
      if (!map.has(label)) map.set(label, []);
      map.get(label)!.push(row);
    }
    return [...map.entries()];
  });

  /** The reader-mode previous/next stepper's list (packet 002): every
   * openable row in `feedRows`' own order — same order the day-grouped
   * `groups` above render in, since grouping only partitions an
   * already-sorted array, never reorders it. A thread collapses to its one
   * target (its child events aren't separate stops — see EventRow's
   * `siblings` doc). */
  let feedSiblings = $derived(
    feedRows
      .map((row): DetailTarget | null =>
        row.thread ? { kind: "thread", repoId: row.thread.repoId, number: row.thread.number } : eventDetailTarget(row.single!),
      )
      .filter((t): t is DetailTarget => t != null),
  );

  function repoLabelFor(repoId: number): string | undefined {
    const r = $repoItems.find((r) => r.id === repoId);
    return r ? `${r.owner}/${r.name}` : undefined;
  }

  async function handleSeen(id: number) {
    feedEventStore.markSeenLocally([id]);
    try {
      await api.markSeen([id]);
      await Promise.all([unseenStore.refresh(), badgeStore.refresh()]);
    } catch {
      // non-fatal: seen state is best-effort
    }
  }

  function toggleActorFilter(login: string) {
    filterActor = filterActor === login ? null : login;
  }

  async function doPoll() {
    try {
      await pollStore.pollNow((r) => {
        guardState = clearBackfillGuardOnNewEvents(effectiveGuardState, r.new_events.length);
        if (r.new_events.length > 0) reload();
      });
    } catch {
      // pollStore.lastError already carries the message; surfaced in the template
    }
  }

  /** The fleet a step would hit right now, and the guard reconciled against
   * it — see `effectiveBackfillGuard`'s doc for why watching a different set
   * of repos than the persisted streak always clears it, even before the
   * next step runs. */
  let currentRepoIds = $derived($repoItems.map((r) => r.id));
  let effectiveGuardState = $derived(effectiveBackfillGuard(guardState, currentRepoIds));

  /** The three honest reasons "Load older activity" stops offering another
   * step (packet gm-guard-r1, work items 3-5): the budget check wins when
   * both apply, since it's the one that's actually about to run out. */
  let olderActivityBudgetLow = $derived(backfillBudgetLow($rateLimitRemaining));
  let olderActivityNothingOlder = $derived(backfillExhausted(effectiveGuardState));
  let olderActivityDisabled = $derived(
    backfillingOlder || olderActivityBudgetLow || olderActivityNothingOlder,
  );

  /** What the control says right now — the cost it's about to spend, or
   * which of the two guards is holding it back. */
  let olderActivityLabel = $derived(
    olderActivityBudgetLow && $rateLimitRemaining != null
      ? describeLowBudget($rateLimitRemaining)
      : olderActivityNothingOlder
        ? NOTHING_OLDER_MESSAGE
        : backfillStepLabel(currentRepoIds.length),
  );

  /** One `backfill` step across every repo — the deliberate action behind
   * "Load older activity". Every step is an explicit click: nothing in this
   * view ever calls it on its own (packet gm-guard-r1, work item 2), and the
   * three guards above (in-flight, budget, exhausted) keep the button itself
   * from offering one when it shouldn't. Reloads the current page afterward
   * so any newly-stored older events actually show up. */
  async function loadOlderActivity() {
    if (olderActivityDisabled) return;
    backfillingOlder = true;
    olderActivityStatus = null;
    const repoIdsForStep = currentRepoIds;
    const budgetBefore = $rateLimitRemaining;
    try {
      const result = await api.backfill(null, BACKFILL_STEP_SPAN_SECS);
      if (result.rate_limit_remaining != null) pollStore.setRateLimitRemaining(result.rate_limit_remaining);
      const outcome = classifyBackfillResult(result);
      // A step always runs fleet-wide; say so when the feed is filtered, so a
      // real count never reads as a lie against a list that gained nothing.
      let status = describeBackfillOutcome(outcome, isFeedFiltered());
      // "Show what it cost" (work item 6): alongside the outcome, when both
      // readings are known.
      const spent = requestsSpent(budgetBefore, result.rate_limit_remaining);
      if (spent != null) status += ` ${describeRequestsSpent(spent)}`;
      olderActivityStatus = status;
      guardState = advanceBackfillGuard(effectiveGuardState, outcome, repoIdsForStep);
      await reload();
    } catch (e) {
      olderActivityStatus = isEngineError(e) ? engineErrorMessage(e) : "Couldn't fetch older activity.";
    } finally {
      backfillingOlder = false;
    }
  }
</script>

<div class="head">
  <h1>Feed</h1>
  {#if filterActor}
    <span class="muted">{filterActor} · {$eventItems.length} events</span>
    <button class="btn-text" onclick={() => (filterActor = null)}>Clear filter</button>
  {:else}
    <span class="muted">{$eventItems.filter((e) => !e.seen).length} new</span>
  {/if}
  <span class="spacer"></span>
  {#if $rateLimitRemaining != null}
    <span class="muted">{$rateLimitRemaining.toLocaleString()} requests left</span>
  {/if}
  <button class="tbtn" onclick={doPoll} disabled={$polling}>
    {#if $polling}<span class="spin"><Icon name="spinner" size={13} /></span>{/if}
    {$polling ? "Polling…" : "Poll now"}
  </button>
  {#if $pollError}
    <span class="error-text">{$pollError}</span>
  {/if}
</div>

<TeamSwitcher showWatchedChip />

<div class="filter-row">
  <div class="mode-toggle" role="group" aria-label="Show activity from">
    <button
      class="mode-btn"
      class:on={effectiveMode === "team"}
      aria-pressed={effectiveMode === "team"}
      onclick={() => selectFeedMode("team")}
    >
      My team
    </button>
    <button
      class="mode-btn"
      class:on={effectiveMode === "all"}
      aria-pressed={effectiveMode === "all"}
      onclick={() => selectFeedMode("all")}
    >
      Everyone
    </button>
  </div>
  {#if repoOptions.length > 1}
    <select
      class="repo-select"
      value={$repoFilter ?? ""}
      onchange={handleRepoFilterChange}
      aria-label="Filter by repo"
    >
      <option value="">All repos</option>
      {#each repoOptions as repo (repo.id)}
        <option value={repo.id}>{repo.owner}/{repo.name}</option>
      {/each}
    </select>
  {/if}
  <!-- Shown only to installs that actually lost history: the old setting was
       an INGESTION filter, so anyone who ran on "My team only" has gaps
       nothing can conjure back. A fresh install never lost anything, and
       permanent explanatory furniture in the filter row is exactly what got
       the status-bar dots and the activity-mix bar removed — so this is
       gated on the stored setting, and only while "Everyone" is selected,
       which is when the gap is visible. -->
  {#if $settings.filter_mode === "team" && effectiveMode === "all"}
    <span class="muted filter-hint">
      Activity from outside your team wasn't stored before this version and can't come
      back on its own;
      <button
        class="btn-text hint-link"
        onclick={loadOlderActivity}
        disabled={olderActivityDisabled}
        title={olderActivityLabel}
      >
        Load older activity
      </button>
      can fill in some of the past.
    </span>
  {/if}
</div>

<div class="people-strip">
  <span class="avatars">
    {#each stripLogins as login (login)}
      <button class="avatar-btn" class:selected={filterActor === login} onclick={() => toggleActorFilter(login)}>
        <Avatar {login} size={20} />
        <span class="login">{login}</span>
      </button>
    {/each}
  </span>
  <span class="muted strip-hint">click a person to filter · click again to clear</span>
</div>

<div class="body" use:scroller>
  {#if $eventItems.length === 0 && !$eventLoading}
    <div class="zero-state">
      {#if noTeamsBlockFeed}
        <p>
          "Show activity from" is set to team-only in Settings, but there are no teams yet — nothing can match.
          <button class="btn-text hint-link" onclick={() => onGoToTeams?.()}>Go to Teams</button> to add one.
        </p>
      {:else if looksEmptyNotFiltered}
        <p>
          {$polling
            ? "Checking GitHub…"
            : "No activity yet — a new repo only starts from the last 24 hours until it backfills further back."}
        </p>
        <button class="btn-primary" onclick={loadOlderActivity} disabled={olderActivityDisabled}>
          {#if backfillingOlder}<span class="spin"><Icon name="spinner" size={13} /></span>{/if}
          {backfillingOlder ? "Fetching the last 7 days…" : olderActivityLabel}
        </button>
        {#if olderActivityStatus}<p class="muted">{olderActivityStatus}</p>{/if}
      {:else}
        <p>{$polling ? "Checking GitHub…" : `No activity yet${filterActor ? ` from ${filterActor}` : ""}.`}</p>
        <button class="btn-primary" onclick={doPoll} disabled={$polling}>
          {#if $polling}<span class="spin"><Icon name="spinner" size={13} /></span>{/if}
          {$polling ? "Polling…" : "Poll now"}
        </button>
      {/if}
    </div>
  {:else}
    {#each groups as [label, rows] (label)}
      <div class="day-label">{label}</div>
      <div class="day-group">
        {#each rows as row (row.thread ? `t${row.thread.repoId}:${row.thread.number}` : `s${row.single!.id}`)}
          {#if row.thread}
            {@const t = row.thread}
            <ThreadBlock
              thread={t}
              repoLabel={repoLabelFor(t.repoId)}
              onOpen={(repoId, number) => detailStore.openThread(repoId, number, { origin: "Feed", siblings: feedSiblings })}
              onSeen={handleSeen}
              onFilterActor={toggleActorFilter}
            />
          {:else}
            {@const e = row.single!}
            <EventRow
              event={e}
              repoLabel={repoLabelFor(e.repo_id)}
              selected={sameTarget($detailTarget, eventDetailTarget(e))}
              origin="Feed"
              siblings={feedSiblings}
              onSeen={handleSeen}
              onFilterActor={toggleActorFilter}
            />
          {/if}
        {/each}
      </div>
    {/each}
    <div bind:this={sentinel} class="sentinel">
      {#if $eventLoading}
        <span class="muted">Loading…</span>
      {:else if $eventExhausted}
        {#if backfillingOlder}
          <span class="muted">Fetching the last 7 days…</span>
        {:else}
          <button class="btn-text" onclick={loadOlderActivity} disabled={olderActivityDisabled}>
            {olderActivityLabel}
          </button>
          {#if olderActivityStatus}<span class="muted">{olderActivityStatus}</span>{/if}
        {/if}
      {/if}
    </div>
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
    display: flex;
    align-items: center;
    gap: 6px;
    transition: background 0.12s;
  }
  .tbtn:hover:not(:disabled) {
    background: var(--surface-active);
  }
  .tbtn:active:not(:disabled) {
    background: var(--surface-sunken);
  }
  .tbtn:disabled {
    opacity: 0.7;
    cursor: default;
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
  .filter-row {
    display: flex;
    flex-direction: row;
    align-items: center;
    flex-wrap: wrap;
    gap: 10px;
    padding: 0 24px 10px 24px;
  }
  .mode-toggle {
    display: flex;
    flex-direction: row;
    border: 1px solid var(--border);
    border-radius: 8px;
    overflow: hidden;
  }
  .mode-btn {
    font-size: 12px;
    padding: 4px 10px;
    border: none;
    background: none;
    color: var(--text);
    cursor: pointer;
    font-family: inherit;
    transition: background 0.12s, color 0.12s;
  }
  .mode-btn + .mode-btn {
    border-left: 1px solid var(--border);
  }
  .mode-btn.on {
    background: var(--accent);
    color: var(--surface);
    font-weight: 500;
  }
  .mode-btn:hover:not(.on) {
    background: var(--surface-active);
  }
  .repo-select {
    font-size: 12px;
    padding: 4px 8px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--surface);
    color: var(--text);
    font-family: inherit;
    cursor: pointer;
  }
  .filter-hint {
    flex-basis: 100%;
    font-size: 11px;
    line-height: 1.4;
  }
  .filter-hint .hint-link {
    font-size: 11px;
    padding: 0 2px;
  }
  .people-strip {
    display: flex;
    flex-direction: row;
    align-items: center;
    flex-wrap: wrap;
    gap: 8px;
    padding: 0 24px 12px 24px;
  }
  .avatars {
    display: flex;
    flex-direction: row;
    flex-wrap: wrap;
    gap: 6px;
  }
  /* A pill chip (avatar + login), not a bare avatar — an initial alone
     doesn't say who's who. Unselected chips stay muted (dim text, no
     ring); the selected chip keeps the app's existing accent-ring treatment
     and brightens its text to read as "on". */
  .avatar-btn {
    background: var(--surface-active);
    border: none;
    padding: 3px 10px 3px 4px;
    border-radius: 999px;
    cursor: pointer;
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 6px;
    box-shadow: none;
    transition: filter 0.12s, box-shadow 0.12s;
  }
  .avatar-btn:hover {
    filter: brightness(0.94);
  }
  .avatar-btn.selected {
    box-shadow: 0 0 0 2px var(--surface), 0 0 0 4px var(--accent);
  }
  .avatar-btn .login {
    font-size: 12px;
    font-weight: 500;
    color: var(--muted);
    max-width: 110px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .avatar-btn.selected .login {
    color: var(--text);
    font-weight: 600;
  }
  .strip-hint {
    padding-left: 6px;
  }
  .body {
    padding: 0 24px 24px 24px;
    overflow-y: auto;
    flex-grow: 1;
    /* Establishes the container-query context EventRow's and ThreadBlock's
       `.repo` columns measure against (this packet's responsive columns) —
       their own `@container (max-width: 640px)` rules live in those
       components' own scoped styles, not here. */
    container-type: inline-size;
  }
  .day-label {
    padding: 14px 0 2px 0;
    font-weight: 500;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    font-size: 11px;
    color: var(--muted);
  }
  .day-group {
    display: flex;
    flex-direction: column;
  }
  .sentinel {
    padding: 16px 0;
    text-align: center;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 6px;
  }
  .zero-state {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 10px;
    padding: 40px 0;
  }
  .hint-link {
    padding: 0 2px;
  }
</style>
