<script lang="ts">
  // One card per person, grouped by team in display order, then "Not in a
  // team" for actors seen in events who belong to none (docs/CONTRACT.md
  // work item 4). A person in two teams appears under both — each group is
  // computed independently from `team.logins`, so there's no dedup step to
  // accidentally drop a second appearance.
  import { onMount } from "svelte";
  import { api } from "../lib/api";
  import { scroller } from "../lib/scroller";
  import { reposStore, teamsStore, settingsStore } from "../lib/stores";
  import { repoFilterStore, feedModeStore, effectiveFilterMode } from "../lib/feed-filters";
  import { relativeTime } from "../lib/time";
  import type { Event, EventKind, Team } from "../lib/types";
  import Avatar from "../components/Avatar.svelte";
  import KindBadge from "../components/KindBadge.svelte";

  let {
    onOpenPerson,
    onOpenPersonInSummary,
    onGoToTeams,
  }: {
    onOpenPerson: (login: string) => void;
    /** Row-menu action: opens Summary with this person preselected as `actor`. */
    onOpenPersonInSummary: (login: string) => void;
    onGoToTeams: () => void;
  } = $props();

  /** Which card's row menu is open, or none. */
  let openMenuFor = $state<string | null>(null);
  function toggleMenu(login: string) {
    openMenuFor = openMenuFor === login ? null : login;
  }
  function closeMenu(e: FocusEvent) {
    const wrap = e.currentTarget as HTMLElement;
    if (!wrap.contains(e.relatedTarget as Node | null)) openMenuFor = null;
  }

  const { items: teams } = teamsStore;
  const { items: repoItems } = reposStore;
  const { selected: repoFilter } = repoFilterStore;
  const { mode: feedMode } = feedModeStore;
  const { settings } = settingsStore;

  /** The app-wide "My team / Everyone" control's effective value — see
   * Commits.svelte's identical comment. */
  let effectiveMode = $derived(effectiveFilterMode($feedMode, $settings.filter_mode));

  interface PersonStats {
    login: string;
    avatarUrl: string | null;
    counts: Partial<Record<EventKind, number>>;
    lastActive: number | null;
  }

  interface PersonGroup {
    key: string;
    label: string;
    people: PersonStats[];
  }

  let groups = $state<PersonGroup[]>([]);
  let loading = $state(true);
  let loadError = $state<string | null>(null);

  const SEVEN_DAYS = 7 * 24 * 60 * 60;

  async function statsFor(login: string): Promise<PersonStats> {
    const events: Event[] = await api.listEvents({
      actor: login,
      repoId: $repoFilter,
      mode: effectiveMode,
      limit: 500,
    });
    const counts: Partial<Record<EventKind, number>> = {};
    let lastActive: number | null = null;
    let avatarUrl: string | null = null;
    const cutoff = Date.now() / 1000 - SEVEN_DAYS;
    for (const e of events) {
      if (lastActive === null || e.occurred_at > lastActive) lastActive = e.occurred_at;
      if (!avatarUrl && e.actor_avatar_url) avatarUrl = e.actor_avatar_url;
      if (e.occurred_at >= cutoff) counts[e.kind] = (counts[e.kind] ?? 0) + 1;
    }
    return { login, avatarUrl, counts, lastActive };
  }

  async function load() {
    loading = true;
    loadError = null;
    try {
      const currentTeams: Team[] = $teams;

      // Every actor seen anywhere, deliberately IGNORING the "My team /
      // Everyone" mode: finding people who are not in a team is this scan's
      // whole purpose, and scoping it to team members would make the "Not in
      // a team" bucket structurally always empty. The repo filter still
      // applies — narrowing to one repo is a question about that repo's
      // people, which is a sensible thing to ask.
      const allEvents: Event[] = await api.listEvents({ repoId: $repoFilter, mode: "all", limit: 500 });
      const seenLogins = [...new Set(allEvents.map((e) => e.actor_login))];
      const inATeam = new Set(currentTeams.flatMap((t) => t.logins));
      const unassigned = seenLogins.filter((l) => !inATeam.has(l.toLowerCase()) && !inATeam.has(l));

      const built: PersonGroup[] = [];
      for (const team of currentTeams) {
        const people = await Promise.all(team.logins.map(statsFor));
        built.push({ key: `team-${team.id}`, label: team.name, people });
      }
      if (unassigned.length > 0) {
        const people = await Promise.all(unassigned.map(statsFor));
        built.push({ key: "unassigned", label: "Not in a team", people });
      }
      groups = built;
    } catch (e) {
      loadError = e instanceof Error ? e.message : "Couldn't load people.";
    } finally {
      loading = false;
    }
  }

  onMount(() => {
    teamsStore.refresh();
  });

  // Reloads whenever the team roster, the app-wide repo filter, or the
  // app-wide mode changes — including the very first load, replacing the
  // old explicit `onMount(async () => { await teamsStore.refresh(); await
  // load(); })` sequencing (packet gm-scope-r1: "pick a repo in Commits,
  // switch to People, it is still applied", mirroring the other views' own
  // `$effect`). `$teams` has to be a tracked read here — unlike those other
  // views, `load` also depends on the team roster, and `teamsStore.refresh()`
  // above resolves asynchronously after this effect's first run, so without
  // this it would build every group from whatever (possibly empty) roster
  // happened to be in the store at that first instant and never revisit it.
  $effect(() => {
    $teams;
    $repoFilter;
    effectiveMode;
    load();
  });

  function topKinds(counts: Partial<Record<EventKind, number>>): [EventKind, number][] {
    return (Object.entries(counts) as [EventKind, number][]).sort((a, b) => b[1] - a[1]).slice(0, 4);
  }

  let totalCount = $derived(groups.reduce((n, g) => n + g.people.length, 0));

  /** The selected repo's own label, for the "nothing silently empty"
   * message below. */
  let filterRepoLabel = $derived.by(() => {
    const r = $repoFilter != null ? $repoItems.find((r) => r.id === $repoFilter) : null;
    return r ? `${r.owner}/${r.name}` : null;
  });
