<script lang="ts">
  // Stuck panel (packet §A2): above the roster, team-wide regardless of any
  // selected person — same team-hygiene scope as Needs attention. Four
  // facts: unreviewed merges, p90 time to first review, the oldest PR that's
  // still actionable, and the reviewer sitting on the stalest request.
  // `pullsError` mirrors NeedsAttention's caveat: the oldest-PR and
  // reviewer-queue facts need `openPulls`, not just the digest. The oldest
  // PR's own click opens Vigie's own detail pane — the exact item that was
  // named ("oldest waiting for your review goes to GitHub") — with GitHub
  // as a secondary control, same principle as PullRow.
  //
  // gm-sumpolish-r1 §3: naming whatever has waited longest, full stop, means
  // this tile ends up permanently pointing at one abandoned PR nobody is
  // coming back to — training the user to ignore the whole panel. `oldest`
  // is already filtered to exclude those (see `oldestAwaitingReview` /
  // `ABANDONED_AFTER_SECS`); `excludedAbandonedCount` says how many were
  // hidden so silence here reads as "nothing is waiting", not as "the
  // waiting ones are dead".
  import { openUrl } from "@tauri-apps/plugin-opener";
  import { formatDuration, type OldestAwaitingReview, type ReviewerQueueWait } from "../../lib/summary";
  import { detailStore } from "../../lib/stores";
  import Icon from "../Icon.svelte";

  let {
    unreviewedMerges,
    p90TtfrSecs,
    oldest,
    excludedAbandonedCount,
    reviewerWait,
    pullsError = null,
    repoLabel,
  }: {
    unreviewedMerges: number;
    p90TtfrSecs: number | null;
    /** `null` when nothing is both awaiting review and still alive — either
     * nothing is awaiting review at all, or `pullsError` is set, or
     * everything that qualified has gone abandoned-quiet (see
     * `excludedAbandonedCount`). */
    oldest: OldestAwaitingReview | null;
    /** Non-draft, undecided PRs left out of `oldest` for having no activity
     * in over `ABANDONED_AFTER_SECS` — 0 means nothing was hidden. */
    excludedAbandonedCount: number;
    reviewerWait: ReviewerQueueWait | null;
    /** Set when the team-wide `openPulls` call failed — `unreviewedMerges`
     * and `p90TtfrSecs` come from the digest alone and stay trustworthy;
     * the other two read as "none" whether or not that's true. */
    pullsError?: string | null;
    repoLabel: (repoId: number) => string;
  } = $props();

  function openOnGitHub(e: MouseEvent, url: string) {
    e.stopPropagation();
    openUrl(url).catch(() => {
      // best-effort — same as PullRow's opener
    });
  }
</script>

<div class="section-label">Stuck</div>
{#if pullsError}
  <p class="error-text">Couldn't load open PRs, so the oldest-PR and reviewer-queue facts below read "none" whether or not that's true: {pullsError}</p>
{/if}
<div class="stuck-grid">
  <div class="stuck-item">
    <div class="stuck-value">{unreviewedMerges}</div>
    <div class="stuck-label">Unreviewed merges</div>
    <div class="stuck-note muted">Merged with no review stored — sees only what Vigie has watched.</div>
  </div>

  <div class="stuck-item">
    <div class="stuck-value">{formatDuration(p90TtfrSecs)}</div>
    <div class="stuck-label">p90 time to first review</div>
    <div class="stuck-note muted">9 in 10 waited no longer than this.</div>
  </div>

  <div class="stuck-item">
    <div class="stuck-label">Oldest still moving</div>
    {#if oldest}
      <div class="stuck-link-wrap">
        <button
          class="stuck-link"
          onclick={() => detailStore.openThread(oldest.pull.repo_id, oldest.pull.number, { origin: "Summary" })}
        >
          {repoLabel(oldest.pull.repo_id)}#{oldest.pull.number} {oldest.pull.title}
        </button>
        <button class="ext-link" onclick={(e) => openOnGitHub(e, oldest.pull.url)} title="Open on GitHub">
          <Icon name="external" size={12} />
        </button>
      </div>
      <div class="stuck-note muted">open {formatDuration(oldest.ageSecs)}</div>
      {#if excludedAbandonedCount > 0}
        <div class="stuck-note muted">{excludedAbandonedCount} older ignored as abandoned</div>
      {/if}
    {:else if excludedAbandonedCount > 0}
      <div class="stuck-note muted">{pullsError ? "—" : `All ${excludedAbandonedCount} waiting are abandoned — nothing to act on.`}</div>
    {:else}
      <div class="stuck-note muted">{pullsError ? "—" : "None right now."}</div>
    {/if}
  </div>

  <div class="stuck-item">
    <div class="stuck-label">Longest-waiting reviewer queue</div>
    {#if reviewerWait}
      <div class="stuck-value small">{reviewerWait.login}</div>
      <div class="stuck-note muted">
        {reviewerWait.queueSize} awaiting · oldest open {formatDuration(reviewerWait.oldestAgeSecs)}
      </div>
    {:else}
      <div class="stuck-note muted">{pullsError ? "—" : "Nobody has a queue right now."}</div>
    {/if}
  </div>
</div>

<style>
  .stuck-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
    gap: 12px;
  }
  .stuck-item {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 12px 14px;
    border-radius: 8px;
    background: var(--surface-card);
    border: 1px solid var(--border-card);
  }
  .stuck-value {
    font-size: 20px;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    line-height: 1.15;
  }
  .stuck-value.small {
    font-size: 15px;
  }
  .stuck-label {
    font-size: 11px;
    color: var(--label);
    text-transform: uppercase;
    letter-spacing: 0.03em;
  }
  .stuck-note {
    font-size: 11px;
    margin-top: 2px;
  }
  /* Positions `.ext-link` over the link's own right edge as a real sibling
     button, never nested inside `.stuck-link` — see PullRow's identical
     `.pull-row-wrap` comment. */
  .stuck-link-wrap {
    position: relative;
    padding-right: 20px;
  }
  .stuck-link {
    background: none;
    border: none;
    padding: 0;
    margin-top: 2px;
    font-family: inherit;
    font-size: 13px;
    font-weight: 600;
    text-align: left;
    color: var(--accent);
    cursor: pointer;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    width: 100%;
  }
  /* Muted until the item (or the button itself) is hovered/focused — same
     reveal-on-hover recipe as PullRow's `.ext-link`. */
  .ext-link {
    position: absolute;
    top: 2px;
    right: 0;
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
  .stuck-link-wrap:hover .ext-link,
  .ext-link:focus-visible {
    opacity: 1;
  }
  .ext-link:hover {
    background: var(--surface-sunken);
    color: var(--text);
  }
  .stuck-link:hover {
    color: var(--link-hover);
    text-decoration: underline;
  }
</style>
