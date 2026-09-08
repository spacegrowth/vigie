<script lang="ts">
  // Onboarding (docs/design/Onboarding.dc.html). Sign-in is GitHub's OAuth
  // device flow (docs/CONTRACT.md "Sign-in") — the only way in; there is
  // no token field anywhere in the app. The device-flow UI itself lives in
  // DeviceSignIn.svelte, shared with Settings' "Add account".
  import { api } from "../lib/api";
  import { isEngineError, engineErrorMessage, type PersonSuggestion, type RepoSuggestion } from "../lib/types";
  import { reposStore, teamsStore } from "../lib/stores";
  import DeviceSignIn from "../components/DeviceSignIn.svelte";

  let { onDone }: { onDone: () => void } = $props();

  // --- GitHub account (device flow) ---
  let signedInLogin = $state<string | null>(null);

  function handleSignedIn(login: string) {
    signedInLogin = login;
    loadRepoSuggestions();
  }

  // --- Repos ---
  interface RepoRow extends RepoSuggestion {
    selected: boolean;
  }
  let repoRows = $state<RepoRow[]>([]);
  let repoPaste = $state("");
  let peopleLoaded = false;

  async function loadRepoSuggestions() {
    try {
      const list = await api.suggestRepos();
      repoRows = list.map((s) => ({ ...s, selected: true }));
    } catch {
      repoRows = [];
    }
    if (!peopleLoaded) {
      peopleLoaded = true;
      loadPeopleSuggestions("");
    }
  }

  function toggleRepo(i: number) {
    repoRows[i] = { ...repoRows[i], selected: !repoRows[i].selected };
  }

  /** Minimal "owner/name or a github.com URL" split for the paste field —
   * the same shapes `add_repo` itself accepts; actual validation happens
   * server-side when "Start watching" adds the repo. */
  function parseRepoSpec(spec: string): { owner: string; name: string } | null {
    const s = spec.trim().replace(/\/$/, "");
    const after = s.replace(/^https?:\/\/(www\.)?github\.com\//, "").replace(/^github\.com\//, "");
    const parts = after.replace(/\.git$/, "").split("/");
    if (parts.length < 2 || !parts[0] || !parts[1]) return null;
    return { owner: parts[0], name: parts[1] };
  }

  function addRepoSpec(spec: string) {
    const parsed = parseRepoSpec(spec);
    repoPaste = "";
    if (!parsed) return;
    if (repoRows.some((r) => r.owner.toLowerCase() === parsed.owner.toLowerCase() && r.name.toLowerCase() === parsed.name.toLowerCase())) return;
    repoRows = [...repoRows, { owner: parsed.owner, name: parsed.name, why: "added", selected: true }];
  }

  /** Reads the clipboard text directly rather than waiting a tick for the
   * input's own value to catch up post-paste. */
  function handleRepoPaste(e: ClipboardEvent) {
    const text = e.clipboardData?.getData("text");
    if (text) {
      e.preventDefault();
      addRepoSpec(text);
    }
  }

  // --- People ---
  interface PersonRow extends PersonSuggestion {
    selected: boolean;
  }
  let peopleRows = $state<PersonRow[]>([]);
  let peopleQuery = $state("");

  async function loadPeopleSuggestions(query: string) {
    try {
      const list = await api.suggestPeople(query, 8);
      const prevSelected = new Map(peopleRows.map((r) => [r.login, r.selected]));
      peopleRows = list.map((s) => ({ ...s, selected: prevSelected.get(s.login) ?? s.source !== "bot" }));
    } catch {
      peopleRows = [];
    }
  }

  function togglePerson(i: number) {
    peopleRows[i] = { ...peopleRows[i], selected: !peopleRows[i].selected };
  }

  // --- Finish ---
  let finishing = $state(false);
  let finishError = $state<string | null>(null);
  let selectedRepoCount = $derived(repoRows.filter((r) => r.selected).length);
  let selectedPeopleCount = $derived(peopleRows.filter((r) => r.selected).length);
  let canFinish = $derived(signedInLogin !== null && selectedRepoCount > 0);

  async function finish() {
    finishing = true;
    finishError = null;
    try {
      for (const r of repoRows) {
        if (!r.selected) continue;
        try {
          await reposStore.add(`${r.owner}/${r.name}`);
        } catch {
          // best-effort: one bad suggestion shouldn't block the rest
        }
      }
      const logins = peopleRows.filter((r) => r.selected).map((r) => r.login);
      if (logins.length > 0) {
        // The very first team (docs/CONTRACT.md work item 5): a fresh
        // install has none yet, so this always creates rather than updates.
        await teamsStore.create("Team", logins);
      }
      onDone();
    } catch (e) {
      finishError = isEngineError(e) ? engineErrorMessage(e) : "Something went wrong starting up.";
    } finally {
      finishing = false;
    }
  }
</script>

<div class="onboarding">
  <div class="intro">
    <h1>Vigie <span class="pronunciation">vee-zhee · French for lookout</span></h1>
    <p class="muted">Watch your team on GitHub. Sign in once, we suggest the rest. Nothing leaves this Mac except calls to GitHub.</p>
  </div>

  <section class="field account">
    <span class="label">GitHub account</span>
    <DeviceSignIn onSignedIn={handleSignedIn} />
  </section>

  <div class="columns" class:disabled={!signedInLogin}>
    <div class="field">
      <span class="label">Repos · from your recent activity</span>
      <div class="tick-list">
        {#each repoRows as r, i (`${r.owner}/${r.name}`)}
          <label class="tick-row">
            <input type="checkbox" checked={r.selected} onchange={() => toggleRepo(i)} disabled={!signedInLogin} />
            <span class="mono">{r.owner}/{r.name}</span>
            <span class="muted">{r.why}</span>
          </label>
        {:else}
          <span class="muted">{signedInLogin ? "No suggestions — paste a repo below." : "Sign in to see suggestions."}</span>
        {/each}
      </div>
      <input
        class="ph-input"
        placeholder="or paste owner/name"
        bind:value={repoPaste}
        disabled={!signedInLogin}
        onkeydown={(e) => e.key === "Enter" && addRepoSpec(repoPaste)}
        onpaste={handleRepoPaste}
      />
    </div>
    <div class="field">
      <span class="label">People · contributors of those repos</span>
      <div class="tick-list">
        {#each peopleRows as p, i (p.login)}
          <label class="tick-row">
            <input type="checkbox" checked={p.selected} onchange={() => togglePerson(i)} disabled={!signedInLogin} />
            <span>{p.login}</span>
            <span class="muted">{p.why}</span>
          </label>
        {:else}
          <span class="muted">{signedInLogin ? "Tick a repo to see contributors." : "Sign in to see suggestions."}</span>
        {/each}
      </div>
      <input
        class="ph-input"
        placeholder="search contributors and org members"
        bind:value={peopleQuery}
        disabled={!signedInLogin}
        oninput={() => loadPeopleSuggestions(peopleQuery)}
      />
    </div>
  </div>

  <div class="finish-row">
    <span class="muted">{selectedRepoCount} repos · {selectedPeopleCount} people</span>
    <span class="spacer"></span>
    {#if finishError}<span class="error-text">{finishError}</span>{/if}
    <button class="btn-primary" disabled={!canFinish || finishing} onclick={finish}>
      {finishing ? "Starting…" : "Start watching"}
    </button>
  </div>
</div>

<style>
  .onboarding {
    max-width: 640px;
    margin: 0 auto;
    padding: 48px 24px 32px;
    display: flex;
    flex-direction: column;
    gap: 24px;
    height: 100%;
    overflow-y: auto;
    box-sizing: border-box;
  }
  .intro {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .intro h1 {
    font-size: 22px;
    font-weight: 600;
    margin: 0;
  }
  .pronunciation {
    font-size: 13px;
    font-weight: 400;
    color: var(--muted);
    padding-left: 6px;
  }
  .intro p {
    margin: 0;
    font-size: 13px;
    color: var(--label);
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
  .columns {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 24px;
  }
  .columns.disabled {
    opacity: 0.6;
  }
  .tick-list {
    display: flex;
    flex-direction: column;
    gap: 8px;
    min-height: 24px;
  }
  .tick-row {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 13px;
    cursor: pointer;
  }
  .ph-input {
    height: 28px;
    margin-top: 4px;
    font-size: 12px;
  }
  .finish-row {
    display: flex;
    align-items: center;
    gap: 14px;
  }
  .spacer {
    flex-grow: 1;
  }
  .mono {
    font-family: ui-monospace, Menlo, monospace;
    font-size: 12px;
  }
</style>
