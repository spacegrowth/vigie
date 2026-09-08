<script lang="ts">
  // Repos (docs/design/Repos.dc.html): paste (or type) a repo, then Add —
  // each row shows the repo, its default branch, when it last polled (or its
  // error), and a Remove text link.
  //
  // Adding used to fire straight from the paste event, which cancelled the
  // paste itself: the pasted text never reached the input, so there was no
  // way to see what had been pasted, correct a mistake, or pick the account
  // first. Paste now lands in the field like anywhere else and adding is an
  // explicit act (the Add button, or Enter).
  import { onMount } from "svelte";
  import Icon from "../components/Icon.svelte";
  import { api } from "../lib/api";
  import { scroller } from "../lib/scroller";
  import { BACKFILL_STEP_SPAN_SECS, describeNewRepoBackfill, stepNeedsFirstPoll } from "../lib/backfill";
  import { accountsStore, pollStore, reposStore } from "../lib/stores";
  import { isEngineError, engineErrorMessage, type Repo } from "../lib/types";
  import { relativeTime } from "../lib/time";

  const { items } = reposStore;
  const { items: accounts } = accountsStore;

  let spec = $state("");
  let adding = $state(false);
  let addError = $state<string | null>(null);
  // Which account a new repo is filed under — shown only with more than one
  // account (docs/CONTRACT.md "Accounts"); defaults to the first.
  let selectedAccountLogin = $state<string | null>(null);

  let confirmRemoveId = $state<number | null>(null);
  let removingId = $state<number | null>(null);

  // Hidden repos (docs/CONTRACT.md "Hiding a repo"). They are deliberately
  // *not* in `reposStore`: every other view reads that store, and a hidden
  // repo has to be absent from all of them — including the feed's own repo
  // filter, whose options are that same list. This page is the one place
  // they are still shown, so it fetches them itself.
  let hiddenRepos = $state<Repo[]>([]);
  // Collapsed until asked for, so a page about the repos being watched is
  // not led by the ones that are not — but opened straight after a hide, so
  // the row the user just acted on is visibly somewhere rather than gone.
  let hiddenOpen = $state(false);
  let hidingId = $state<number | null>(null);

  // Progress/result of the backfill step that runs right after adding a
  // repo (packet item 1) — keyed to one repo id at a time, so it shows next
  // to that row and quietly stops applying once a newer add replaces it.
  let backfillRepoId = $state<number | null>(null);
  let backfillStatus = $state<string | null>(null);

  onMount(async () => {
    await Promise.all([reposStore.refresh(), accountsStore.refresh(), refreshHidden()]);
    if (selectedAccountLogin === null && $accounts.length > 0) {
      selectedAccountLogin = $accounts[0].login;
    }
  });

  async function addRepo(value: string) {
    const v = value.trim();
    if (!v) return;
    adding = true;
    addError = null;
    try {
      const repo = await reposStore.add(v, $accounts.length > 1 ? selectedAccountLogin : undefined);
      spec = "";
      void backfillNewRepo(repo.id);
    } catch (e) {
      addError = isEngineError(e) ? engineErrorMessage(e) : "Couldn't add that repo.";
    } finally {
      adding = false;
    }
  }

  /** A newly added repo has no stored history yet — a poll only ever looks
   * back `FIRST_POLL_LOOKBACK_SECS` (24h; see crates/gitmon/src/poll.rs) —
   * so it fetches one backfill step (7 days) right away rather than leaving
   * the row looking broken. `add_repo` itself never polls (there is no
   * other call in this app that does either, right after an add), so the
   * repo's `backfilled_to` floor is still null the first time this runs:
   * that first attempt comes back "not polled yet", and this polls once
   * (the only way to set that floor) before retrying the same step. */
  async function backfillNewRepo(repoId: number) {
    backfillRepoId = repoId;
    backfillStatus = "Fetching the last 7 days…";
    try {
      let result = await api.backfill(repoId, BACKFILL_STEP_SPAN_SECS);
      if (stepNeedsFirstPoll(result, repoId)) {
        backfillStatus = "Polling for the first time…";
        try {
          await pollStore.pollNow();
        } catch (e) {
          backfillStatus = isEngineError(e) ? engineErrorMessage(e) : "Couldn't poll this repo yet.";
          return;
        }
        // That poll just set (or tried to set) this row's `last_polled_at`/
        // `last_error` — refresh so the row's own status line agrees with
        // the backfill status line landing right under it.
        await reposStore.refresh();
        result = await api.backfill(repoId, BACKFILL_STEP_SPAN_SECS);
      }
      if (result.rate_limit_remaining != null) pollStore.setRateLimitRemaining(result.rate_limit_remaining);
      backfillStatus = describeNewRepoBackfill(result, repoId);
    } catch (e) {
      backfillStatus = isEngineError(e) ? engineErrorMessage(e) : "Couldn't check for older activity.";
    }
  }

  async function refreshHidden() {
    hiddenRepos = await api.listHiddenRepos();
  }

  /** Hide or unhide, then re-read both lists: the repo moves between them,
   * and nothing it collected is touched either way. */
  async function setHidden(repoId: number, hidden: boolean) {
    hidingId = repoId;
    try {
      await api.setRepoHidden(repoId, hidden);
      await Promise.all([reposStore.refresh(), refreshHidden()]);
      if (hidden) hiddenOpen = true;
    } finally {
      hidingId = null;
    }
  }

  async function confirmRemove(repoId: number) {
    removingId = repoId;
    try {
      await reposStore.remove(repoId);
      await refreshHidden();
    } finally {
      removingId = null;
      confirmRemoveId = null;
    }
  }
