<script lang="ts">
  // Team-wide in-flight PRs (packet §5): every open PR in scope, grouped by
  // state in a fixed order. Shown in the roster (team) view; Engineer focus
  // has its own narrower "In flight"/"Reviewing" lists instead of this.
  import type { OpenPullGroup } from "../../lib/summary";
  import type { DetailTarget } from "../../lib/stores";
  import PullRow from "./PullRow.svelte";

  let {
    groups,
    total,
    repoLabel,
  }: {
    groups: OpenPullGroup[];
    total: number;
    repoLabel: (repoId: number) => string;
  } = $props();

  /** The pane's previous/next steps through every in-flight PR across every
   * group, in the order the groups render — not just the one group a row
   * happens to sit in. */
  let siblings = $derived<DetailTarget[]>(
    groups.flatMap((g) => g.pulls.map((p) => ({ kind: "thread", repoId: p.repo_id, number: p.number }))),
  );
</script>

<div class="section-label">In-flight PRs · {total}</div>
{#if total === 0}
  <p class="muted">No open PRs in scope.</p>
{:else}
  {#each groups as group (group.group)}
    <div class="group">
      <div class="group-label">{group.label} · {group.pulls.length}</div>
      {#each group.pulls as pull (`${pull.repo_id}:${pull.number}`)}
        <PullRow {pull} repoLabel={repoLabel(pull.repo_id)} {siblings} />
      {/each}
    </div>
  {/each}
{/if}

<style>
  .group {
    margin-bottom: 10px;
  }
  .group-label {
    font-size: 12px;
    font-weight: 600;
    color: var(--label);
    padding: 8px 0 4px 0;
  }
</style>
