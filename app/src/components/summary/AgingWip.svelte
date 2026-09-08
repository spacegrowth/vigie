<script lang="ts">
  // Aging WIP (packet §A1): replaces the activity-mix bar in the same slot.
  // Open PRs bucketed by age as one horizontal bar with counts; clicking a
  // bucket filters the pull list rendered directly below (clicking the same
  // bucket again clears it). `pulls` is whatever's already in scope on the
  // page (team-wide, or one person's once selected) — same population the
  // KPI gauges and in-flight lists already use.
  import type { OpenPull } from "../../lib/types";
  import { AGE_BUCKET_ORDER, agingWipBuckets, filterPullsByAgeBucket, type AgeBucket } from "../../lib/summary";
  import type { DetailTarget } from "../../lib/stores";
  import PullRow from "./PullRow.svelte";

  let {
    pulls,
    now,
    repoLabel,
  }: {
    pulls: OpenPull[];
    now: number;
    repoLabel: (repoId: number) => string;
  } = $props();

  let selected = $state<AgeBucket | null>(null);

  function clickBucket(b: AgeBucket) {
    selected = selected === b ? null : b;
  }

  let buckets = $derived(agingWipBuckets(pulls, now));
  let total = $derived(pulls.length);
  let filtered = $derived(filterPullsByAgeBucket(pulls, selected, now));
  // The pane's previous/next steps through the currently-filtered bucket's
  // own rows, not the whole unfiltered set.
  let siblings = $derived<DetailTarget[]>(filtered.map((p) => ({ kind: "thread", repoId: p.repo_id, number: p.number })));
</script>

<div class="section-label">Aging WIP · {total}</div>
{#if total === 0}
  <p class="muted">No open PRs in scope.</p>
{:else}
  <div class="bucket-bar" role="group" aria-label="Open PRs by age">
    {#each buckets as b (b.bucket)}
      <button
        class="bucket"
        class:on={selected === b.bucket}
        class:dim={selected !== null && selected !== b.bucket}
        onclick={() => clickBucket(b.bucket)}
        aria-pressed={selected === b.bucket}
      >
        <span class="fill-track">
          <span class="fill" style:--w="{total > 0 ? (b.count / total) * 100 : 0}%"></span>
        </span>
        <span class="bucket-label">{b.label}</span>
        <span class="bucket-count">{b.count}</span>
      </button>
    {/each}
  </div>

  {#if filtered.length === 0}
    <p class="muted">No open PRs in this bucket.</p>
  {:else}
    <div class="rows">
      {#each filtered as pull (`${pull.repo_id}:${pull.number}`)}
        <PullRow {pull} repoLabel={repoLabel(pull.repo_id)} {siblings} />
      {/each}
    </div>
  {/if}
{/if}

<style>
  .bucket-bar {
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    gap: 8px;
  }
  .bucket {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 8px 10px;
    border-radius: 7px;
    border: 1px solid var(--border-card);
    background: var(--surface-card);
    cursor: pointer;
    font-family: inherit;
    text-align: left;
    transition:
      background 0.12s,
      opacity 0.12s;
  }
  .bucket:hover {
    background: var(--surface-active);
  }
  .bucket.on {
    border-color: var(--accent);
  }
  .bucket.dim {
    opacity: 0.55;
  }
  .fill-track {
    display: block;
    height: 5px;
    border-radius: 2px;
    background: var(--surface-sunken);
    overflow: hidden;
  }
  .fill {
    display: block;
    height: 100%;
    width: var(--w, 0%);
    background: var(--accent);
    border-radius: 2px;
  }
  .bucket-label {
    font-size: 11px;
    color: var(--muted);
  }
  .bucket-count {
    font-size: 16px;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }
  .rows {
    display: flex;
    flex-direction: column;
    margin-top: 10px;
  }
</style>
