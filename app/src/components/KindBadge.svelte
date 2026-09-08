<script lang="ts">
  import type { EventKind } from "../lib/types";
  import Icon from "./Icon.svelte";
  import type { IconName } from "../lib/icons";

  let {
    kind,
    iconOnly = false,
    eventTitle,
  }: {
    kind: EventKind;
    /** Symbol instead of word (this packet, "instead of commit label, can
     * that be a symbol ... in the menu bar only"): swaps the visible label
     * text for an `Icon`, keeping the same coloured, tinted pill — the
     * colour is still doing real work, only the text goes. The tray
     * popover is the only caller that passes this; Comments/People/Issues
     * and the main feed (via EventRow's own default) omit it and keep the
     * word. */
    iconOnly?: boolean;
    /** The event's own `title` — only read when `kind` is `pr_reviewed` and
     * `iconOnly` is set, to pick which of the three review glyphs applies
     * (mock_engine.rs's `review_state_from_title` and lib/threads.ts's own
     * `e.title.includes("approved")` check use the same convention: a
     * review's title is always "Review: approved" / "Review: changes
     * requested" / "Review: commented", since `pr_reviewed` carries no
     * separate state field of its own). Omitted by every label-mode caller,
     * which has no use for it. */
    eventTitle?: string;
  } = $props();

  // Colour (as CSS custom properties, per the bloat budget's "use nothing
  // else for colour") + short label + default icon per kind, so a badge
  // reads without a legend either way. Matches docs/design/*.dc.html
  // exactly for every kind the mockups show; `pr_closed` has no mockup
  // example, so it reuses the danger token rather than inventing an
  // unreviewed colour. `pr_reviewed`'s `icon` here is just the record's
  // required fallback (a label-mode badge never reads it) — `icon` below
  // picks one of three review glyphs from `eventTitle` instead.
  const META: Record<EventKind, { label: string; bg: string; fg: string; icon: IconName }> = {
    commit: { label: "Commit", bg: "var(--badge-commit-bg)", fg: "var(--badge-commit-fg)", icon: "commits" },
    pr_opened: { label: "PR opened", bg: "var(--badge-open-bg)", fg: "var(--badge-open-fg)", icon: "pull_requests" },
    pr_merged: { label: "Merged", bg: "var(--badge-merged-bg)", fg: "var(--badge-merged-fg)", icon: "pr_merged" },
    pr_closed: { label: "PR closed", bg: "var(--badge-closed-bg)", fg: "var(--badge-closed-fg)", icon: "pr_closed" },
    pr_reviewed: { label: "Review", bg: "var(--badge-review-bg)", fg: "var(--badge-review-fg)", icon: "review_commented" },
    pr_commented: { label: "Comment", bg: "var(--badge-comment-bg)", fg: "var(--badge-comment-fg)", icon: "comments" },
    issue_opened: { label: "Issue", bg: "var(--badge-issue-bg)", fg: "var(--badge-issue-fg)", icon: "issues" },
    issue_commented: { label: "Issue comment", bg: "var(--badge-comment-bg)", fg: "var(--badge-comment-fg)", icon: "comments" },
  };

  let meta = $derived(META[kind]);
  let icon = $derived.by((): IconName => {
    if (kind !== "pr_reviewed") return meta.icon;
    const title = eventTitle ?? "";
    if (title.includes("approved")) return "review_approved";
    if (title.includes("changes requested")) return "review_changes";
    return "review_commented";
  });
</script>

<!-- `title`/`aria-label` carry the full label in both modes (this packet,
     "every badge keeps its full label as title and aria-label") — in
     icon-only mode that's the only place the word still exists. -->
<span class="badge" class:icon-only={iconOnly} style="background:{meta.bg};color:{meta.fg}" title={meta.label} aria-label={meta.label}>
  {#if iconOnly}
    <Icon name={icon} size={13} />
  {:else}
    {meta.label}
  {/if}
</span>

<style>
  .badge {
    font-size: 11px;
    padding: 2px 7px;
    border-radius: 4px;
    white-space: nowrap;
    font-weight: 500;
  }
  /* Same pill, sized to the glyph instead of a word — square-ish and
     centered rather than text-shaped padding. */
  .badge.icon-only {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 3px;
    border-radius: 5px;
  }
</style>
