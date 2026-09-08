<script lang="ts">
  // Pull requests (docs/design/PRs.dc.html): the same event store as Feed,
  // filtered to pr_* kinds and grouped one row per PR number.
  import { onMount, onDestroy } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { openUrl } from "@tauri-apps/plugin-opener";
  import { api } from "../lib/api";
  import { scroller } from "../lib/scroller";
  import {
    reposStore,
    settingsStore,
    teamFilterStore,
    teamsStore,
    detailStore,
    watchedOnlyStore,
    type DetailTarget,
  } from "../lib/stores";
  import { repoFilterStore, feedModeStore, effectiveFilterMode, scopeEmptyMessage } from "../lib/feed-filters";
  import { summarizePrThreads, type PrThreadSummary } from "../lib/threads";
  import { relativeTime } from "../lib/time";
  import type { Event, PollResult } from "../lib/types";
  import Avatar from "../components/Avatar.svelte";
  import Icon from "../components/Icon.svelte";
  import TeamSwitcher from "../components/TeamSwitcher.svelte";

  let events = $state<Event[]>([]);
  let loading = $state(true);

  const { selected: teamFilter } = teamFilterStore;
  const { items: teams } = teamsStore;
  const { selected: repoFilter } = repoFilterStore;
  const { mode: feedMode } = feedModeStore;
  const { settings } = settingsStore;

  /** The app-wide "My team / Everyone" control's effective value — see
   * Commits.svelte's identical comment. */
  let effectiveMode = $derived(effectiveFilterMode($feedMode, $settings.filter_mode));

  async function load() {
    loading = true;
    try {
      events = await api.listEvents({
        limit: 500,
        teamId: $teamFilter,
        repoId: $repoFilter,
        mode: effectiveMode,
        watchedOnly: $watchedOnlyStore,
      });
    } finally {
      loading = false;
    }
  }

  let unlisten: (() => void) | undefined;
  onMount(async () => {
    unlisten = await listen<PollResult>("new-events", () => load());
  });
  onDestroy(() => unlisten?.());

  // Loads on mount and again whenever the team, repo, mode or watched-only
  // filter changes.
  $effect(() => {
    $teamFilter;
    $repoFilter;
    effectiveMode;
    $watchedOnlyStore;
    load();
  });

  const { items: repoItems } = reposStore;

  function repoLabel(repoId: number): string {
    const r = $repoItems.find((r) => r.id === repoId);
    return r ? `${r.owner}/${r.name}` : "";
  }

  /** The app-wide repo/team filters' own labels, for the "nothing silently
   * empty" message below. */
  let filterRepoLabel = $derived.by(() => {
    const r = $repoFilter != null ? $repoItems.find((r) => r.id === $repoFilter) : null;
    return r ? `${r.owner}/${r.name}` : null;
  });
  let filterTeamLabel = $derived.by(() => {
    const t = $teamFilter != null ? $teams.find((t) => t.id === $teamFilter) : null;
    return t ? t.name : null;
  });

  /** `null` when the repo isn't known yet (e.g. a stale `repoId` between a
   * repo removal and the next reload) — callers hide the button rather than
   * link to a guessed URL. */
  function prUrl(pr: PrThreadSummary): string | null {
    const r = $repoItems.find((r) => r.id === pr.repoId);
    return r ? `https://github.com/${r.owner}/${r.name}/pull/${pr.number}` : null;
  }

  /** The row's own click already opens in-app; this is the row's *only*
   * other button (a sibling, never nested inside the row button — see the
   * markup below), so `stopPropagation` here is a belt-and-suspenders
   * guard against a future restructure re-nesting it, not a fix for a
   * bubbling bug that exists today (verified live: it doesn't). */
  function openOnGitHub(e: MouseEvent, url: string) {
    e.stopPropagation();
    openUrl(url).catch(() => {
      // best-effort — same as DetailPane's openOnGitHub
    });
  }

  const STATE_LABEL: Record<PrThreadSummary["state"], string> = {
    open: "Open",
    changes_requested: "Changes requested",
    merged: "Merged",
    closed: "Closed",
  };
  const STATE_CLASS: Record<PrThreadSummary["state"], string> = {
    open: "st-open",
    changes_requested: "st-review",
    merged: "st-merged",
    closed: "st-closed",
  };

  const WEEK = 7 * 24 * 60 * 60;

  let summaries = $derived(summarizePrThreads(events));
  let open = $derived(summaries.filter((s) => s.state === "open" || s.state === "changes_requested").sort((a, b) => b.lastActivityAt - a.lastActivityAt));
  let mergedThisWeek = $derived(
    summaries.filter((s) => s.state === "merged" && Date.now() / 1000 - s.lastActivityAt < WEEK).sort((a, b) => b.lastActivityAt - a.lastActivityAt),
  );
  let closed = $derived(summaries.filter((s) => s.state === "closed").sort((a, b) => b.lastActivityAt - a.lastActivityAt));
  let repoCount = $derived(new Set(summaries.map((s) => s.repoId)).size);

  /** The reader-mode stepper's list (packet 002): every row in the order the
   * three sections render top to bottom — Open, then Merged this week, then
   * Closed. */
  let prSiblings = $derived(
    [...open, ...mergedThisWeek, ...closed].map((pr): DetailTarget => ({ kind: "thread", repoId: pr.repoId, number: pr.number })),
  );
