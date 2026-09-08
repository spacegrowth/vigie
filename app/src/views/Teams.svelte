<script lang="ts">
  // Teams (docs/design/Teams.dc.html): a list of teams on the left, each
  // expandable to its members on the right. Every control saves on the spot
  // — no Save buttons, matching the mockup and Team.svelte's prior behavior.
  import { onMount } from "svelte";
  import { teamsStore, reposStore } from "../lib/stores";
  import { api } from "../lib/api";
  import { scroller } from "../lib/scroller";
  import { isEngineError, engineErrorMessage, type PersonSuggestion, type Team } from "../lib/types";
  import Avatar from "../components/Avatar.svelte";

  const { items: teams } = teamsStore;
  const { items: repoItems } = reposStore;

  let selectedId = $state<number | null>(null);
  let selected = $derived($teams.find((t) => t.id === selectedId) ?? null);

  // Keep a selection alive: pick the first team once any exist, and follow
  // along if the selected one is deleted out from under us.
  $effect(() => {
    if (selectedId != null && $teams.some((t) => t.id === selectedId)) return;
    selectedId = $teams[0]?.id ?? null;
  });

  onMount(async () => {
    await Promise.all([teamsStore.refresh(), reposStore.refresh()]);
  });

  // --- rename (inline, saves on blur) ---
  let nameDraft = $state("");
  let nameSynced = $state<number | null>(null); // which team's id nameDraft currently reflects
  $effect(() => {
    if (selected && nameSynced !== selected.id) {
      nameDraft = selected.name;
      nameSynced = selected.id;
    }
  });

  async function renameSelected() {
    if (!selected) return;
    const name = nameDraft.trim() || selected.name;
    nameDraft = name;
    if (name === selected.name) return;
    try {
      await teamsStore.update({ ...selected, name });
    } catch {
      nameDraft = selected.name; // revert the field to what actually saved
    }
  }

  // --- members ---
  async function removeMember(login: string) {
    if (!selected) return;
    await teamsStore.update({ ...selected, logins: selected.logins.filter((l) => l !== login) });
    searchPeople(query);
  }

  let query = $state("");
  let results = $state<PersonSuggestion[]>([]);
  let searching = $state(false);
  let showResults = $state(false);

  async function searchPeople(q: string) {
    if (!selected) return;
    searching = true;
    try {
      results = await api.suggestPeople(q, 8, selected.id);
    } catch {
      results = [];
    } finally {
      searching = false;
    }
  }

  $effect(() => {
    selected; // re-search when the selected team changes
    searchPeople("");
  });

  async function addMember(login: string) {
    if (!selected) return;
    const v = login.trim().toLowerCase().replace(/^@/, "");
    if (!v || selected.logins.includes(v)) return;
    await teamsStore.update({ ...selected, logins: [...selected.logins, v] });
    query = "";
    showResults = false;
    searchPeople("");
  }

  let visibleResults = $derived(results.filter((r) => !(selected?.logins.includes(r.login) ?? false)));
  let orgs = $derived([...new Set($repoItems.map((r) => r.owner))]);
  let searchCaption = $derived(
    `Contributors of your ${$repoItems.length} repo${$repoItems.length === 1 ? "" : "s"}${orgs.length ? ` and members of ${orgs.join(", ")}` : ""}`,
  );

  // --- reorder (arrow buttons) ---
  function moveTeam(index: number, dir: -1 | 1) {
    const ids = $teams.map((t) => t.id);
    const j = index + dir;
    if (j < 0 || j >= ids.length) return;
    [ids[index], ids[j]] = [ids[j], ids[index]];
    teamsStore.reorder(ids);
  }

  // --- new team / delete ---
  let creating = $state(false);
  async function newTeam() {
    creating = true;
    try {
      const team = await teamsStore.create("New team", []);
      selectedId = team.id;
    } finally {
      creating = false;
    }
  }

  let confirmDeleteId = $state<number | null>(null);
  let deleting = $state(false);
  async function deleteTeam(teamId: number) {
    deleting = true;
    try {
      await teamsStore.remove(teamId);
    } finally {
      deleting = false;
      confirmDeleteId = null;
    }
  }

  // --- import from org (creates a new team, per the packet) ---
  let orgInput = $state("");
  let teamSlugInput = $state("");
  let importing = $state(false);
  let importError = $state<string | null>(null);
  let importPreview = $state<Team | null>(null);

  async function previewImport() {
    if (!orgInput.trim() || !teamSlugInput.trim()) return;
    importing = true;
    importError = null;
    importPreview = null;
    try {
      importPreview = await api.importOrgTeam(orgInput.trim(), teamSlugInput.trim());
    } catch (e) {
      importError = isEngineError(e) ? engineErrorMessage(e) : "Couldn't import that team.";
    } finally {
      importing = false;
    }
  }

  async function applyImport() {
    if (!importPreview) return;
    const created = await teamsStore.create(importPreview.name, importPreview.logins);
    selectedId = created.id;
    importPreview = null;
    orgInput = "";
    teamSlugInput = "";
  }