</script>

<div class="head">
  <h1>People</h1>
  {#if !loading}
    <span class="muted">{totalCount} total</span>
  {/if}
</div>

<div class="body" use:scroller>
  {#if loading}
    <p class="muted">Loading…</p>
  {:else if loadError}
    <p class="error-text">{loadError}</p>
  {:else if totalCount === 0}
    <div class="zero-state">
      <p>Nobody to show yet — add a team, or wait for the next poll to see who's active.</p>
      <button class="btn-primary" onclick={onGoToTeams}>Go to Teams</button>
    </div>
  {:else}
    {#each groups as group (group.key)}
      <div class="group-label">{group.label} · {group.people.length}</div>
      <div class="grid">
        {#each group.people as p (p.login)}
          <div class="card">
            <button class="card-main" onclick={() => onOpenPerson(p.login)}>
              <div class="card-head">
                <Avatar login={p.login} avatarUrl={p.avatarUrl} size={36} />
                <div class="card-id">
                  <span class="login">{p.login}</span>
                  <span class="muted">
                    {p.lastActive ? `active ${relativeTime(p.lastActive)}` : "no activity yet"}
                  </span>
                </div>
              </div>
              <div class="kinds">
                {#each topKinds(p.counts) as [kind, count] (kind)}
                  <div class="kind-row">
                    <KindBadge {kind} />
                    <span class="muted">{count} · 7d</span>
                  </div>
                {:else}
                  <span class="muted">No activity{filterRepoLabel ? ` in ${filterRepoLabel}` : ""} in the last 7 days</span>
                {/each}
              </div>
            </button>
            <div class="card-menu" onfocusout={closeMenu}>
              <button
                class="menu-btn"
                onclick={() => toggleMenu(p.login)}
                aria-label="More actions for {p.login}"
                aria-expanded={openMenuFor === p.login}
              >
                ⋯
              </button>
              {#if openMenuFor === p.login}
                <div class="menu">
                  <button
                    class="menu-item"
                    onclick={() => {
                      onOpenPerson(p.login);
                      openMenuFor = null;
                    }}
                  >
                    Open feed
                  </button>
                  <button
                    class="menu-item"
                    onclick={() => {
                      onOpenPersonInSummary(p.login);
                      openMenuFor = null;
                    }}
                  >
                    Open summary
                  </button>
                </div>
              {/if}
            </div>
          </div>
        {/each}
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
  .body {
    padding: 0 24px 24px 24px;
    overflow-y: auto;
    flex-grow: 1;
  }
  .group-label {
    padding: 14px 0 8px 0;
    font-weight: 500;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    font-size: 11px;
    color: var(--muted);
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(220px, 1fr));
    gap: 12px;
  }
  .card {
    position: relative;
    border: 1px solid var(--border-card);
    border-radius: 8px;
  }
  .card:hover {
    border-color: var(--accent);
  }
  .card-main {
    display: flex;
    flex-direction: column;
    gap: 10px;
    text-align: left;
    background: none;
    border: none;
    border-radius: 8px;
    width: 100%;
    padding: 14px;
    cursor: pointer;
    font-family: inherit;
    color: inherit;
  }
  .card-menu {
    position: absolute;
    top: 6px;
    right: 6px;
  }
  .menu-btn {
    background: none;
    border: none;
    color: var(--muted);
    font-size: 14px;
    line-height: 1;
    padding: 3px 6px;
    border-radius: 6px;
    cursor: pointer;
    font-family: inherit;
    transition: background 0.12s, color 0.12s;
  }
  .menu-btn:hover {
    background: var(--surface-active);
    color: var(--text);
  }
  .menu {
    position: absolute;
    top: 100%;
    right: 0;
    z-index: 1;
    display: flex;
    flex-direction: column;
    min-width: 130px;
    padding: 4px;
    background: var(--surface-card);
    border: 1px solid var(--border-card);
    border-radius: 8px;
    box-shadow: 0 4px 16px rgb(0 0 0 / 0.12);
  }
  .menu-item {
    text-align: left;
    background: none;
    border: none;
    font-size: 12px;
    color: var(--text);
    padding: 6px 8px;
    border-radius: 5px;
    cursor: pointer;
    font-family: inherit;
    white-space: nowrap;
  }
  .menu-item:hover {
    background: var(--surface-active);
  }
  .card-head {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .card-id {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .login {
    font-size: 13px;
    font-weight: 600;
  }
  .kinds {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .kind-row {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .zero-state {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 10px;
    padding: 40px 0;
  }
</style>
