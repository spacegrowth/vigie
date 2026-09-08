<script lang="ts">
  // The slim poll status bar across the bottom of the main area. Left is
  // "is a poll running right now", middle is a one-line health summary
  // (packet gm-statusbar-r2 — replaced the r1 per-repo dot row with words:
  // quiet when everything's fine, specific when it isn't), right is the API
  // budget and the last error, if any. Everything here is derived from
  // `pollStore` / `reposStore` / `accountsStore` — no state of its own
  // beyond the render.
  import Icon from "./Icon.svelte";
  import type { ViewName } from "./Sidebar.svelte";
  import { accountsStore, pollStore, reposStore } from "../lib/stores";
  import { healthSummary } from "../lib/status";
  import { relativeTime } from "../lib/time";
  import { rateLimitMeter } from "../lib/rateLimitMeter";

  let {
    now,
    onNavigate,
  }: {
    /** Current time in unix seconds, ticked by App.svelte every 30s — the
     * same clock Sidebar.svelte's footer uses, so both agree. */
    now: number;
    onNavigate: (v: ViewName) => void;
  } = $props();

  const { polling, lastPolledAt, rateLimitRemaining, rateLimitResetAt, rateLimitLimit, lastError, lastPollErrors } =
    pollStore;
  const { items: repoItems } = reposStore;
  const { signedOutLogins } = accountsStore;

  let health = $derived(healthSummary($repoItems, $lastPollErrors, $signedOutLogins));

  let leftText = $derived(
    $lastPolledAt == null ? "Not polled yet · Cmd+R polls now" : `Polled ${relativeTime($lastPolledAt, now)}`,
  );

  // `null` until the first poll comes back — rendered as nothing at all
  // rather than an empty track (see the `{#if meter}` below).
  let meter = $derived(
    $rateLimitRemaining == null
      ? null
      : rateLimitMeter($rateLimitRemaining, $rateLimitLimit, $rateLimitResetAt, now),
  );

  function goToRepos() {
    onNavigate("repos");
  }
</script>

<div class="bar">
  <div class="left">
    {#if $polling}
      <span class="spin"><Icon name="spinner" size={10} /></span>
      <span>Polling…</span>
    {:else}
      <span>{leftText}</span>
    {/if}
  </div>
  <button class="health" title={health.title || undefined} onclick={goToRepos}>
    {#each health.segments as segment, i (i)}
      {#if i > 0}<span class="sep"> · </span>{/if}
      <span class="tone-{segment.tone}">{segment.text}</span>
    {/each}
  </button>
  <div class="right">
    {#if $lastError}
      <span class="error-snippet" title={$lastError}>{$lastError.slice(0, 60)}</span>
    {/if}
    {#if meter}
      <span class="meter" title={meter.title}>
        <span class="meter-track">
          <span class="meter-fill tone-{meter.tone}" style:--fill="{meter.fraction * 100}%"></span>
        </span>
        <span class="rate">{meter.countText}</span>
      </span>
    {/if}
  </div>
</div>

<style>
  .bar {
    display: flex;
    align-items: center;
    height: 24px;
    flex-shrink: 0;
    padding: 0 10px;
    gap: 10px;
    background: var(--surface);
    border-top: 1px solid var(--border);
    color: var(--muted);
    font-size: 11px;
  }
  .left {
    display: flex;
    align-items: center;
    gap: 5px;
    flex-shrink: 0;
    white-space: nowrap;
  }
  .spin {
    display: inline-flex;
    animation: spin 0.8s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  .health {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    background: none;
    border: 0;
    padding: 0;
    margin: 0;
    font: inherit;
    color: var(--muted);
    text-align: left;
    cursor: pointer;
  }
  .health .sep {
    color: var(--muted);
  }
  .health .tone-danger {
    color: var(--danger);
  }
  .health .tone-warn {
    color: var(--warn);
  }
  .right {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-left: auto;
    flex-shrink: 0;
    white-space: nowrap;
  }
  .error-snippet {
    color: var(--danger);
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 260px;
  }
  /* The API budget meter: a fixed-width track so the number changing next
     to it never reflows the bar, plus the count in the same quiet text the
     rest of the bar uses — colour lives on the fill alone (see
     lib/rateLimitMeter.ts), never on this text, so the state still reads
     with no colour at all. */
  .meter {
    display: inline-flex;
    align-items: center;
    gap: 2px;
  }
  .meter-track {
    width: 56px;
    height: 4px;
    flex-shrink: 0;
    border-radius: 2px;
    background: var(--divider);
    overflow: hidden;
  }
  .meter-fill {
    display: block;
    height: 100%;
    width: var(--fill, 0%);
    border-radius: 2px;
    background: var(--muted);
    transition: width 0.3s ease;
  }
  .meter-fill.tone-warn {
    background: var(--warn);
  }
  .meter-fill.tone-danger {
    background: var(--danger);
  }
  .rate {
    color: var(--muted);
  }
</style>