</script>

<div class="head">
  <h1>Teams</h1>
  <span class="muted">{$teams.length} team{$teams.length === 1 ? "" : "s"}</span>
</div>

{#if $teams.length === 0}
  <div class="zero-state">
    <p class="muted">No teams yet.</p>
    <button class="btn-primary" onclick={newTeam} disabled={creating}>{creating ? "Creating…" : "New team"}</button>
  </div>
{:else}
  <div class="split">
    <div class="list" use:scroller>
      {#each $teams as team, i (team.id)}
        <div class="team-row" class:on={team.id === selectedId}>
          <button class="team-row-main" onclick={() => (selectedId = team.id)}>
            <span class="dots" aria-hidden="true">
              {#each Array(6) as _}<span class="d"></span>{/each}
            </span>
            <span class="team-id">
              <span class="team-name">{team.name}</span>
              <span class="muted">{team.logins.length} people</span>
            </span>
            <span class="avatars">
              {#each team.logins.slice(0, 3) as login, j (login)}
                <span class="avatar-stack" style="z-index:{3 - j}"><Avatar {login} size={20} /></span>
              {/each}
            </span>
          </button>
          <span class="reorder">
            <button class="arrow" disabled={i === 0} onclick={() => moveTeam(i, -1)} aria-label="Move {team.name} up">▲</button>
            <button class="arrow" disabled={i === $teams.length - 1} onclick={() => moveTeam(i, 1)} aria-label="Move {team.name} down">▼</button>
          </span>
        </div>
      {/each}
      <div class="spacer-grow"></div>
      <button class="new-team-row" onclick={newTeam} disabled={creating}>
        <span class="plus">+</span>
        <span>{creating ? "Creating…" : "New team"}</span>
      </button>
    </div>

    {#if selected}
      {@const team = selected}
      <div class="detail" use:scroller>
        <div class="detail-head">
          <input
            class="name-input"
            bind:value={nameDraft}
            placeholder="Team name"
            onblur={renameSelected}
            onkeydown={(e) => e.key === "Enter" && (e.currentTarget as HTMLInputElement).blur()}
          />
          <span class="spacer"></span>
          {#if confirmDeleteId === team.id}
            <span class="muted">Delete "{team.name}"?</span>
            <button class="btn-text danger" onclick={() => deleteTeam(team.id)} disabled={deleting}>
              {deleting ? "Deleting…" : "Confirm"}
            </button>
            <button class="btn-text" onclick={() => (confirmDeleteId = null)}>Cancel</button>
          {:else}
            <button class="tbtn danger" onclick={() => (confirmDeleteId = team.id)}>Delete</button>
          {/if}
        </div>

        <section class="field">
          <span class="label">People</span>
          <div class="chips">
            {#each team.logins as l (l)}
              <span class="chip">
                <Avatar login={l} size={20} />
                {l}
                <button class="chip-x" onclick={() => removeMember(l)} aria-label="Remove {l}">×</button>
              </span>
            {:else}
              <span class="muted">No one yet.</span>
            {/each}
          </div>
        </section>

        <section
          class="field search-field"
          onfocusout={(e) => {
            const wrap = e.currentTarget as HTMLElement;
            if (!wrap.contains(e.relatedTarget as Node | null)) showResults = false;
          }}
        >
          <span class="label">Add people</span>
          <input
            placeholder="octocat"
            bind:value={query}
            onfocus={() => (showResults = true)}
            oninput={() => {
              showResults = true;
              searchPeople(query);
            }}
            onkeydown={(e) => e.key === "Enter" && addMember(query)}
          />
          {#if showResults}
            <div class="dropdown">
              <div class="caption">{searchCaption}</div>
              {#each visibleResults as r (r.login)}
                <button class="result-row" onclick={() => addMember(r.login)}>
                  <Avatar login={r.login} avatarUrl={r.avatar_url} size={22} />
                  <span class="login">{r.login}</span>
                  <span class="muted">{r.why}</span>
                  <span class="spacer"></span>
                  <span class="muted">{r.source === "bot" ? "bot" : "contributor"}</span>
                </button>
              {:else}
                {#if !searching}<div class="empty muted">No matches.</div>{/if}
              {/each}
            </div>
          {/if}
          <span class="muted hint">Searches the people who commit, review or comment in the repos you watch, plus the org's members. Enter adds.</span>
        </section>

        <div class="divider"></div>

        <section class="field import-row">
          <div class="row">
            <span class="label">Or import an org team</span>
            <input placeholder="acme" bind:value={orgInput} class="narrow" />
            <input placeholder="platform" bind:value={teamSlugInput} class="narrow" />
            <button class="btn-text" onclick={previewImport} disabled={importing || !orgInput.trim() || !teamSlugInput.trim()}>
              {importing ? "Loading…" : "Preview members"}
            </button>
          </div>
          {#if importError}<p class="error-text">{importError}</p>{/if}
          {#if importPreview}
            <div class="preview-box">
              <p class="muted">{importPreview.logins.length} members in {importPreview.name}:</p>
              <div class="chips">
                {#each importPreview.logins as l (l)}<span class="chip plain">{l}</span>{/each}
              </div>
              <div class="row">
                <button class="btn-primary" onclick={applyImport}>Create team from these {importPreview.logins.length} members</button>
                <button class="btn-text" onclick={() => (importPreview = null)}>Discard</button>
              </div>
            </div>
          {/if}
        </section>
      </div>
    {/if}
  </div>
{/if}

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
  .zero-state {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 10px;
    padding: 12px 24px 24px 24px;
  }
  .split {
    display: flex;
    flex-direction: row;
    flex-grow: 1;
    min-height: 0;
  }
  .list {
    width: 250px;
    flex-shrink: 0;
    border-right: 1px solid var(--border);
    padding: 8px 12px 16px 12px;
    display: flex;
    flex-direction: column;
    gap: 4px;
    box-sizing: border-box;
    overflow-y: auto;
  }
  .team-row {
    display: flex;
    flex-direction: row;
    align-items: center;
    border-radius: 8px;
  }
  .team-row.on {
    background: var(--surface-active);
  }
  .team-row-main {
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 10px;
    padding: 9px 6px 9px 10px;
    background: none;
    border: none;
    cursor: pointer;
    font-family: inherit;
    color: inherit;
    text-align: left;
    flex-grow: 1;
    min-width: 0;
  }
  .dots {
    display: grid;
    grid-template-columns: repeat(2, 1fr);
    gap: 2px;
    width: 14px;
    height: 14px;
    flex-shrink: 0;
  }
  .d {
    width: 3px;
    height: 3px;
    border-radius: 2px;
    background: var(--placeholder);
  }
  .team-id {
    display: flex;
    flex-direction: column;
    gap: 1px;
    flex-grow: 1;
    min-width: 0;
  }
  .team-name {
    font-size: 13px;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .avatars {
    display: flex;
    flex-direction: row;
    flex-shrink: 0;
  }
  .avatar-stack {
    display: flex;
    border-radius: 10px;
    box-shadow: 0 0 0 2px var(--surface-sunken);
    margin-left: -6px;
  }
  .avatar-stack:first-child {
    margin-left: 0;
  }
  .reorder {
    display: flex;
    flex-direction: column;
    flex-shrink: 0;
    padding-right: 4px;
  }
  .arrow {
    background: none;
    border: none;
    color: var(--muted);
    cursor: pointer;
    font-size: 8px;
    line-height: 1.4;
    padding: 0 2px;
  }
  .arrow:disabled {
    opacity: 0.3;
    cursor: default;
  }
  .arrow:hover:not(:disabled) {
    color: var(--text);
  }
  .spacer-grow {
    flex-grow: 1;
  }
  .new-team-row {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 9px 10px;
    background: none;
    border: none;
    cursor: pointer;
    font-family: inherit;
    font-size: 13px;
    font-weight: 500;
    color: var(--accent);
    text-align: left;
    border-radius: 8px;
  }
  .new-team-row:hover:not(:disabled) {
    background: var(--surface-active);
  }
  .plus {
    font-size: 15px;
    line-height: 1;
  }
  .detail {
    flex-grow: 1;
    padding: 16px 24px 24px 24px;
    display: flex;
    flex-direction: column;
    gap: 18px;
    max-width: 560px;
    overflow-y: auto;
    box-sizing: border-box;
  }
  .detail-head {
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 10px;
  }
  .name-input {
    font-size: 15px;
    font-weight: 600;
    border: 1px solid transparent;
    background: none;
    padding: 4px 6px;
    border-radius: 6px;
    max-width: 280px;
  }
  .name-input:hover,
  .name-input:focus {
    border-color: var(--input-border);
    background: var(--surface);
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
  }
  .tbtn:hover {
    background: var(--surface-active);
  }
  .tbtn.danger {
    color: var(--danger);
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .label {
    font-size: 12px;
    color: var(--label);
  }
  .row {
    display: flex;
    gap: 8px;
    align-items: center;
  }
  .narrow {
    width: 140px;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }
  .chip {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 4px 6px 4px 4px;
    border: 1px solid var(--border);
    border-radius: 14px;
    font-size: 13px;
  }
  .chip.plain {
    padding: 3px 8px;
  }
  .chip-x {
    background: none;
    border: none;
    color: var(--placeholder);
    cursor: pointer;
    font-size: 13px;
    line-height: 1;
    padding: 0 0 0 2px;
  }
  .search-field {
    position: relative;
  }
  .dropdown {
    width: 420px;
    max-width: 100%;
    border: 1px solid var(--border);
    border-radius: 8px;
    box-shadow: 0 6px 20px rgba(0, 0, 0, 0.08);
    overflow: hidden;
    display: flex;
    flex-direction: column;
    background: var(--surface-card);
  }
  .caption {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--muted);
    padding: 8px 10px 4px 10px;
  }
  .result-row {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 7px 10px;
    background: none;
    border: none;
    cursor: pointer;
    font-family: inherit;
    color: inherit;
    text-align: left;
    width: 100%;
  }
  .result-row:hover {
    background: var(--surface-sunken);
  }
  .login {
    font-size: 13px;
    font-weight: 500;
  }
  .empty {
    padding: 10px;
  }
  .hint {
    max-width: 420px;
  }
  .divider {
    height: 1px;
    background: var(--divider);
    margin: 4px 0;
  }
  .preview-box {
    background: var(--surface-sunken);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 12px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin-top: 6px;
  }
</style>
