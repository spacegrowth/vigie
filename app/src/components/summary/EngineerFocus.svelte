<script lang="ts">
  // Engineer focus (packet §4): shown once a person is selected, below the
  // KPI row (already re-scoped to that person by the caller — see
  // Summary.svelte). Ordered per the framing rule (packet §A6 — "where can
  // I help", never "who did more"): their own "In flight" and "Reviewing"
  // open-PR lists come first (what's blocked and what's waiting on them),
  // the per-day activity bar (a volume count) comes last. The "Most
  // discussed" section further down the page is already actor-filtered via
  // `digestData` when a person is selected, so it needs no separate copy
  // here for "threads they drove".
  import { ALL_EVENT_KINDS } from "../../lib/types";
  import type { DayCounts, OpenPull } from "../../lib/types";
  import { KIND_BAR_COLOR, KIND_TOOLTIP, presentKinds, totalOf } from "../../lib/summary";
  import type { DetailTarget } from "../../lib/stores";
  import { dayLabel } from "../../lib/time";
  import PullRow from "./PullRow.svelte";

  let {
    series,
    inFlight,
    inFlightError,
    reviewing,
    reviewingError,
    repoLabel,
  }: {
    series: DayCounts[];
    inFlight: OpenPull[] | null;
    inFlightError: string | null;
    reviewing: OpenPull[] | null;
    reviewingError: string | null;
    repoLabel: (repoId: number) => string;
  } = $props();

  let maxDay = $derived(series.reduce((m, d) => Math.max(m, totalOf(d.counts)), 0));

  // Each list's own siblings, so the pane steps through "In flight" or
  // "Reviewing" — not the other list, and not merged across the two.
  let inFlightSiblings = $derived<DetailTarget[]>(
    (inFlight ?? []).map((p) => ({ kind: "thread", repoId: p.repo_id, number: p.number })),
  );
  let reviewingSiblings = $derived<DetailTarget[]>(
    (reviewing ?? []).map((p) => ({ kind: "thread", repoId: p.repo_id, number: p.number })),
  );
</script>

<div class="lists">
  <div class="list">
    <div class="section-label">In flight · {inFlight?.length ?? "—"}</div>
    {#if inFlightError}
      <p class="error-text">{inFlightError}</p>
    {:else if inFlight === null}
      <p class="muted">Loading…</p>
    {:else if inFlight.length === 0}
      <p class="muted">No open PRs authored right now.</p>
    {:else}
      {#each inFlight as pull (`${pull.repo_id}:${pull.number}`)}
        <PullRow {pull} repoLabel={repoLabel(pull.repo_id)} siblings={inFlightSiblings} />
      {/each}
    {/if}
  </div>

  <div class="list">
    <div class="section-label">Reviewing · {reviewing?.length ?? "—"}</div>
    {#if reviewingError}
      <p class="error-text">{reviewingError}</p>
    {:else if reviewing === null}
      <p class="muted">Loading…</p>
    {:else if reviewing.length === 0}
      <p class="muted">Not a requested reviewer on any open PR right now.</p>
    {:else}
      {#each reviewing as pull (`${pull.repo_id}:${pull.number}`)}
        <PullRow {pull} repoLabel={repoLabel(pull.repo_id)} siblings={reviewingSiblings} />
      {/each}
    {/if}
  </div>
</div>

<div class="section-label">Activity by day</div>
{#if series.length === 0}
  <p class="muted">No day-by-day activity in this window.</p>
{:else}
  <div class="day-bars">
    {#each series as d (d.day)}
      {@const dayTotal = totalOf(d.counts)}
      <div class="day-col">
        <div class="day-bar-track" role="img" aria-label={`${dayLabel(d.day)}: ${dayTotal} events`}>
          <div class="day-bar" style:--h="{dayTotal > 0 ? Math.max((dayTotal / Math.max(maxDay, 1)) * 100, 6) : 0}%">
            {#each presentKinds(d.counts) as kind (kind)}
              <span
                class="seg"
                style:--sh="{(d.counts[kind] / Math.max(dayTotal, 1)) * 100}%"
                style:background={KIND_BAR_COLOR[kind]}
                title={KIND_TOOLTIP[kind](d.counts[kind])}
              ></span>
            {/each}
          </div>
        </div>
        <span class="day-tick muted">{dayLabel(d.day)}</span>
      </div>
    {/each}
  </div>
  <div class="legend">
    {#each ALL_EVENT_KINDS.filter((k) => series.some((d) => d.counts[k] > 0)) as kind (kind)}
      <span class="legend-item">
        <span class="dot" style:background={KIND_BAR_COLOR[kind]}></span>
        <span class="muted">{KIND_TOOLTIP[kind](series.reduce((s, d) => s + d.counts[kind], 0))}</span>
      </span>
    {/each}
  </div>
{/if}

<style>
  .day-bars {
    display: flex;
    flex-direction: row;
    align-items: flex-end;
    gap: 4px;
    height: 90px;
    padding: 4px 0;
  }
  .day-col {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 4px;
    flex: 1;
    min-width: 0;
    height: 100%;
  }
  .day-bar-track {
    display: flex;
    align-items: flex-end;
    height: 72px;
    width: 100%;
  }
  .day-bar {
    display: flex;
    flex-direction: column-reverse;
    width: 100%;
    height: var(--h, 0%);
    border-radius: 2px 2px 0 0;
    overflow: hidden;
    background: var(--surface-sunken);
  }
  .seg {
    display: block;
    width: 100%;
    height: var(--sh, 0%);
  }
  .day-tick {
    font-size: 10px;
    white-space: nowrap;
  }
  .legend {
    display: flex;
    flex-direction: row;
    flex-wrap: wrap;
    gap: 10px;
    margin: 8px 0 4px 0;
  }
  .legend-item {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 2px;
    flex-shrink: 0;
  }
  .lists {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
    gap: 20px;
    margin-top: 8px;
  }
</style>
