<script lang="ts">
  // The 6-tile KPI row (packet §2): big number, small label, a delta chip
  // vs. the previous window of equal length. Reused twice on the page —
  // team-scoped at the top, and again (same component, narrower scope) as
  // Engineer focus's header numbers once a person is selected, since
  // `tiles` already carries whatever scope the caller computed it for.
  import type { KpiTile } from "../../lib/summary";

  let { tiles }: { tiles: KpiTile[] } = $props();
</script>

<div class="kpi-row">
  {#each tiles as tile (tile.key)}
    <div class="tile">
      <div class="value">{tile.displayValue}</div>
      <div class="label">{tile.label}</div>
      {#if tile.secondaryLabel}
        <div class="secondary muted">{tile.secondaryLabel}</div>
      {/if}
      {#if tile.deltaLabel}
        <div class="delta" class:good={tile.deltaGood === true} class:bad={tile.deltaGood === false}>
          {tile.deltaLabel}
          <span class="muted">vs prev</span>
        </div>
      {:else}
        <div class="delta muted">—</div>
      {/if}
    </div>
  {/each}
</div>

<style>
  .kpi-row {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(140px, 1fr));
    gap: 12px;
    margin-bottom: 6px;
  }
  .tile {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 12px 14px;
    border-radius: 8px;
    background: var(--surface-card);
    border: 1px solid var(--border-card);
  }
  .value {
    font-size: 22px;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    line-height: 1.15;
  }
  .label {
    font-size: 11px;
    color: var(--label);
    text-transform: uppercase;
    letter-spacing: 0.03em;
  }
  .secondary {
    font-size: 11px;
    margin-top: 1px;
  }
  .delta {
    font-size: 12px;
    font-weight: 600;
    margin-top: 4px;
    display: flex;
    align-items: baseline;
    gap: 5px;
  }
  .delta.good {
    color: var(--ok);
  }
  .delta.bad {
    color: var(--danger);
  }
</style>
