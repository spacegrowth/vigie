<script lang="ts">
  import type { Event } from "../lib/types";
  import { relativeTime, shortRelativeTime, absoluteTime } from "../lib/time";
  import { eventDetailTarget } from "../lib/threads";
  import { detailStore, type DetailOpenOptions, type DetailTarget } from "../lib/stores";
  import { collapsePreview, detectAgent, splitTrailers, type Agent } from "../lib/trailers";
  import Avatar from "./Avatar.svelte";
  import AgentMark from "./AgentMark.svelte";
  import Icon from "./Icon.svelte";
  import KindBadge from "./KindBadge.svelte";

  let {
    event,
    repoLabel,
    titleOverride,
    compact = false,
    selected = false,
    onOpen,
    origin,
    siblings,
    onSeen,
    onFilterActor,
    showUnseenDot = true,
    showPreview = true,
    fullPreview = false,
    iconOnly = false,
  }: {
    event: Event;
    repoLabel?: string;
    /** A short verb phrase shown instead of `event.title` — used for a
     * thread's pr_opened/merged/closed child rows, where the title is
     * already up in the thread header (see lib/threads.ts childActionText). */
    titleOverride?: string | null;
    /** Thread-child rows use the smaller avatar/no-repo-label styling from
     * the mockups' indented child lines, vs. a standalone row's own line. */
    compact?: boolean;
    /** Whether this row's target is the one currently open in the detail
     * pane — draws the same highlight the active toolbar button uses. */
    selected?: boolean;
    /** Override for what a click does with the resolved detail target —
     * the tray popover uses this (its `detailStore` is a *different*
     * webview's memory, so the default "open it in my own store" behavior
     * below would silently do nothing there); every other caller omits
     * this and gets the normal in-window pane. */
    onOpen?: (target: DetailTarget) => void;
    /** The reader-mode breadcrumb's first crumb and the previous/next
     * stepper's list (packet 002's `detailStore.open` options) — only
     * meaningful on the default (non-`onOpen`) path, and only supplied by a
     * top-level standalone row (Feed.svelte); a thread's compact child rows
     * omit both and fall back to the store's own single-item default,
     * since "the list it came from" is the origin view's top-level rows,
     * not a thread's children. */
    origin?: string;
    siblings?: DetailTarget[];
    onSeen?: (id: number) => void;
    onFilterActor?: (login: string) => void;
    /** Whether the unseen-dot column is reserved at all (packet gm-popover-r2,
     * "collapse the whole column when nothing in the list is unseen"). The
     * tray popover passes `false` for every row when its own polled events
     * have nothing unseen, so the list loses the dead 16px gutter every row
     * otherwise reserves for a dot that's never lit; Feed always leaves this
     * at the default since the main window has room to spare. */
    showUnseenDot?: boolean;
    /** Whether the body snippet renders below the header line at all. Every
     * current caller leaves this at the default. */
    showPreview?: boolean;
    /** Full, uncapped description instead of the feed's truncated preview
     * (this packet, "I want the menu to show full desc ... it is still
     * doing ..." — the ellipsis was the complaint). The tray popover passes
     * `true`; the main feed omits it and keeps its one-line, 200-char-capped
     * preview — "one-line rows are right there." See `fullBody`'s own
     * comment for where the untruncated text comes from. Also switches the
     * title to wrap instead of clipping to one line, for the same reason. */
    fullPreview?: boolean;
    /** Symbol instead of word on the kind badge (this packet, "instead of
     * commit label, can that be a symbol ... in the menu bar only"). The
     * tray popover passes `true` and nothing else does — forwarded straight
     * to KindBadge. */
    iconOnly?: boolean;
  } = $props();

  let el: HTMLDivElement | undefined = $state();
  let seenTimer: ReturnType<typeof setTimeout> | undefined;

  // Mark seen after the row has been visible for 2 continuous seconds.
  $effect(() => {
    if (event.seen || !el || !onSeen) return;
    const target = el;
    const observer = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          if (entry.isIntersecting) {
            seenTimer = setTimeout(() => onSeen?.(event.id), 2000);
          } else if (seenTimer) {
            clearTimeout(seenTimer);
            seenTimer = undefined;
          }
        }
      },
      { threshold: 0.6 },
    );
    observer.observe(target);
    return () => {
      observer.disconnect();
      if (seenTimer) clearTimeout(seenTimer);
    };
  });

  // Every row opens the in-app detail pane — nothing here ever navigates to
  // github.com (docs/CONTRACT.md "Reading in-app"; see also the packet's
  // "Reading in-app" section for the deliberate deviation from the
  // mockups, which predate this pane). A single click (or Space) always
  // opens split, as before; double-click or Enter opens straight into
  // reader mode (App.svelte's `.divider`/reader-mode packet) — `onOpen`
  // (the tray popover's override) has no reader-mode equivalent of its own,
  // since it just forwards the target to the main window's default pane.
  function openDetail(reader = false) {
    const target = eventDetailTarget(event);
    if (!target) return;
    if (onOpen) {
      onOpen(target);
      return;
    }
    const opts: DetailOpenOptions = { origin, siblings };
    detailStore.open(target, opts);
    if (reader) detailStore.enterReader();
  }

  // Enter's default button activation would otherwise fire the same click
  // this handles (opening split) before we get a chance to look at it —
  // preventDefault suppresses that synthetic click so only this fires.
  function handleTitleKeydown(e: KeyboardEvent) {
    if (e.key === "Enter") {
      e.preventDefault();
      openDetail(true);
    }
  }

  // One block, one click target (this packet, "the whole row block ... is
  // one click target and one highlight"): the `.title` button above stays
  // for keyboard focus and Enter, but a click/double-click anywhere else in
  // the block — the preview text included — opens the same detail. Skips
  // when the click lands on a button or link (the actor button's own filter
  // click, or the title button itself, which already has its own identical
  // handler above) so nothing double-fires.
  function handleBlockClick(e: MouseEvent) {
    if ((e.target as HTMLElement).closest("button, a")) return;
    openDetail();
  }
  function handleBlockDblClick(e: MouseEvent) {
    if ((e.target as HTMLElement).closest("button, a")) return;
    openDetail(true);
  }

  // Co-Authored-By/Claude-Session trailers (lib/trailers.ts) are a git
  // commit-message convention — a PR/issue/comment body has no equivalent,
  // so this only runs for `commit` events; every other kind's preview below
  // (capped or full) just reads its body straight, no trailer-stripping.
  let commitSplit = $derived.by(() => {
    if (event.kind !== "commit" || !event.body) return null;
    // `event.body` is the full raw commit message (types.ts), title line
    // included (title === its first line) — drop that line first so the
    // preview below doesn't repeat what `.title-text` already shows, same
    // as the server's own `body_preview` (rest-of-message) did.
    const rest = event.body.split("\n").slice(1).join("\n");
    return splitTrailers(rest);
  });
  /** Capped, single-line preview — the feed's row (`fullPreview` false): a
   * commit collapses its post-title, trailer-stripped body to ≤200 chars via
   * `collapsePreview`; every other kind uses the server's own capped
   * `body_preview` as before. */
  let cappedPreview = $derived(commitSplit ? collapsePreview(commitSplit.body) : event.body_preview);
  /** Full, uncapped description — the tray popover only (`fullPreview`
   * true). `listEvents` already returns the complete `body` alongside the
   * capped `body_preview` on every row it hands back (types.ts: "the full
   * markdown body, untruncated") — no second fetch needed to show it. A
   * commit reuses the same trailer-stripped `commitSplit.body` as the
   * capped path above, just without the 200-char cut; every other kind uses
   * `event.body`, falling back to `body_preview` only on the rare row where
   * `body` itself is unset (then the popover really has nothing longer to
   * show, and says so via that value rather than presenting it as "full").
   * This still renders into a plain (non-`pre`) div below, so the browser's
   * own whitespace collapsing already reflows the raw text into a wrapped
   * paragraph — no manual line-collapsing needed here. */
  let fullBody = $derived.by(() => {
    const raw = commitSplit ? commitSplit.body : (event.body ?? event.body_preview);
    return raw?.trim() || null;
  });
  let previewText = $derived(fullPreview ? fullBody : cappedPreview);
  /** Every `Co-Authored-By` trailer that resolves to a known agent, in
   * order — empty on a row with no agent trailer (most rows), which
   * renders no avatar badge at all. The badge shows the first agent's mark;
   * `title` below lists every raw trailer line, for a commit with more than
   * one AI co-author. */
  let rowAgents = $derived.by<Agent[]>(() => {
    if (!commitSplit) return [];
    const agents: Agent[] = [];
    for (const t of commitSplit.trailers) {
      if (!/^co-authored-by$/i.test(t.key)) continue;
      const agent = detectAgent(t.value);
      if (agent) agents.push(agent);
    }
    return agents;
  });
  /** Raw `Co-Authored-By` line(s) — the badge's tooltip text. */
  let rowAgentTitle = $derived(
    commitSplit
      ? commitSplit.trailers
          .filter((t) => /^co-authored-by$/i.test(t.key) && detectAgent(t.value))
          .map((t) => `${t.key}: ${t.value}`)
          .join("\n")
      : "",
  );
