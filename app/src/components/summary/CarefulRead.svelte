<script lang="ts">
  // Worth a careful read (packet §A4): top 5 open PRs by the deterministic
  // score in lib/summary.ts, with the reason in words. Never phrased as a
  // quality judgement — "worth a careful read", not "risky" or "bad". The
  // row's own click opens Vigie's own detail pane; GitHub is a secondary
  // control (same principle as PullRow — see its comment).
  import { openUrl } from "@tauri-apps/plugin-opener";
  import type { CarefulReadItem } from "../../lib/summary";
  import { detailStore, type DetailTarget } from "../../lib/stores";
  import Icon from "../Icon.svelte";

  let {
    items,
    usedCommentRounds,
    repoLabel,
  }: {
    items: CarefulReadItem[];
    /** `false` when the "comment rounds" term was dropped for lack of any
     * thread data this window — captioned rather than left silent. */
    usedCommentRounds: boolean;
    repoLabel: (repoId: number) => string;
  } = $props();

  /** The pane's previous/next steps through this same five, in the order
   * shown. */
  let siblings = $derived<DetailTarget[]>(
    items.map((item) => ({ kind: "thread", repoId: item.pull.repo_id, number: item.pull.number })),
  );

  function open(item: CarefulReadItem) {
    detailStore.openThread(item.pull.repo_id, item.pull.number, { origin: "Summary", siblings });
  }

  function openOnGitHub(e: MouseEvent, url: string) {
    e.stopPropagation();
    openUrl(url).catch(() => {
      // best-effort — same as PullRow's opener
    });
  }
</script>

<div class="section-label">Worth a careful read</div>
{#if !usedCommentRounds && items.length > 0}
  <p class="muted note">No discussion data this window — scored on size, age and reviewer count only.</p>
{/if}
{#if items.length === 0}
  <p class="muted">No open PRs to score.</p>
{:else}
  <div class="rows">
    {#each items as item (`${item.pull.repo_id}:${item.pull.number}`)}
      <div class="item-row-wrap">
        <button class="item-row" onclick={() => open(item)}>
          <span class="title">{item.pull.title}</span>
          <span class="muted ref">{repoLabel(item.pull.repo_id)}#{item.pull.number}</span>
          <span class="muted reason">{item.reason}</span>
        </button>
        <button class="ext-link" onclick={(e) => openOnGitHub(e, item.pull.url)} title="Open on GitHub">
          <Icon name="external" size={13} />
        </button>
      </div>
    {/each}
  </div>
{/if}

<style>
  .note {
    margin: -2px 0 8px 0;
  }
  .rows {
    display: flex;
    flex-direction: column;
  }
  /* Positions `.ext-link` over the row's own right edge as a real sibling
     button, never nested inside `.item-row` — see PullRequests.svelte's
     identical `.pr-row-wrap` comment. */
  .item-row-wrap {
    position: relative;
  }
  .item-row {
    display: flex;
    flex-direction: row;
    align-items: baseline;
    gap: 10px;
    padding: 9px 28px 9px 0;
    border-bottom: 1px solid var(--divider);
    background: none;
    border-left: none;
    border-right: none;
    border-top: none;
    text-align: left;
    cursor: pointer;
    font-family: inherit;
    color: inherit;
    width: 100%;
  }
  .item-row-wrap:hover .item-row {
    background: var(--surface-active);
    border-radius: 6px;
  }
  .title {
    font-size: 13px;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex-shrink: 1;
    min-width: 0;
  }
  .ref {
    font-size: 12px;
    flex-shrink: 0;
  }
  .reason {
    font-size: 12px;
    flex-shrink: 0;
    margin-left: auto;
    text-align: right;
  }
  /* Muted until the row (or the button itself) is hovered/focused — same
     reveal-on-hover recipe as PullRow's `.ext-link`. */
  .ext-link {
    position: absolute;
    top: 50%;
    right: 0;
    transform: translateY(-50%);
    display: flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    padding: 0;
    background: none;
    border: none;
    border-radius: 5px;
    color: var(--muted);
    cursor: pointer;
    opacity: 0;
    transition: opacity 0.12s, background 0.12s, color 0.12s;
  }
  .item-row-wrap:hover .ext-link,
  .ext-link:focus-visible {
    opacity: 1;
  }
  .ext-link:hover {
    background: var(--surface-sunken);
    color: var(--text);
  }
</style>
