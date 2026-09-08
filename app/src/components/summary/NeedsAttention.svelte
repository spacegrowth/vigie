<script lang="ts">
  // Needs attention (packet §6): one line per rule, count included, with the
  // offending items behind a disclosure. Always team-wide (see
  // lib/summary.ts's computeAttentionRules) regardless of whether a person
  // is currently selected elsewhere on the page. An item about a thread
  // (has `repo_id`/`number`) opens Vigie's own detail pane as its primary
  // click, with GitHub as a secondary control — same principle as PullRow.
  // A person-level item (no thread — `zeroActivity`'s team member,
  // `reviewImbalance`'s reviewer) has never had anywhere to click and still
  // doesn't.
  import { openUrl } from "@tauri-apps/plugin-opener";
  import type { AttentionRule } from "../../lib/summary";
  import { detailStore } from "../../lib/stores";
  import Icon from "../Icon.svelte";

  let {
    rules,
    pullsError = null,
  }: {
    rules: AttentionRule[];
    /** Set when the team-wide `openPulls` call behind five of these six
     * rules failed — `zeroActivity` (the one rule that needs only the
     * digest, not pulls) is still trustworthy, but the rest read as "0
     * found" whether or not that's true, so this renders as a caveat
     * instead of letting the page imply everything's clear. */
    pullsError?: string | null;
  } = $props();

  let open = $state<Set<string>>(new Set());
  function toggle(key: string) {
    open = new Set(open.has(key) ? [...open].filter((k) => k !== key) : [...open, key]);
  }

  function openThread(repoId: number, number: number) {
    detailStore.openThread(repoId, number, { origin: "Summary" });
  }

  function openOnGitHub(e: MouseEvent, url: string) {
    e.stopPropagation();
    openUrl(url).catch(() => {
      // best-effort — same as PullRow's opener
    });
  }
</script>

<div class="section-label">Needs attention</div>
{#if pullsError}
  <p class="error-text">Couldn't load open PRs, so every PR-based rule below reads "0 found" whether or not that's true: {pullsError}</p>
{/if}
<div class="rules">
  {#each rules as rule (rule.key)}
    <div class="rule">
      <button
        class="rule-head"
        class:flagged={rule.count > 0}
        class:unavailable={rule.unavailable}
        onclick={() => rule.items.length > 0 && toggle(rule.key)}
        disabled={rule.items.length === 0}
      >
        <span class="dot" class:on={rule.count > 0 && !rule.unavailable}></span>
        <span class="rule-label">{rule.label}</span>
        {#if rule.items.length > 0}
          <span class="disclosure muted">{open.has(rule.key) ? "hide" : "show"}</span>
        {/if}
      </button>
      {#if open.has(rule.key) && rule.items.length > 0}
        <ul class="items">
          {#each rule.items as item, i (item.repo_id != null && item.number != null ? `${item.repo_id}:${item.number}` : (item.url ?? `${rule.key}:${i}`))}
            <li>
              {#if item.repo_id != null && item.number != null}
                <div class="item-row">
                  <button class="item-link" onclick={() => openThread(item.repo_id!, item.number!)}>{item.label}</button>
                  {#if item.url}
                    <button class="ext-link" onclick={(e) => openOnGitHub(e, item.url!)} title="Open on GitHub">
                      <Icon name="external" size={12} />
                    </button>
                  {/if}
                </div>
              {:else}
                <span>{item.label}</span>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
    </div>
  {/each}
</div>

<style>
  .rules {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .rule-head {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    text-align: left;
    background: none;
    border: none;
    padding: 7px 0;
    font-family: inherit;
    font-size: 13px;
    color: inherit;
    cursor: pointer;
  }
  .rule-head:disabled {
    cursor: default;
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--border);
    flex-shrink: 0;
  }
  .dot.on {
    background: var(--warn);
  }
  .rule-label {
    flex-grow: 1;
  }
  .unavailable .rule-label {
    color: var(--muted);
  }
  .disclosure {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.03em;
  }
  .items {
    list-style: none;
    margin: 0 0 6px 0;
    padding: 0 0 0 15px;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .items li {
    font-size: 12px;
    color: var(--muted-strong);
  }
  .item-row {
    position: relative;
    display: flex;
    align-items: center;
    gap: 4px;
    padding-right: 20px;
  }
  .item-link {
    background: none;
    border: none;
    padding: 0;
    font-family: inherit;
    font-size: 12px;
    color: var(--accent);
    cursor: pointer;
    text-align: left;
  }
  .item-link:hover {
    color: var(--link-hover);
    text-decoration: underline;
  }
  /* Muted until the row (or the button itself) is hovered/focused — same
     reveal-on-hover recipe as PullRow's `.ext-link`, sized down for this
     denser list. */
  .ext-link {
    position: absolute;
    top: 50%;
    right: 0;
    transform: translateY(-50%);
    display: flex;
    align-items: center;
    justify-content: center;
    width: 18px;
    height: 18px;
    padding: 0;
    background: none;
    border: none;
    border-radius: 4px;
    color: var(--muted);
    cursor: pointer;
    opacity: 0;
    transition: opacity 0.12s, background 0.12s, color 0.12s;
  }
  .item-row:hover .ext-link,
  .ext-link:focus-visible {
    opacity: 1;
  }
  .ext-link:hover {
    background: var(--surface-sunken);
    color: var(--text);
  }
</style>