</script>

<div class="head">
  <h1>Repos</h1>
  <span class="muted">{$items.length} watched</span>
</div>

<div class="body" use:scroller>
  <div class="add-row">
    <input
      class="ph-input"
      placeholder="paste or type owner/name, or a GitHub URL"
      bind:value={spec}
      disabled={adding}
      onkeydown={(e) => e.key === "Enter" && addRepo(spec)}
    />
    <button class="btn-primary" disabled={adding || spec.trim() === ""} onclick={() => addRepo(spec)}>
      {adding ? "Adding…" : "Add"}
    </button>
    {#if $accounts.length > 1}
      <div class="account-picker">
        <span class="muted">to</span>
        <select bind:value={selectedAccountLogin} disabled={adding}>
          {#each $accounts as account (account.login)}
            <option value={account.login}>{account.login}</option>
          {/each}
        </select>
      </div>
    {/if}
  </div>
  {#if addError}
    <p class="error-text">{addError}</p>
  {/if}

  {#if $items.length === 0 && hiddenRepos.length === 0}
    <div class="zero-state">
      <p class="muted">No repos yet — paste one above and press Add to start watching activity.</p>
    </div>
  {:else if $items.length === 0}
    <div class="zero-state">
      <p class="muted">Every repo is hidden right now — unhide one below, or add another above.</p>
    </div>
  {:else}
    {#each $items as repo (repo.id)}
      {@render repoRow(repo)}
    {/each}
  {/if}

  <!-- Hidden repos stay on the page that manages them rather than vanishing
       from it: this is the only place they are shown, and the only place
       they can be brought back from. -->
  {#if hiddenRepos.length > 0}
    <button
      class="hidden-head"
      aria-expanded={hiddenOpen}
      onclick={() => (hiddenOpen = !hiddenOpen)}
    >
      <span class="caret" class:open={hiddenOpen}><Icon name="chevron" size={14} /></span>
      Hidden ({hiddenRepos.length})
    </button>
    {#if hiddenOpen}
      <p class="muted hidden-note">
        A hidden repo is left out of every view and is never polled, so it costs no GitHub
        requests. Nothing it has already collected is deleted — unhide it and all of it comes
        straight back.
      </p>
      {#each hiddenRepos as repo (repo.id)}
        {@render repoRow(repo)}
      {/each}
    {/if}
  {/if}
</div>

<!-- One row, shown in the watched list or the Hidden section: `repo.hidden`
     decides which of Hide/Unhide it offers. Remove is on both, and keeps its
     confirmation on both — hiding is reversible and removing is not, and the
     two must never read alike. -->
{#snippet repoRow(repo: Repo)}
  <div class="repo-row">
    <div class="repo-id">
      <span class="repo-name">{repo.owner}/{repo.name}</span>
      {#if repo.hidden}
        <span class="muted">hidden — not polled, history kept</span>
      {:else if repo.last_error}
        <span class="error-text">{repo.last_error}</span>
      {:else}
        <span class="muted">{repo.last_polled_at ? `polled ${relativeTime(repo.last_polled_at)}` : "not polled yet"}</span>
      {/if}
      {#if backfillRepoId === repo.id && backfillStatus}
        <span class="muted">{backfillStatus}</span>
      {/if}
    </div>
    {#if $accounts.length > 1}
      <span class="muted account-tag">{repo.account_login || "unclaimed"}</span>
    {/if}
    <span class="muted branch">{repo.default_branch}</span>
    {#if confirmRemoveId === repo.id}
      <span class="muted">Remove, and delete everything collected?</span>
      <button class="btn-text danger" onclick={() => confirmRemove(repo.id)} disabled={removingId === repo.id}>
        {removingId === repo.id ? "Removing…" : "Confirm"}
      </button>
      <button class="btn-text" onclick={() => (confirmRemoveId = null)}>Cancel</button>
    {:else}
      <button
        class="btn-text hide"
        title={repo.hidden
          ? "Show this repo again and resume polling it. Everything it collected is still here."
          : "Stop showing and stop polling this repo. Nothing it has collected is deleted."}
        disabled={hidingId === repo.id}
        onclick={() => setHidden(repo.id, !repo.hidden)}
      >
        {#if hidingId === repo.id}
          {repo.hidden ? "Unhiding…" : "Hiding…"}
        {:else}
          {repo.hidden ? "Unhide" : "Hide"}
        {/if}
      </button>
      <button class="btn-text remove" onclick={() => (confirmRemoveId = repo.id)}>Remove</button>
    {/if}
  </div>
{/snippet}

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
  .add-row {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 4px 0 14px 0;
  }
  .ph-input {
    flex-grow: 1;
  }
  .account-picker {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-shrink: 0;
  }
  .account-tag {
    flex-shrink: 0;
  }
  .repo-row {
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 14px;
    padding: 12px 0;
    border-bottom: 1px solid var(--divider);
  }
  .repo-id {
    display: flex;
    flex-direction: column;
    gap: 3px;
    flex-grow: 1;
    min-width: 0;
  }
  .repo-name {
    font-size: 13px;
    font-weight: 500;
  }
  .branch {
    flex-shrink: 0;
  }
  .remove {
    /* Kept for layout only — the button reads like any other .btn-text;
       only the Confirm step goes red. */
    flex-shrink: 0;
  }
  .hide {
    /* Quieter than Remove: hiding is the reversible one, so it is muted text
       rather than the accent every other .btn-text carries. */
    flex-shrink: 0;
    color: var(--muted);
  }
  .hidden-head {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-top: 18px;
    padding: 6px 8px 6px 0;
    border: none;
    background: none;
    color: var(--muted);
    font-size: 12px;
    cursor: pointer;
  }
  .caret {
    display: flex;
    /* One chevron, rotated, rather than a second glyph for the open state. */
    transform: rotate(-90deg);
    transition: transform 0.12s;
  }
  .caret.open {
    transform: none;
  }
  .hidden-note {
    margin: 0 0 6px 0;
    max-width: 62ch;
    line-height: 1.5;
  }
  .danger {
    color: var(--danger);
  }
  .zero-state {
    padding: 24px 0;
  }
</style>