</script>

<div class="head">
  <h1>Pull requests</h1>
  {#if !loading}
    <span class="muted">{open.length} open · {mergedThisWeek.length} merged this week</span>
  {/if}
  <span class="spacer"></span>
  <span class="muted">by {effectiveMode === "team" ? "your team" : "everyone"}, across {repoCount} repos</span>
</div>

<TeamSwitcher showWatchedChip />

<div class="body" use:scroller>
  {#if loading}
    <p class="muted">Loading…</p>
  {:else if summaries.length === 0}
    <div class="zero-state">
      <p class="muted">
        {scopeEmptyMessage("pull request activity", {
          repoLabel: filterRepoLabel,
          teamLabel: filterTeamLabel,
          mode: effectiveMode,
        }) ?? "No pull request activity yet."}
      </p>
    </div>
  {:else}
    {#if open.length > 0}
      <div class="section-label">Open · {open.length}</div>
      {#each open as pr (`${pr.repoId}:${pr.number}`)}
        <div class="pr-row-wrap">
          <button class="pr-row" onclick={() => detailStore.openThread(pr.repoId, pr.number, { origin: "Pull requests", siblings: prSiblings })}>
            <div class="row title-row">
              <span class="dot"></span>
              <span class="state-badge {STATE_CLASS[pr.state]}">{STATE_LABEL[pr.state]}</span>
              <span class="pr-title">{pr.title}</span>
              <span class="muted">#{pr.number} · {repoLabel(pr.repoId)}</span>
            </div>
            <div class="row meta-row">
              <Avatar login={pr.openedBy} size={18} />
              <span class="muted">{pr.openedBy} · opened {relativeTime(pr.openedAt)}</span>
              <span class="muted">·</span>
              <span class="muted">
                {pr.state === "changes_requested" ? `changes requested by ${pr.changesRequestedBy}` : pr.approvals > 0 ? `${pr.approvals} approval${pr.approvals === 1 ? "" : "s"}` : "no reviews yet"}
              </span>
              <span class="muted">·</span>
              <span class="muted">{pr.comments} comment{pr.comments === 1 ? "" : "s"}</span>
            </div>
          </button>
          {#if prUrl(pr)}
            <button class="ext-link" onclick={(e) => openOnGitHub(e, prUrl(pr)!)} title="Open on GitHub">
              <Icon name="external" size={13} />
            </button>
          {/if}
        </div>
      {/each}
    {/if}

    {#if mergedThisWeek.length > 0}
      <div class="section-label">Merged this week · {mergedThisWeek.length}</div>
      {#each mergedThisWeek as pr (`${pr.repoId}:${pr.number}`)}
        <div class="pr-row-wrap">
          <button class="pr-row" onclick={() => detailStore.openThread(pr.repoId, pr.number, { origin: "Pull requests", siblings: prSiblings })}>
            <div class="row title-row">
              <span class="dot"></span>
              <span class="state-badge {STATE_CLASS[pr.state]}">{STATE_LABEL[pr.state]}</span>
              <span class="pr-title">{pr.title}</span>
              <span class="muted">#{pr.number} · {repoLabel(pr.repoId)}</span>
            </div>
            <div class="row meta-row">
              <Avatar login={pr.openedBy} size={18} />
              <span class="muted">{pr.openedBy} · merged {relativeTime(pr.lastActivityAt)}</span>
              <span class="muted">·</span>
              <span class="muted">{pr.approvals} approval{pr.approvals === 1 ? "" : "s"}</span>
              <span class="muted">·</span>
              <span class="muted">{pr.comments} comment{pr.comments === 1 ? "" : "s"}</span>
            </div>
          </button>
          {#if prUrl(pr)}
            <button class="ext-link" onclick={(e) => openOnGitHub(e, prUrl(pr)!)} title="Open on GitHub">
              <Icon name="external" size={13} />
            </button>
          {/if}
        </div>
      {/each}
    {/if}

    {#if closed.length > 0}
      <div class="section-label">Closed · {closed.length}</div>
      {#each closed as pr (`${pr.repoId}:${pr.number}`)}
        <div class="pr-row-wrap">
          <button class="pr-row" onclick={() => detailStore.openThread(pr.repoId, pr.number, { origin: "Pull requests", siblings: prSiblings })}>
            <div class="row title-row">
              <span class="dot"></span>
              <span class="state-badge {STATE_CLASS[pr.state]}">{STATE_LABEL[pr.state]}</span>
              <span class="pr-title">{pr.title}</span>
              <span class="muted">#{pr.number} · {repoLabel(pr.repoId)}</span>
            </div>
            <div class="row meta-row">
              <Avatar login={pr.openedBy} size={18} />
              <span class="muted">{pr.openedBy} · closed {relativeTime(pr.lastActivityAt)}</span>
            </div>
          </button>
          {#if prUrl(pr)}
            <button class="ext-link" onclick={(e) => openOnGitHub(e, prUrl(pr)!)} title="Open on GitHub">
              <Icon name="external" size={13} />
            </button>
          {/if}
        </div>
      {/each}
    {/if}
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
  .body {
    padding: 0 24px 24px 24px;
    overflow-y: auto;
    flex-grow: 1;
  }
  .section-label {
    padding: 14px 0 2px 0;
    font-weight: 500;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    font-size: 11px;
    color: var(--muted);
  }
  /* Positions `.ext-link` over the row's own right edge (this packet, "the
     small ⧉ button at the right edge") as a real sibling button, never
     nested inside `.pr-row` — so a click on it can never also fire the
     row's own handler; no `stopPropagation` load-bearing here, just belt
     and suspenders (see `openOnGitHub`'s comment). */
  .pr-row-wrap {
    position: relative;
  }
  .pr-row {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 12px 28px 12px 0;
    border-bottom: 1px solid var(--divider);
    background: none;
    border-left: none;
    border-right: none;
    border-top: none;
    text-align: left;
    cursor: pointer;
    font-family: inherit;
    color: inherit;
    width: 100%;
    transition: background 0.12s;
  }
    /* The hover fill is a highlight, so it wears the app's row radius — the
     same 6px EventRow uses. Without it the fill has square corners while
     every other highlighted row in the app is rounded. */
  .pr-row-wrap:hover .pr-row {
    background: var(--surface-active);
    border-radius: 6px;
  }
  /* Muted until the row (or the button itself) is hovered/focused — the
     in-app open stays the obvious default action; this is clearly
     secondary (Watched.svelte's `.unwatch` uses the same reveal-on-hover
     recipe). */
  .ext-link {
    position: absolute;
    top: 16px;
    right: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    padding: 0;
    background: none;
    border: none;
    border-radius: 5px;
    color: var(--muted);
    cursor: pointer;
    opacity: 0;
    transition: opacity 0.12s, background 0.12s, color 0.12s;
  }
  .pr-row-wrap:hover .ext-link,
  .ext-link:focus-visible {
    opacity: 1;
  }
  .ext-link:hover {
    background: var(--surface-sunken);
    color: var(--text);
  }
  .row {
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 10px;
  }
  .title-row {
    gap: 10px;
  }
  .dot {
    width: 6px;
    height: 6px;
    border-radius: 3px;
    background: var(--accent);
    flex-shrink: 0;
  }
  .pr-title {
    font-size: 13px;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex-grow: 1;
  }
  .state-badge {
    font-size: 11px;
    padding: 2px 7px;
    border-radius: 4px;
    white-space: nowrap;
    font-weight: 500;
  }
  .st-open {
    background: var(--badge-open-bg);
    color: var(--badge-open-fg);
  }
  .st-review {
    background: var(--badge-review-bg);
    color: var(--badge-review-fg);
  }
  .st-merged {
    background: var(--badge-merged-bg);
    color: var(--badge-merged-fg);
  }
  .st-closed {
    background: var(--badge-closed-bg);
    color: var(--badge-closed-fg);
  }
  .meta-row {
    padding-left: 16px;
    gap: 8px;
  }
  .zero-state {
    padding: 40px 0;
  }
</style>