</script>

<!-- The block-level click/dblclick below is a redundant convenience over
     the `.title` button's identical, fully keyboard-accessible handlers —
     deliberately plain markup with no role (the title button carries the
     accessible action), per the packet. -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<!-- svelte-ignore a11y_click_events_have_key_events -->
<div
  class="event"
  class:unseen={!event.seen}
  class:watched={event.watched && !compact}
  class:selected
  bind:this={el}
  onclick={handleBlockClick}
  ondblclick={handleBlockDblClick}
>
  <div class="row">
    {#if showUnseenDot}<span class="dot" class:on={!event.seen}></span>{/if}
    <button class="actor" onclick={() => onFilterActor?.(event.actor_login)} title="Filter by {event.actor_login}">
      <span class="avatar-wrap">
        <Avatar login={event.actor_login} avatarUrl={event.actor_avatar_url} size={compact ? 20 : 24} />
        {#if rowAgents.length > 0}
          <span class="agent-badge" title={rowAgentTitle}><AgentMark agent={rowAgents[0]} size={9} /></span>
        {/if}
      </span>
      {#if !compact}<span class="login">{event.actor_login}</span>{/if}
    </button>
    {#if event.watched}
      <span class="watch-eye" title="Watched"><Icon name="eye_filled" size={12} /></span>
    {/if}
    <KindBadge kind={event.kind} {iconOnly} eventTitle={event.title} />
    {#if compact}<span class="login">{event.actor_login}</span>{/if}
    <button
      class="title"
      onclick={() => openDetail()}
      ondblclick={() => openDetail(true)}
      onkeydown={handleTitleKeydown}
      title="Open (double-click or Enter for full width)"
    >
      <span class="title-text" class:strong={!compact} class:wrap={fullPreview}
        >{titleOverride ?? event.title}</span
      >
    </button>
    {#if repoLabel}
      <span class="muted repo" title="{repoLabel}{event.number ? ` #${event.number}` : ''}"
        >{repoLabel}{event.number ? ` #${event.number}` : ""}</span
      >
    {/if}
    <span class="muted time" class:time-compact={compact} title={absoluteTime(event.occurred_at)}
      >{compact ? shortRelativeTime(event.occurred_at) : relativeTime(event.occurred_at)}</span
    >
  </div>
  {#if showPreview && previewText}
    <div class="preview" class:preview-compact={compact}>{previewText}</div>
  {/if}
</div>

<style>
  /* The whole event block — header row plus preview line (this packet,
     "one click target and one highlight"): carries the classes and the
     background that used to live on `.row` alone, so both lines highlight
     and share one rounded corner. */
  .event {
    border-radius: 6px;
    /* A resting row paints NOTHING and simply shows whatever it sits on —
       the feed's own surface, or the tray popover's white panel. It used to
       paint `--surface` unconditionally, which is invisible in the feed and
       reads as pale green stripes on white.
       `--row-bg` still names the colour actually behind the row, because
       the agent badge cuts a disc out of the avatar and has to match it; a
       container that is not `--surface` (the popover) overrides it. */
    --row-bg: var(--surface);
    background: none;
  }
  .event.unseen .title-text.strong,
  .event.unseen .login {
    font-weight: 700;
  }
  /* The event whose target is open in the detail pane (Feed.svelte computes
     `selected` via lib/stores.ts's sameTarget) — same tint as the active
     toolbar button elsewhere in the app. Hover gets the same tint, since the
     whole block is now one click target. */
  .event.selected {
    --row-bg: var(--surface-active);
  }
  .event:hover {
    --row-bg: var(--surface-sunken); /* lighter than .selected so the open row stays distinct under the pointer */
  }
  /* .watched and .watch-eye are shared globals (app.css) — ThreadBlock uses
     the identical recipe on its own header row. A *compact* (thread-child)
     row skips the `watched` class above even when its own event is watched
     — the thread container already draws that bar once, over the header
     and every child; a compact row still gets the eye glyph. */
  .row {
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 10px;
    /* Horizontal padding so the time on the right (and the dot on the left)
       sit inside the row's highlight instead of touching its edge — the
       highlight is drawn on `.event`, which wraps this row, so without it
       "1h" ends up flush against the rounded corner. */
    padding: 9px 10px;
  }
  .dot {
    width: 6px;
    height: 6px;
    border-radius: 3px;
    background: transparent;
    flex-shrink: 0;
  }
  .dot.on {
    background: var(--accent);
  }
  .actor {
    display: flex;
    align-items: center;
    gap: 6px;
    background: none;
    border: none;
    padding: 0;
    cursor: pointer;
    flex-shrink: 0;
    color: inherit;
  }
  /* Wraps the Avatar so the agent badge below can sit absolutely positioned
     over its bottom-right corner without adding to the row's layout width
     or height (this packet, design option B — "the agent mark belongs on
     the avatar, where authorship already lives"). Sized to its content
     (inline-flex, zero line-height) so it never grows past the Avatar it
     wraps. */
  .avatar-wrap {
    position: relative;
    display: inline-flex;
    line-height: 0;
    flex-shrink: 0;
  }
  /* The agent's brand mark, tucked at the avatar's bottom-right corner — a
     `--surface` disc (the row's own background) so it reads as a cut-out
     against the avatar rather than a shape stacked on top of it. Replaces
     the old end-of-row AgentChip entirely; a row with no agent trailer
     renders no badge. */
  .agent-badge {
    position: absolute;
    right: -2px;
    bottom: -2px;
    width: 12px;
    height: 12px;
    border-radius: 50%;
    background: var(--row-bg, var(--surface));
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .login {
    font-size: 13px;
    font-weight: 500;
    max-width: 90px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex-shrink: 0;
  }
  .title {
    display: flex;
    align-items: center;
    gap: 5px;
    background: none;
    border: none;
    padding: 0;
    cursor: pointer;
    color: var(--text);
    /* Wins at least 120px even when `.repo` is still visible and the row is
       narrow (this packet, "the title keeps readable width") — `.repo`
       shrinks first (below), and disappears entirely under 640px. */
    flex: 1 1 40%;
    min-width: 120px;
    text-align: left;
  }
  .title:hover .title-text {
    color: var(--accent);
  }
  .title-text {
    font-size: 13px;
    font-weight: 400;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .title-text.strong {
    font-weight: 600;
  }
  /* `fullPreview` (tray popover) only — the title wraps over as many lines
     as it needs instead of clipping to one, same reasoning as `.preview`
     below: a 400px panel with no per-row scroll shouldn't hide any of the
     title either. `overflow-wrap` guards against a single unbroken token
     (a slug-like title with no spaces) forcing the row wider than its
     container instead of breaking. */
  .title-text.wrap {
    white-space: normal;
    overflow: visible;
    text-overflow: clip;
    overflow-wrap: anywhere;
  }
  /* Shrinks before `.title` gives up any of its 120px floor (this packet's
     responsive columns) — the 200px basis is a ceiling, not a hard width,
     and the whole column disappears under the 640px container query below
     (the repo already shows in the pane header and breadcrumb). */
  .repo {
    flex: 0 1 200px;
    min-width: 72px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    text-align: right;
  }
  .time {
    width: 70px;
    white-space: nowrap;
    text-align: right;
    flex-shrink: 0;
  }
  .time-compact {
    width: 40px;
  }
  .preview {
    font-size: 13px;
    color: var(--muted-strong);
    line-height: 1.4;
    border-left: 2px solid var(--divider);
    padding: 2px 0 8px 10px;
    margin: 0 0 0 32px;
    /* Guards the same single-unbroken-token case as `.title-text.wrap`
       above — more likely to matter here now that `fullPreview` can put an
       entire uncapped body (a bare URL, say) into this line. */
    overflow-wrap: anywhere;
  }
  .preview-compact {
    margin-left: 48px;
  }
  /* Below 640px (Feed.svelte's `.body` establishes the `container-type:
     inline-size` context this measures) there's no room for both a
     readable title and the repo label — the repo already shows in the pane
     header and breadcrumb, so it's the one that goes, not the title. */
  @container (max-width: 640px) {
    .repo {
      display: none;
    }
  }
  /* A genuinely narrow container — the 400px tray popover, not a squeezed
     main window. Without these the row's own minimums (login 90 + title 120
     + repo 72 + time 70, plus gaps) add up to more than the popover can give
     it, the row overflows horizontally, and because a box that scrolls on
     one axis becomes scrollable on the other, a trackpad gesture drags the
     whole list sideways. Shrinking the parts that can afford it keeps the
     row inside its container so there is nothing to drag. */
  @container (max-width: 420px) {
    .login {
      max-width: 64px;
    }
    .title {
      min-width: 84px;
    }
    .time {
      width: 44px;
    }
  }
</style>
