<script lang="ts">
  // Comments (docs/design/Comments.dc.html): reviews, PR comments and issue
  // comments as full-text cards.
  import { onMount, onDestroy } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { openUrl } from "@tauri-apps/plugin-opener";
  import { api } from "../lib/api";
  import { scroller } from "../lib/scroller";
  import {
    reposStore,
    teamFilterStore,
    teamsStore,
    settingsStore,
    detailStore,
    watchedOnlyStore,
    type DetailTarget,
  } from "../lib/stores";
  import { repoFilterStore, feedModeStore, effectiveFilterMode, scopeEmptyMessage } from "../lib/feed-filters";
  import { dayLabel, shortRelativeTime } from "../lib/time";
  import { COMMENT_KINDS, commentRefLabel, eventDetailTarget } from "../lib/threads";
  import type { Event, PollResult } from "../lib/types";
  import Avatar from "../components/Avatar.svelte";
  import Icon from "../components/Icon.svelte";
  import KindBadge from "../components/KindBadge.svelte";
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
      events = (
        await api.listEvents({
          limit: 500,
          teamId: $teamFilter,
          repoId: $repoFilter,
          mode: effectiveMode,
          watchedOnly: $watchedOnlyStore,
        })
      ).filter((e) => COMMENT_KINDS.includes(e.kind));
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

  /** The reader-mode stepper's list (packet 002): `events` in the exact
   * order rendered — grouping into `groups` below only buckets by day
   * label, it never reorders (an already-sorted `events` never interleaves
   * two days, so the flattened day-groups match `events`' own order). */
  let commentSiblings = $derived(events.map(eventDetailTarget).filter((t): t is DetailTarget => t != null));

  function open(e: Event) {
    const target = eventDetailTarget(e);
    if (target) detailStore.open(target, { origin: "Comments", siblings: commentSiblings });
  }

  /** Sibling button, never nested inside `.card` (see markup below) —
   * `stopPropagation` is belt and suspenders, not load-bearing. */
  function openOnGitHub(e: MouseEvent, url: string) {
    e.stopPropagation();
    openUrl(url).catch(() => {
      // best-effort — same as DetailPane's openOnGitHub
    });
  }

  let groups = $derived.by(() => {
    const map = new Map<string, Event[]>();
    for (const e of events) {
      const label = dayLabel(e.occurred_at);
      if (!map.has(label)) map.set(label, []);
      map.get(label)!.push(e);
    }
    return [...map.entries()];
  });

  let newCount = $derived(events.filter((e) => !e.seen).length);
</script>

<div class="head">
  <h1>Comments</h1>
  {#if !loading}<span class="muted">{newCount} new</span>{/if}
  <span class="spacer"></span>
  <span class="muted">reviews, PR and issue comments</span>
</div>

<TeamSwitcher showWatchedChip />

<div class="body" use:scroller>
  {#if loading}
    <p class="muted">Loading…</p>
  {:else if events.length === 0}
    <div class="zero-state">
      <p class="muted">
        {scopeEmptyMessage("comments", { repoLabel: filterRepoLabel, teamLabel: filterTeamLabel, mode: effectiveMode }) ??
          "No comments yet."}
      </p>
    </div>
  {:else}
    {#each groups as [label, dayEvents] (label)}
      <div class="day-label">{label}</div>
      {#each dayEvents as e (e.id)}
        <div class="card-wrap">
          <button class="card" class:unseen={!e.seen} onclick={() => open(e)}>
            <div class="row">
              <span class="dot" class:on={!e.seen}></span>
              <Avatar login={e.actor_login} avatarUrl={e.actor_avatar_url} size={20} />
              <span class="login">{e.actor_login}</span>
              <KindBadge kind={e.kind} />
              <span class="muted ref">{commentRefLabel(e)}</span>
              <span class="spacer"></span>
              <span class="muted">{repoLabel(e.repo_id)} · {shortRelativeTime(e.occurred_at)}</span>
            </div>
            {#if e.body_preview}
              <div class="quote">{e.body_preview}</div>
            {/if}
          </button>
          <button class="ext-link" onclick={(ev) => openOnGitHub(ev, e.url)} title="Open on GitHub">
            <Icon name="external" size={13} />
          </button>
        </div>
      {/each}
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
  .body {
    padding: 0 24px 24px 24px;
    overflow-y: auto;
    flex-grow: 1;
  }
  .day-label {
    padding: 14px 0 2px 0;
    font-weight: 500;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    font-size: 11px;
    color: var(--muted);
  }
  .card-wrap {
    position: relative;
  }
  .card {
    display: flex;
    flex-direction: column;
    gap: 6px;
    width: 100%;
    padding: 10px 28px 10px 0;
    border-bottom: 1px solid var(--divider);
    background: none;
    border-left: none;
    border-right: none;
    border-top: none;
    text-align: left;
    cursor: pointer;
    font-family: inherit;
    color: inherit;
    transition: background 0.12s;
  }
    /* The hover fill is a highlight, so it wears the app's row radius — the
     same 6px EventRow uses. Without it the fill has square corners while
     every other highlighted row in the app is rounded. */
  .card-wrap:hover .card {
    background: var(--surface-active);
    border-radius: 6px;
  }
  /* Sibling of `.card`, never nested inside it, muted until the row is
     hovered/focused — see PullRequests.svelte's identical recipe. */
  .ext-link {
    position: absolute;
    top: 14px;
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
  .card-wrap:hover .ext-link,
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
  .login {
    font-size: 13px;
    font-weight: 500;
  }
  .ref {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .card.unseen .ref {
    font-weight: 600;
    color: var(--text);
  }
  .quote {
    font-size: 13px;
    color: var(--muted-strong);
    line-height: 1.4;
    border-left: 2px solid var(--divider);
    padding: 2px 0 2px 10px;
    margin-left: 16px;
  }
  .zero-state {
    padding: 40px 0;
  }
</style>
