<script lang="ts">
  // The team switcher row (docs/design/Main.dc.html): "All teams" then one
  // chip per team in display order. One app-wide, localStorage-backed
  // selection (lib/stores.ts teamFilterStore) shared by every Activity view
  // and the tray popover. With no teams at all, only "All teams" shows —
  // the empty state falls out of mapping over an empty list, nothing extra
  // to special-case here.
  import { onMount } from "svelte";
  import { teamsStore, teamFilterStore, watchedOnlyStore } from "../lib/stores";
  import Icon from "./Icon.svelte";

  // The "Watched" chip (docs/CONTRACT.md "Watched threads"): a quick filter
  // alongside the team chips, not part of the same exclusive selection — a
  // team filter and "only watched" combine, both passed straight through to
  // `list_events`. Only Feed, Pull requests and Comments show it.
  let { showWatchedChip = false }: { showWatchedChip?: boolean } = $props();

  const { items: teams } = teamsStore;
  const { selected } = teamFilterStore;

  onMount(() => {
    teamsStore.refresh();
  });
</script>

<div class="switcher">
  <button class="chip" class:on={$selected == null} onclick={() => teamFilterStore.select(null)}>
    All teams
  </button>
  {#each $teams as team (team.id)}
    <button class="chip" class:on={$selected === team.id} onclick={() => teamFilterStore.select(team.id)}>
      {team.name}
    </button>
  {/each}
  {#if showWatchedChip}
    <button
      class="chip watched-chip"
      class:on={$watchedOnlyStore}
      onclick={() => watchedOnlyStore.update((v) => !v)}
      aria-pressed={$watchedOnlyStore}
    >
      <Icon name="eye" size={12} />
      Watched
    </button>
  {/if}
</div>

<style>
  .switcher {
    display: flex;
    flex-direction: row;
    flex-wrap: wrap;
    gap: 8px;
    padding: 0 24px 10px 24px;
  }
  .chip {
    font-size: 12px;
    padding: 4px 12px;
    border-radius: 12px;
    border: 1px solid var(--border);
    background: none;
    color: var(--text);
    cursor: pointer;
    font-family: inherit;
    transition: background 0.12s, border-color 0.12s;
  }
  .chip:hover:not(.on) {
    background: var(--surface-active);
  }
  .chip.on {
    background: var(--accent);
    border-color: var(--accent);
    color: var(--surface);
    font-weight: 500;
  }
  .watched-chip {
    display: flex;
    align-items: center;
    gap: 4px;
  }
</style>
