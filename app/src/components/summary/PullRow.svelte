<script lang="ts">
  // One open-PR row: title, repo, author avatar, age, last activity, size —
  // shared by InFlightPulls' grouped sections and EngineerFocus'
  // "In flight"/"Reviewing" lists so the two don't each grow their own copy
  // of this markup. The row's own click opens Vigie's own detail pane (the
  // principle every list view follows since b1f385e: if Vigie has a page
  // for it, clicking opens that page) — GitHub is a secondary control.
  import { openUrl } from "@tauri-apps/plugin-opener";
  import type { OpenPull } from "../../lib/types";
  import { relativeTime } from "../../lib/time";
  import { detailStore, type DetailTarget } from "../../lib/stores";
  import Avatar from "../Avatar.svelte";
  import Icon from "../Icon.svelte";

  let {
    pull,
    repoLabel,
    siblings,
  }: {
    pull: OpenPull;
    repoLabel: string;
    /** The caller's own currently-rendered list, snapshotted at render time
     * — what the pane's previous/next steps through (the in-flight list, or
     * the careful-read five). */
    siblings: DetailTarget[];
  } = $props();

  function open() {
    detailStore.openThread(pull.repo_id, pull.number, { origin: "Summary", siblings });
  }

  /** The row's own click already opens in-app; this is the row's only other
   * button (a sibling, never nested inside the row button), so
   * `stopPropagation` is belt-and-suspenders, matching PullRequests.svelte's
   * `openOnGitHub`. */
  function openOnGitHub(e: MouseEvent) {
    e.stopPropagation();
    openUrl(pull.url).catch(() => {
      // best-effort — same as DetailPane's openOnGitHub
    });
  }

  let size = $derived(
    pull.additions != null && pull.deletions != null ? `+${pull.additions} −${pull.deletions}` : null,
  );
</script>

<div class="pull-row-wrap">
  <button class="pull-row" onclick={open}>
    <Avatar login={pull.author_login} avatarUrl={pull.author_avatar_url} size={22} />
    <span class="title">{pull.title}</span>
    <span class="muted repo">{repoLabel}#{pull.number}</span>
    {#if pull.draft}<span class="draft-tag">Draft</span>{/if}
    {#if size}<span class="muted size">{size}</span>{/if}
    <span class="muted age">opened {relativeTime(pull.created_at)}</span>
    <span class="muted last-active">active {relativeTime(pull.last_activity_at)}</span>
  </button>
  <button class="ext-link" onclick={openOnGitHub} title="Open on GitHub">
    <Icon name="external" size={13} />
  </button>
</div>

<style>
  /* Positions `.ext-link` over the row's own right edge as a real sibling
     button, never nested inside `.pull-row` — see PullRequests.svelte's
     identical `.pr-row-wrap` comment. */
  .pull-row-wrap {
    position: relative;
  }
  .pull-row {
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 10px;
    padding: 8px 28px 8px 0;
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
    transition: background 0.12s;
  }
  .pull-row-wrap:hover .pull-row {
    background: var(--surface-active);
    border-radius: 6px;
  }
  .title {
    font-size: 13px;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex-grow: 1;
    min-width: 0;
  }
  .repo {
    flex-shrink: 0;
  }
  .draft-tag {
    font-size: 11px;
    font-weight: 500;
    padding: 2px 6px;
    border-radius: 4px;
    background: var(--surface-sunken);
    color: var(--muted);
    flex-shrink: 0;
  }
  .size {
    flex-shrink: 0;
    font-variant-numeric: tabular-nums;
  }
  .age,
  .last-active {
    flex-shrink: 0;
    width: 92px;
    text-align: right;
  }
  /* Muted until the row (or the button itself) is hovered/focused — the
     in-app open stays the obvious default action; this is clearly
     secondary. Same reveal-on-hover recipe as PullRequests.svelte's
     `.ext-link`. */
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
  .pull-row-wrap:hover .ext-link,
  .ext-link:focus-visible {
    opacity: 1;
  }
  .ext-link:hover {
    background: var(--surface-sunken);
    color: var(--text);
  }
</style>
