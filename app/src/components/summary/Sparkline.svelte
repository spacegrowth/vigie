<script lang="ts">
  // A tiny per-day bar sparkline from a digest's `series` (packet's roster
  // column). `greyed` is used whenever the series isn't actually scoped to
  // one person — today that's always, since the contract's `series` lives
  // on `Digest` itself (team/actor scope), not per roster row; see
  // EngineerRoster's own note on this.
  import type { DayCounts } from "../../lib/types";
  import { pct, totalOf } from "../../lib/summary";

  let { series, greyed = false, ariaLabel }: { series: DayCounts[]; greyed?: boolean; ariaLabel: string } = $props();

  let totals = $derived(series.map((d) => totalOf(d.counts)));
  let max = $derived(totals.reduce((m, t) => Math.max(m, t), 0));
</script>

<div class="sparkline" class:greyed role="img" aria-label={ariaLabel}>
  {#each totals as t, i (i)}
    <span class="bar" style:--h="{Math.max(pct(t, max), t > 0 ? 8 : 0)}%"></span>
  {/each}
</div>

<style>
  .sparkline {
    display: flex;
    align-items: flex-end;
    gap: 1px;
    height: 20px;
    width: 100px;
    flex-shrink: 0;
  }
  .bar {
    flex: 1;
    min-width: 2px;
    height: var(--h, 0%);
    min-height: 1px;
    background: var(--accent);
    border-radius: 1px;
  }
  .greyed .bar {
    background: var(--border);
  }
</style>
