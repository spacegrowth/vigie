<script lang="ts">
  // Commits (docs/design/Commits.dc.html): `commit` kind events, grouped by
  // day then repo.
  import { onMount, onDestroy } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { openUrl } from "@tauri-apps/plugin-opener";
  import { api } from "../lib/api";
  import { scroller } from "../lib/scroller";
  import { reposStore, teamFilterStore, teamsStore, settingsStore, detailStore } from "../lib/stores";
  import { repoFilterStore, feedModeStore, effectiveFilterMode, scopeEmptyMessage } from "../lib/feed-filters";
  import { dayLabel, shortRelativeTime } from "../lib/time";
  import { eventDetailTarget } from "../lib/threads";
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

  /** The app-wide "My team / Everyone" control's effective value — same
   * fallback Feed.svelte itself uses (packet gm-scope-r1: every other view
   * now reads the same two stores it does). */
  let effectiveMode = $derived(effectiveFilterMode($feedMode, $settings.filter_mode));

  async function load() {
    loading = true;
    try {
      events = (
        await api.listEvents({ limit: 500, teamId: $teamFilter, repoId: $repoFilter, mode: effectiveMode })
      ).filter((e) => e.kind === "commit");
    } finally {
      loading = false;
    }
  }

  let unlisten: (() => void) | undefined;
  onMount(async () => {
    unlisten = await listen<PollResult>("new-events", () => load());
  });
  onDestroy(() => unlisten?.());

  // Loads on mount and again whenever the team, repo or mode filter changes.
  $effect(() => {
    $teamFilter;
    $repoFilter;
    effectiveMode;
    load();
  });

  const { items: repoItems } = reposStore;

  function repo(repoId: number) {
    return $repoItems.find((r) => r.id === repoId);
  }

  /** The app-wide repo/team filters' own labels, for the "nothing silently
   * empty" message below — `null` when that filter isn't narrowing anything. */
  let filterRepoLabel = $derived.by(() => {
    const r = $repoFilter != null ? $repoItems.find((r) => r.id === $repoFilter) : null;
    return r ? `${r.owner}/${r.name}` : null;
  });
  let filterTeamLabel = $derived.by(() => {
    const t = $teamFilter != null ? $teams.find((t) => t.id === $teamFilter) : null;
    return t ? t.name : null;
  });

  function shaOf(event: Event): string {
    return (event.url.split("/").pop() ?? "").slice(0, 7);
  }

  function openCommit(event: Event) {
    const target = eventDetailTarget(event);
    if (target) detailStore.open(target);
  }

  /** Sibling button, never nested inside `.commit-row` (see markup below) —
   * `stopPropagation` is belt and suspenders, not load-bearing. */
  function openOnGitHub(e: MouseEvent, url: string) {
    e.stopPropagation();
    openUrl(url).catch(() => {
      // best-effort — same as DetailPane's openOnGitHub
    });
  }

  // Day -> repo_id -> events, both ordered newest first.
  let groups = $derived.by(() => {
    const byDay = new Map<string, Map<number, Event[]>>();
    for (const e of events) {
      const day = dayLabel(e.occurred_at);
      if (!byDay.has(day)) byDay.set(day, new Map());
      const byRepo = byDay.get(day)!;
      if (!byRepo.has(e.repo_id)) byRepo.set(e.repo_id, []);
      byRepo.get(e.repo_id)!.push(e);
    }
    return [...byDay.entries()].map(([day, byRepo]) => ({
      day,
      repos: [...byRepo.entries()].sort((a, b) => {
        const aMax = Math.max(...a[1].map((e) => e.occurred_at));
        const bMax = Math.max(...b[1].map((e) => e.occurred_at));
        return bMax - aMax;
      }),
    }));
  });

  let newToday = $derived(events.filter((e) => !e.seen).length);
  let todayCount = $derived(events.filter((e) => dayLabel(e.occurred_at) === "Today").length);
</script>

<div class="head">
  <h1>Commits</h1>
  {#if !loading}
    <span class="muted">{newToday} new · {todayCount} today</span>
  {/if}
  <span class="spacer"></span>
  <span class="muted">default branches only</span>
</div>

<TeamSwitcher />

<div class="body" use:scroller>
  {#if loading}
    <p class="muted">Loading…</p>
  {:else if events.length === 0}
    <div class="zero-state">
      <p class="muted">
        {scopeEmptyMessage("commits", { repoLabel: filterRepoLabel, teamLabel: filterTeamLabel, mode: effectiveMode }) ??
          "No commits yet."}
      </p>
    </div>
  {:else}
    {#each groups as { day, repos } (day)}
      <div class="day-label">{day}</div>
      {#each repos as [repoId, repoEvents] (repoId)}
        <div class="repo-group">
          <div class="row repo-head">
            <span class="repo-name">{repo(repoId)?.owner}/{repo(repoId)?.name}</span>
            <span class="muted">{repo(repoId)?.default_branch}</span>
          </div>
          {#each repoEvents as e (e.id)}
            <div class="commit-row-wrap">
              <button class="row commit-row" class:unseen={!e.seen} onclick={() => openCommit(e)}>
                <span class="dot" class:on={!e.seen}></span>
                <Avatar login={e.actor_login} avatarUrl={e.actor_avatar_url} size={20} />
                <span class="login">{e.actor_login}</span>
                <span class="sha mono">{shaOf(e)}</span>
                <span class="msg">{e.title}</span>
                <span class="muted time">{shortRelativeTime(e.occurred_at)}</span>
              </button>
              <button class="ext-link" onclick={(ev) => openOnGitHub(ev, e.url)} title="Open on GitHub">
                <Icon name="external" size={13} />
              </button>
            </div>
          {/each}
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
  .repo-group {
    padding-bottom: 6px;
    border-bottom: 1px solid var(--divider);
  }
  .row {
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 10px;
  }
  .repo-head {
    padding: 12px 0 4px 0;
    gap: 8px;
  }
  .repo-name {
    font-size: 13px;
    font-weight: 600;
  }
  .commit-row-wrap {
    position: relative;
  }
  .commit-row {
    width: 100%;
    padding: 8px 28px 8px 0;
    background: none;
    border: none;
    cursor: pointer;
    font-family: inherit;
    color: inherit;
    text-align: left;
    transition: background 0.12s;
  }
    /* The hover fill is a highlight, so it wears the app's row radius — the
     same 6px EventRow uses. Without it the fill has square corners while
     every other highlighted row in the app is rounded. */
  .commit-row-wrap:hover .commit-row {
    background: var(--surface-active);
    border-radius: 6px;
  }
  /* Sibling of `.commit-row`, never nested inside it, muted until the row
     is hovered/focused — see PullRequests.svelte's identical recipe. */
  .ext-link {
    position: absolute;
    top: 50%;
    right: 0;
    transform: translateY(-50%);
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
  .commit-row-wrap:hover .ext-link,
  .ext-link:focus-visible {
    opacity: 1;
  }
  .ext-link:hover {
    background: var(--surface-sunken);
    color: var(--text);
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
    width: 60px;
    flex-shrink: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .sha {
    font-size: 11px;
    color: var(--muted);
    width: 56px;
    flex-shrink: 0;
  }
  .msg {
    font-size: 13px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex-grow: 1;
  }
  .commit-row.unseen .msg {
    font-weight: 600;
  }
  .time {
    width: 44px;
    text-align: right;
    flex-shrink: 0;
  }
  .mono {
    font-family: ui-monospace, Menlo, monospace;
  }
  .zero-state {
    padding: 40px 0;
  }
</style>
