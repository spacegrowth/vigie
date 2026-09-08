<script lang="ts">
  // Right-hand "read it in-app" pane (docs/CONTRACT.md "Reading in-app"):
  // fetches a full PR/issue conversation or a commit's patch, live, so no
  // row anywhere has to leave the app to be read. "Open on GitHub" here is
  // the ONLY place the opener plugin is used in the whole app.
  //
  // Reader mode (packet 002, "full view is a place you go, not a mode you
  // toggle"): the header swaps the plain "#123 · owner/repo" line for a
  // breadcrumb and adds a previous/next stepper that walks
  // `detailStore.siblings` without leaving reader mode. App.svelte's j/k ·
  // ↑/↓ keys drive the same stepper in either layout.
  //
  // Navigation stack (packet 007, "stop the detail pane throwing away
  // where you came from"): `detailStore` holds a stack of levels rather
  // than one flat target — opening from a list (a feed row, Commits view,
  // a PR row, the tray popover, …) still REPLACES it down to one entry, but
  // opening from *inside* the pane (today, only a commit clicked in a PR's
  // Commits tab — `openCommitItem` below) PUSHES a level on top instead.
  // `target`/`siblings`/`activeTab` are read-only views onto the top of
  // that stack — every existing reader of them, and the stepper above,
  // keeps working unchanged, now scoped to whichever level is current. The
  // breadcrumb (`detailStore.origin` first, then one crumb per level, `›`
  // between) shows the whole chain, kept to the last 3 levels with a
  // leading "…" if deeper; every crumb but the last pops back to that
  // level, the first (origin) crumb is exactly Back as before. Back/Esc pop
  // one level while there's more than one; only once the stack is back to
  // one entry do they fall through to exiting reader mode, then closing —
  // see `handleBack` and App.svelte's `handleKeydown` Escape branch.
  //
  // Every way in and out of full view (this packet, "make full view
  // discoverable and reachable from anywhere in the detail pane"):
  //   In  — the "expand" icon button at the far left of the split-layout
  //         header (this file), double-clicking anywhere in the pane
  //         (`handlePaneDoubleClick` below — header, blank space, a comment
  //         card, a commit row, a file header, a diff line's row/gutter; a
  //         double-click on selectable text, e.g. `.dtxt` or a markdown
  //         paragraph/list item, is left alone so word-selection still
  //         works), double-clicking a feed row or Enter on one (App.svelte),
  //         ⌘Enter (App.svelte's `handleKeydown`), or dragging the
  //         list/pane divider past its 0.85 snap point (App.svelte).
  //   Out — the Back arrow (far left of the reader-layout header, the same
  //         slot the full-view button occupies in split layout — the two
  //         never show together), Esc, or ⌘Enter (App.svelte). A
  //         double-click inside the pane does nothing while already in
  //         reader layout.
  import { tick } from "svelte";
  import { openUrl } from "@tauri-apps/plugin-opener";
  import { api } from "../lib/api";
  import { renderMarkdown } from "../lib/markdown";
  import { absoluteTime, relativeTime } from "../lib/time";
  import { parseDiff } from "../lib/diff";
  import { scroller } from "../lib/scroller";
  import { reposStore, detailStore, watchesStore, sameTarget } from "../lib/stores";
  import { breadcrumbLevels } from "../lib/detail-nav";
  import { detectAgent, parseCoAuthor, splitTrailers, type Trailer } from "../lib/trailers";
  import type { CommitDetail, CommitFile, Thread, ThreadItem } from "../lib/types";
  import Avatar from "./Avatar.svelte";
  import AgentChip from "./AgentChip.svelte";
  import Icon from "./Icon.svelte";

  export type DetailTarget = { kind: "thread"; repoId: number; number: number } | { kind: "commit"; repoId: number; sha: string };

  let { target, onClose }: { target: DetailTarget; onClose: () => void } = $props();

  const { items: repoItems } = reposStore;
  const { items: watchItems } = watchesStore;

  let loading = $state(true);
  let error = $state<string | null>(null);
  let thread = $state<Thread | null>(null);
  let commit = $state<CommitDetail | null>(null);

  let pullFiles = $state<CommitFile[] | null>(null);
  let filesLoading = $state(false);
  let filesError = $state<string | null>(null);
  let watchBusy = $state(false);
  let paneBodyEl: HTMLDivElement | undefined = $state();

  function repoLabel(repoId: number): string {
    const r = $repoItems.find((r) => r.id === repoId);
    return r ? `${r.owner}/${r.name}` : "";
  }

  // Which tab is active lives in `detailStore` now, not local state (packet
  // 007) — it's per navigation-stack level, so popping back to a PR that
  // was left on Commits restores Commits rather than always landing back on
  // Conversation (the exact bug this packet fixes). `load()` below
  // deliberately never resets it; the store already holds the right value
  // for whichever level `target` just became, whether that's a fresh
  // "conversation" (a brand-new open/push) or a restored one (a pop).
  const { activeTab } = detailStore;

  async function load() {
    loading = true;
    error = null;
    thread = null;
    commit = null;
    pullFiles = null;
    filesError = null;
    // This level's own remembered scroll offset — read now (before the
    // await) so it reflects wherever the stack just landed (a pop restores
    // an old value; a fresh push/replace reads back its 0 default), applied
    // once the new content is actually in the DOM, below.
    const restoreScrollTop = detailStore.getScrollTop();
    try {
      if (target.kind === "thread") {
        thread = await api.getThread(target.repoId, target.number);
      } else {
        commit = await api.getCommit(target.repoId, target.sha);
      }
    } catch (e) {
      error = e instanceof Error ? e.message : "Couldn't load this.";
    } finally {
      loading = false;
    }
    await tick();
    if (paneBodyEl) paneBodyEl.scrollTop = restoreScrollTop;
  }

  // Re-fetch whenever the caller points the pane at a different thing —
  // `target` is a plain prop (not `{#key}`-remounted by the parent), so
  // clicking a different row while the pane is already open still reloads.
  $effect(() => {
    target;
    load();
  });

  // The Files tab (docs/CONTRACT.md "Reading in-app") loads lazily — only
  // once it's actually opened, and only once per thread.
  $effect(() => {
    if ($activeTab === "files" && thread?.kind === "pull" && pullFiles === null && !filesLoading) {
      loadPullFiles(thread.repo_id, thread.number);
    }
  });

  /** Best-effort scroll-position capture (packet 007, "the pane's scroll
   * position if that is cheap to capture") — every scroll while this level
   * is on top updates its stack entry, so Back/a breadcrumb click away and
   * later a pop back restores it via `load()`'s `restoreScrollTop` above. */
  function handlePaneBodyScroll() {
    if (paneBodyEl) detailStore.setScrollTop(paneBodyEl.scrollTop);
  }

  async function loadPullFiles(repoId: number, number: number) {
    filesLoading = true;
    filesError = null;
    try {
      pullFiles = await api.getPullFiles(repoId, number);
    } catch (e) {
      filesError = e instanceof Error ? e.message : "Couldn't load the changed files.";
    } finally {
      filesLoading = false;
    }
  }

  /** Whether the open thread is currently watched — read from the shared
   * `watchesStore` rather than `Thread` itself, which carries no `watched`
   * field (docs/CONTRACT.md's `Thread` type). */
  let isWatched = $derived(
    thread != null && $watchItems.some((w) => w.repo_id === thread!.repo_id && w.number === thread!.number),
  );

  async function toggleWatch() {
    if (!thread || watchBusy) return;
    watchBusy = true;
    try {
      if (isWatched) await watchesStore.unwatch(thread.repo_id, thread.number);
      else await watchesStore.watch(thread.repo_id, thread.number);
    } catch {
      // best-effort — the toggle just reflects whatever watchesStore ends up holding
    } finally {
      watchBusy = false;
    }
  }

  /** The Conversation tab excludes `commit` items — those get their own tab
   * for a PR (and an issue never has any, since commits carry no number). */
  let conversationItems = $derived(thread ? thread.items.filter((i) => i.kind !== "commit") : []);
  let commitItems = $derived(thread ? thread.items.filter((i) => i.kind === "commit") : []);

  /** Review comments (item 4, "review comments on the diff line"): the
   * Conversation tab already renders every thread item including these
   * (unchanged, per the packet); the Files tab additionally renders each one
   * a second time, under the diff line it targets. */
  let reviewComments = $derived(thread ? thread.items.filter((i) => i.kind === "review_comment") : []);

  function commentsForFile(path: string): ThreadItem[] {
    return reviewComments.filter((c) => c.path === path);
  }

  /** Comments that can't land under a rendered line: outdated (`line` is
   * null — GitHub clears it once the line the comment targeted is gone from
   * the diff) or on a file this pull's file list doesn't carry at all (e.g.
   * a file dropped from the diff since the comment was made). Only
   * resolvable once `pullFiles` has loaded. */
  let outdatedComments = $derived.by(() => {
    if (!pullFiles) return [];
    const paths = new Set(pullFiles.map((f) => f.path));
    return reviewComments.filter((c) => c.line == null || c.path == null || !paths.has(c.path));
  });

  /** Every commit in this PR's own Commits tab, as detail targets — the
   * siblings `openCommitItem` below pushes alongside the clicked one, so
   * the stepper (packet 007, "the stepper follows the level") walks THIS
   * PR's commits once one is open, rather than sitting disabled the way it
   * used to (a commit opened from inside a thread's Commits tab was never
   * in the *pane's* one-and-only siblings snapshot). */
  let commitSiblings = $derived<DetailTarget[]>(
    thread
      ? commitItems.flatMap((i): DetailTarget[] => (i.sha ? [{ kind: "commit", repoId: thread!.repo_id, sha: i.sha }] : []))
      : [],
  );

  /** The only call site in this file that opens a *different* level of the
   * pane rather than a fresh list row (packet 007, "distinguish replace
   * from push"): a commit clicked from inside a PR's own Commits tab keeps
   * the PR underneath it — `push: true` — rather than replacing the whole
   * pane and losing the PR's Conversation/Commits/Files strip, which is
   * exactly the bug this packet fixes. Every other `detailStore.open*` call
   * site in the app opens from a list and omits `push`, i.e. still
   * replaces. */
  function openCommitItem(sha: string | null) {
    if (!thread || !sha) return;
    detailStore.openCommit(thread.repo_id, sha, { push: true, siblings: commitSiblings });
  }

  function itemLabel(item: ThreadItem): string {
    switch (item.kind) {
      case "review":
        if (item.state === "approved") return "Approved";
        if (item.state === "changes_requested") return "Changes requested";
        return "Reviewed";
      case "review_comment":
        return item.path ? `${item.path}${item.line ? `:${item.line}` : ""}` : "Review comment";
      case "commit":
        return "Commit";
      default:
        return "Comment";
    }
  }

  async function openOnGitHub(url: string) {
    try {
      await openUrl(url);
    } catch {
      // best-effort; nothing sensible to surface inline for a link click
    }
  }

  const { layout, paneWidth, origin, siblings, stack } = detailStore;

  /** The breadcrumb's post-origin chain (packet 007, "the breadcrumb shows
   * the chain, not just the origin") — kept to the last 3 stack levels, a
   * leading "…" for anything deeper. Each level's label folds the repo in
   * only when it differs from the level right before it (so a commit
   * pushed from its own PR reads as a bare sha, matching the PR's repo
   * implicitly, rather than repeating "owner/repo" twice in a row). */
  let crumbInfo = $derived.by(() => {
    const { levels, truncated } = breadcrumbLevels($stack);
    let prevRepoId: number | null = null;
    const labeled = levels.map((level) => {
      const label = crumbLabel(level.target, prevRepoId);
      prevRepoId = level.target.repoId;
      return { ...level, label };
    });
    return { levels: labeled, truncated };
  });

  function crumbLabel(t: DetailTarget, prevRepoId: number | null): string {
    const repo = t.repoId === prevRepoId ? "" : repoLabel(t.repoId);
    if (t.kind === "thread") return repo ? `${repo} #${t.number}` : `#${t.number}`;
    return repo ? `${repo} ${t.sha.slice(0, 7)}` : t.sha.slice(0, 7);
  }

  /** Back button (both layouts) and Esc (App.svelte): pop a level while
   * there's one to pop back to; otherwise fall through to what Back always
   * meant before this packet — exit reader (split layout: a no-op, already
   * there). */
  function handleBack() {
    if (detailStore.canPop()) detailStore.popOne();
    else detailStore.exitReader();
  }

  /** Double-click anywhere in the pane enters full view (this packet, "make
   * full view discoverable and reachable from anywhere in the detail
   * pane") — split layout only; reader layout already fills the width, so a
   * double-click there is a no-op (Back arrow, Esc, ⌘Enter step back out).
   * Skips interactive elements and selectable text the double-click may be
   * aimed at instead (a button, a link, a comment paragraph, a diff line's
   * own text span) so word-selection still works; a diff LINE's row/gutter
   * is deliberately NOT in that skip list — double-clicking it must still
   * enter full view, same as the rest of the pane. */
  function handlePaneDoubleClick(event: MouseEvent) {
    if ($layout !== "split") return;
    const target = event.target as HTMLElement;
    if (target.closest("button, a, input, textarea, select, [contenteditable], .dtxt, .markdown p, .markdown li, pre, code")) return;
    detailStore.enterReader();
  }

  /** Where `target` sits in the current `siblings` snapshot — drives the
   * "3 of 40" counter and which of the previous/next arrows is disabled.
   * `-1` (not found — the frozen snapshot never contained this exact
   * target, e.g. a commit opened from inside a thread's Commits tab) reads
   * the same as "nothing to step to": both arrows end up disabled below. */
  let siblingIndex = $derived($siblings.findIndex((t) => sameTarget(t, target)));
  let hasPrevious = $derived(siblingIndex > 0);
  let hasNext = $derived(siblingIndex !== -1 && siblingIndex < $siblings.length - 1);

  // Co-Authored-By/Claude-Session trailers (lib/trailers.ts) — a commit
  // message's trailing `Key: value` footer, split off so it renders as chips
  // beneath the body instead of raw markdown text.
  let messageSplit = $derived(commit ? splitTrailers(commit.message) : null);
  /** A `*-Session:`/`*-Url:` trailer with an `https://` value (e.g.
   * `Claude-Session:`) — the link target for every agent chip below, and
   * left out of `otherTrailers` since the chip already represents it. */
  let sessionTrailer = $derived.by<Trailer | null>(() => {
    if (!messageSplit) return null;
    return messageSplit.trailers.find((t) => /-(session|url)$/i.test(t.key) && /^https:\/\//i.test(t.value)) ?? null;
  });
  let coAuthorTrailers = $derived(messageSplit ? messageSplit.trailers.filter((t) => /^co-authored-by$/i.test(t.key)) : []);
  let otherTrailers = $derived(
    messageSplit ? messageSplit.trailers.filter((t) => t !== sessionTrailer && !/^co-authored-by$/i.test(t.key)) : [],
  );
</script>

<!-- One patch renderer for both the Files tab (PR/issue diffs) and a
     commit's own file list — same parseDiff + row classes, so the two look
     identical; the diff needed add/remove colouring like git's own. -->
{#snippet diffBlock(patch: string, fileComments?: ThreadItem[])}
  <div class="diff">
    {#each parseDiff(patch) as line, i (i)}
      <div class="dline d-{line.type}">
        <span class="dln">{line.lineNumber ?? ""}</span>
        <span class="dtxt">{line.type === "add" ? "+" : line.type === "del" ? "-" : ""}{line.text}</span>
      </div>
      {#if fileComments && line.newLineNumber != null}
        {#each fileComments.filter((c) => c.line === line.newLineNumber) as c, ci (ci)}
          {@render reviewCommentBlock(c)}
        {/each}
      {/if}
    {/each}
  </div>
{/snippet}

<!-- One review comment, under the diff line it targets (Files tab) or in
     the outdated group at the bottom — same block either way. -->
{#snippet reviewCommentBlock(c: ThreadItem)}
  <div class="review-comment">
    <div class="row rc-head">
      <Avatar login={c.actor_login} avatarUrl={c.actor_avatar_url} size={16} />
      <span class="login">{c.actor_login}</span>
      <span class="muted" title={absoluteTime(c.at)}>{relativeTime(c.at)}</span>
    </div>
    {#if c.body}
      <div class="markdown rc-body">{@html renderMarkdown(c.body)}</div>
    {/if}
  </div>
{/snippet}

<!-- The breadcrumb (packet 002 — "full view is a place you go, not a mode
     you toggle"; extended packet 007 — "the breadcrumb shows the chain, not
     just the origin"): the first crumb is the view the whole pane session
     was opened from (`detailStore.origin`), clicking it is exactly Back.
     Every level pushed on top of that (a commit opened from inside a PR's
     Commits tab, say) gets its own crumb — clickable to pop straight back
     to it, except the last (current) one, kept as plain text. Shown
     whenever there's a chain to show (reader layout always; split layout
     only once something's been pushed, so the common single-level case
     keeps its plain "#123 · owner/repo" line below unchanged). -->
{#snippet breadcrumbNav()}
  <nav class="breadcrumb" aria-label="Location">
    <!-- The origin crumb ("Feed", "Pull requests"…) means "go back out to
         where I came from", so at depth it must first unwind the stack —
         `exitReader()` alone only leaves reader mode, which in split layout
         is already true and left the crumb a hoverable button that did
         nothing while a pushed commit stayed open. -->
    <button
      class="crumb-link"
      onclick={() => {
        detailStore.popToLevel(0);
        detailStore.exitReader();
      }}>{$origin}</button>
    {#if crumbInfo.truncated}
      <span class="crumb-sep">›</span>
      <span class="crumb">…</span>
    {/if}
    {#each crumbInfo.levels as level (level.index)}
      <span class="crumb-sep">›</span>
      {#if level.current}
        <span class="crumb mono">{level.label}</span>
      {:else}
        <button class="crumb-link" onclick={() => detailStore.popToLevel(level.index)}>{level.label}</button>
      {/if}
    {/each}
  </nav>
{/snippet}

<!-- The dblclick-to-full-view gesture below is a redundant convenience on
     top of the fully accessible full-view button and ⌘Enter (both handled
     elsewhere in this markup) — the pane itself stays a plain static
     container, not a widget, so no interactive role belongs on it. -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="pane"
  class:reader={$layout === "reader"}
  style:--pane-width="{$paneWidth * 100}%"
  ondblclick={handlePaneDoubleClick}
>
  <div class="pane-head">
    <div class="pane-head-row">
      {#if $layout === "reader"}
        <button class="icon-btn back-btn" onclick={handleBack} title="Back (Esc)" aria-label="Back">
          <Icon name="back" size={16} />
        </button>
        {@render breadcrumbNav()}
      {:else}
        <!-- Back button (packet 007): only shown once there's a level to
             pop back to — a commit opened from inside a PR's Commits tab,
             say — sharing the far-left slot with the full-view button
             below rather than displacing it. -->
        {#if $stack.length > 1}
          <button class="icon-btn back-btn" onclick={handleBack} title="Back (Esc)" aria-label="Back">
            <Icon name="back" size={16} />
          </button>
        {/if}
        <!-- Full-view button (packet 002, "make full view discoverable
             from anywhere in the detail pane"): far-left slot in split
             layout, the same slot the reader-mode Back arrow occupies
             above — so the two never show together. -->
        <button
          class="icon-btn"
          onclick={() => detailStore.enterReader()}
          title="Full view (double-click or ⌘Enter)"
          aria-label="Full view"
        >
          <Icon name="expand" size={16} />
        </button>
        {#if $stack.length > 1}
          {@render breadcrumbNav()}
        {:else if thread}
          <span class="muted">#{thread.number} · {repoLabel(thread.repo_id)}</span>
        {:else if commit}
          <span class="muted mono">{commit.sha.slice(0, 7)} · {repoLabel(commit.repo_id)}</span>
        {:else}
          <span class="muted">&nbsp;</span>
        {/if}
      {/if}
      <span class="spacer"></span>
      {#if thread}
        <button
          class="watch-btn"
          class:on={isWatched}
          onclick={toggleWatch}
          disabled={watchBusy}
          aria-pressed={isWatched}
        >
          <Icon name={isWatched ? "eye_filled" : "eye"} size={13} />
          {isWatched ? "Watching" : "Watch"}
        </button>
        <button class="btn-text" onclick={() => openOnGitHub(thread!.url)}>Open on GitHub<Icon name="external" size={12} /></button>
      {:else if commit}
        <button class="btn-text" onclick={() => openOnGitHub(commit!.url)}>Open on GitHub<Icon name="external" size={12} /></button>
      {/if}
      {#if $layout === "reader"}
        <!-- Previous/next through the list the item was opened from
             (packet 002): stays in reader mode, App.svelte's j/k · ↑/↓
             go through the same `stepPrevious`/`stepNext`. Split layout
             skips this — the list itself is the navigation there. -->
        <div class="stepper">
          <button
            class="icon-btn"
            onclick={() => detailStore.stepPrevious()}
            disabled={!hasPrevious}
            title="Previous (k or ↑)"
            aria-label="Previous item"
          >
            <Icon name="chevron_up" size={14} />
          </button>
          <span class="muted stepper-count">{siblingIndex + 1} of {$siblings.length}</span>
          <button
            class="icon-btn"
            onclick={() => detailStore.stepNext()}
            disabled={!hasNext}
            title="Next (j or ↓)"
            aria-label="Next item"
          >
            <Icon name="chevron" size={14} />
          </button>
        </div>
      {/if}
      <button class="icon-btn" onclick={onClose} title="Close (Esc)" aria-label="Close">
        <Icon name="close" size={16} />
      </button>
    </div>
    {#if thread}
      <h2 title={thread.title}>{thread.title}</h2>
      {#if thread.kind === "pull"}
        <div class="tabstrip">
          <button class="tab" class:on={$activeTab === "conversation"} onclick={() => detailStore.setTab("conversation")}>Conversation</button>
          <button class="tab" class:on={$activeTab === "commits"} onclick={() => detailStore.setTab("commits")}>Commits</button>
          <button class="tab" class:on={$activeTab === "files"} onclick={() => detailStore.setTab("files")}>Files</button>
        </div>
      {/if}
    {:else if commit}
      <h2 title={commit.message.split("\n")[0]}>{commit.message.split("\n")[0]}</h2>
    {/if}
  </div>

  <div class="pane-body" use:scroller bind:this={paneBodyEl} onscroll={handlePaneBodyScroll}>
    {#if loading}
      <p class="muted">Loading…</p>
    {:else if error}
      <p class="error-text">{error}</p>
    {:else if thread && (thread.kind === "issue" || $activeTab === "conversation")}
      <div class="row author-row">
        <Avatar login={thread.author_login} avatarUrl={thread.author_avatar_url} size={22} />
        <span class="login">{thread.author_login}</span>
        <span class="muted">opened {relativeTime(thread.created_at)}</span>
        <span class="state-badge">{thread.state}</span>
      </div>
      {#if thread.kind === "pull"}
        <div class="pr-meta muted">
          {thread.head_ref} → {thread.base_ref} · +{thread.additions} −{thread.deletions} · {thread.changed_files} files changed
        </div>
      {/if}
      {#if thread.body}
        <div class="markdown">{@html renderMarkdown(thread.body)}</div>
      {/if}

      <div class="conversation">
        {#each conversationItems as item, i (i)}
          <div class="item">
            <div class="row item-head">
              <Avatar login={item.actor_login} avatarUrl={item.actor_avatar_url} size={20} />
              <span class="login">{item.actor_login}</span>
              <span class="item-label">{itemLabel(item)}</span>
              <span class="spacer"></span>
              <span class="muted" title={absoluteTime(item.at)}>{relativeTime(item.at)}</span>
            </div>
            {#if item.body}
              <div class="markdown item-body">{@html renderMarkdown(item.body)}</div>
            {/if}
          </div>
        {/each}
      </div>
    {:else if thread && $activeTab === "commits"}
      <div class="commits-tab">
        {#if commitItems.length === 0}
          <p class="muted">No commits.</p>
        {:else}
          {#each commitItems as item (item.sha)}
            <button class="crow" onclick={() => openCommitItem(item.sha)}>
              <span class="mono sha">{item.sha?.slice(0, 7)}</span>
              <span class="crow-msg">{item.body?.split("\n")[0] ?? ""}</span>
              <Avatar login={item.actor_login} avatarUrl={item.actor_avatar_url} size={20} />
              <span class="muted crow-time" title={absoluteTime(item.at)}>{relativeTime(item.at)}</span>
            </button>
          {/each}
        {/if}
      </div>
    {:else if thread && $activeTab === "files"}
      <div class="files-tab">
        {#if filesLoading}
          <p class="muted">Loading…</p>
        {:else if filesError}
          <p class="error-text">{filesError}</p>
        {:else if pullFiles}
          {#each pullFiles as file (file.path)}
            <div class="pfile">
              <div class="row file-head">
                <span class="mono file-path">{file.path}</span>
                <span class="muted file-status">{file.status}</span>
                <span class="spacer"></span>
                <span class="add-count">+{file.additions}</span>
                <span class="del-count">−{file.deletions}</span>
              </div>
              {#if file.patch}
                {@render diffBlock(file.patch, commentsForFile(file.path))}
              {:else}
                <div class="too-large">
                  <span class="muted">Too large to show here</span>
                  <button class="btn-text" onclick={() => openOnGitHub(thread!.url)}>Open on GitHub<Icon name="external" size={12} /></button>
                </div>
              {/if}
            </div>
          {/each}
          {#if outdatedComments.length > 0}
            <div class="outdated-comments">
              <div class="outdated-label">Comments on lines no longer in this diff</div>
              {#each outdatedComments as c, i (i)}
                {@render reviewCommentBlock(c)}
              {/each}
            </div>
          {/if}
        {/if}
      </div>
    {:else if commit}
      <div class="row author-row">
        <Avatar login={commit.author_login} avatarUrl={commit.author_avatar_url} size={22} />
        <span class="login">{commit.author_login}</span>
        <span class="muted">{relativeTime(commit.at)}</span>
        <span class="muted">+{commit.additions} −{commit.deletions}</span>
      </div>
      <div class="markdown">{@html renderMarkdown(messageSplit?.body ?? commit.message)}</div>

      {#if coAuthorTrailers.length > 0 || otherTrailers.length > 0}
        <!-- Co-Authored-By/Claude-Session trailers, as chips instead of the
             raw footer text this replaces (see `messageSplit` above): one
             AgentChip per co-author that resolves to a known agent, a
             human co-author stays name-only muted text, and any other
             trailer (Signed-off-by, …) reads as `key · value`. -->
        <div class="trailers">
          {#each coAuthorTrailers as t (t.key + t.value)}
            {@const agent = detectAgent(t.value)}
            {#if agent}
              <AgentChip {agent} label={agent.label} url={sessionTrailer?.value ?? null} title={`${t.key}: ${t.value}`} />
            {:else}
              <span class="muted trailer-human">{parseCoAuthor(t.value).name || t.value}</span>
            {/if}
          {/each}
          {#each otherTrailers as t (t.key + t.value)}
            <span class="muted trailer-kv">{t.key} · {t.value}</span>
          {/each}
        </div>
      {/if}

      <div class="files">
        {#each commit.files as file (file.path)}
          <div class="file">
            <div class="row file-head">
              <span class="mono file-path">{file.path}</span>
              <span class="muted file-status">{file.status}</span>
              <span class="spacer"></span>
              <span class="add-count">+{file.additions}</span>
              <span class="del-count">−{file.deletions}</span>
            </div>
            {#if file.patch}
              {@render diffBlock(file.patch)}
            {/if}
          </div>
        {/each}
      </div>
    {/if}
  </div>
</div>

<style>
  .pane {
    /* Set from `detailStore.paneWidth` via the `style:` directive above (a
       CSS custom property) rather than a plain inline style attribute. */
    width: var(--pane-width, 45%);
    flex-shrink: 0;
    border-left: 1px solid var(--border);
    background: var(--surface);
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  /* Reader mode (entered by double-click/Enter on a row, or the divider
     dragged past its far edge — App.svelte) — the feed list is unmounted by
     App.svelte while this is set, not just squeezed by flexbox, so this
     only needs to claim the width. */
  .pane.reader {
    width: 100%;
  }
  /* The reader-mode Back arrow (far left of the header row, in place of the
     old Expand/Collapse text toggle). */
  .back-btn {
    margin-right: 2px;
  }
  /* The reader-mode breadcrumb (packet 002) — same slot the split layout's
     plain "#123 · owner/repo" line occupies, left of the header's action
     buttons and above the `<h2>` title below. */
  .breadcrumb {
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 5px;
    min-width: 0;
    overflow: hidden;
  }
  .crumb-link {
    font-size: 12px;
    color: var(--muted);
    background: none;
    border: none;
    padding: 0;
    cursor: pointer;
    font-family: inherit;
    flex-shrink: 0;
  }
  .crumb-link:hover {
    color: var(--text);
    text-decoration: underline;
  }
  .crumb-sep {
    font-size: 12px;
    color: var(--muted);
    flex-shrink: 0;
  }
  .crumb {
    font-size: 12px;
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* The previous/next stepper (packet 002) — reader mode only, between the
     header's action buttons and the Close button. */
  .stepper {
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 2px;
    flex-shrink: 0;
  }
  .stepper-count {
    font-size: 12px;
    white-space: nowrap;
    padding: 0 2px;
  }
  .pane-head {
    display: flex;
    flex-direction: column;
    gap: 8px;
    /* Right padding gets a few extra px over the left — on the diff view the
       Close button sat too close to the top-right corner, and it is the
       header's last flex item, flush against this edge, so
       this is direct breathing room between it and the window's corner on
       top of the wider default/minimum width above (tauri.conf.json) and
       the truncating "#123 · owner/repo" label below, which stop that
       corner gap from collapsing to ~0 in the first place on a long repo
       name. */
    padding: 14px 22px 14px 18px;
    border-bottom: 1px solid var(--divider);
  }
  .pane-head-row {
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 10px;
  }
  .pane-head h2 {
    font-size: 15px;
    font-weight: 600;
    margin: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }
  /* The plain "#123 · owner/repo" / commit-sha label (split layout, no
     pushed level — the lines right below `{@render breadcrumbNav()}`'s
     call sites above): a long "owner/repo" used to carry an implicit
     `flex-shrink: 0` (the flex default for anything without `min-width: 0`
     — a title never shrinks below its own content width), so a long repo
     name pushed the header's fixed-size items after it — Watch, Open on
     GitHub, and Close — straight past the pane's right edge instead of
     making room. Truncating this label instead, the same way the `<h2>`
     title above already does, is what actually fixes that (a wider
     default window narrows how often it happens, but doesn't stop it —
     see tauri.conf.json's width/height comment).

     `.stepper-count` ("3 of 40") is excluded: it is also `.muted` inside
     this header, and truncating a five-character counter to save room
     would hide the one thing it exists to say. */
  .pane-head .muted:not(.stepper-count) {
    flex-shrink: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .spacer {
    flex-grow: 1;
  }
  .btn-text {
    display: flex;
    align-items: center;
    gap: 4px;
    white-space: nowrap;
  }
  .icon-btn {
    background: none;
    border: none;
    color: var(--muted);
    cursor: pointer;
    padding: 4px;
    display: flex;
    flex-shrink: 0;
    border-radius: 6px;
    transition: background 0.12s, color 0.12s;
  }
  .icon-btn:hover {
    color: var(--text);
    background: var(--surface-active);
  }
  /* The stepper's prev/next buttons (below) are the only `.icon-btn`s that
     ever go `disabled` — at either end of the sibling list. */
  .icon-btn:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .icon-btn:disabled:hover {
    background: none;
  }
  .pane-body {
    padding: 16px 18px 24px 18px;
    overflow-y: auto;
    flex-grow: 1;
  }
  .row {
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 8px;
  }
  .author-row {
    margin-bottom: 4px;
  }
  .login {
    font-size: 13px;
    font-weight: 600;
  }
  .state-badge {
    font-size: 11px;
    text-transform: capitalize;
    padding: 2px 7px;
    border-radius: 4px;
    background: var(--surface-sunken);
    color: var(--label);
  }
  .pr-meta {
    font-family: ui-monospace, Menlo, monospace;
    font-size: 11px;
    margin-bottom: 12px;
  }
  .markdown :global(p) {
    margin: 0 0 10px 0;
    font-size: 13px;
    line-height: 1.5;
  }
  .markdown :global(h1),
  .markdown :global(h2),
  .markdown :global(h3) {
    font-size: 14px;
    margin: 14px 0 6px 0;
  }
  .markdown :global(ul),
  .markdown :global(ol) {
    margin: 0 0 10px 0;
    padding-left: 20px;
    font-size: 13px;
  }
  .markdown :global(blockquote) {
    margin: 0 0 10px 0;
    padding: 2px 0 2px 10px;
    border-left: 2px solid var(--divider);
    color: var(--muted-strong);
  }
  .markdown :global(code) {
    font-family: ui-monospace, Menlo, monospace;
    font-size: 12px;
    background: var(--surface-sunken);
    padding: 1px 4px;
    border-radius: 3px;
  }
  .markdown :global(pre) {
    background: var(--surface-sunken);
    border-radius: 6px;
    padding: 10px;
    overflow-x: auto;
    margin: 0 0 10px 0;
  }
  .markdown :global(pre code) {
    background: none;
    padding: 0;
  }
  .conversation {
    display: flex;
    flex-direction: column;
    gap: 16px;
    margin-top: 18px;
    padding-top: 16px;
    border-top: 1px solid var(--divider);
  }
  .item-head {
    margin-bottom: 4px;
  }
  .item-label {
    font-size: 12px;
    color: var(--label);
  }
  .item-body {
    padding-left: 28px;
  }
  /* The Co-Authored-By/Claude-Session trailers row, beneath a commit's body
     (lib/trailers.ts's `splitTrailers`) — chips and muted key/value text
     wrap together in one row rather than each forcing its own line. */
  .trailers {
    display: flex;
    flex-direction: row;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 10px;
    margin-top: 10px;
  }
  .trailer-human,
  .trailer-kv {
    font-size: 12px;
  }

  .files {
    display: flex;
    flex-direction: column;
    gap: 12px;
    margin-top: 18px;
    padding-top: 16px;
    border-top: 1px solid var(--divider);
  }
  .file-head {
    gap: 10px;
  }
  .file-path {
    font-size: 12px;
    font-weight: 500;
  }
  .add-count {
    font-family: ui-monospace, Menlo, monospace;
    font-size: 11px;
    color: var(--badge-open-fg);
  }
  .del-count {
    font-family: ui-monospace, Menlo, monospace;
    font-size: 11px;
    color: var(--danger);
  }
  .mono {
    font-family: ui-monospace, Menlo, monospace;
  }

  /* Watch/Watching toggle (docs/design/DetailFiles.dc.html's `.watching`
     badge) — PRs and issues both get one, in the header row. */
  .watch-btn {
    display: flex;
    align-items: center;
    gap: 5px;
    font-size: 12px;
    font-weight: 500;
    color: var(--text);
    background: none;
    border: none;
    padding: 3px 10px;
    border-radius: 12px;
    cursor: pointer;
    flex-shrink: 0;
    white-space: nowrap;
    transition: background 0.12s;
  }
  .watch-btn:hover:not(:disabled) {
    background: var(--surface-active);
  }
  .watch-btn.on {
    background: var(--badge-open-bg);
    color: var(--badge-open-fg);
  }
  .watch-btn:disabled {
    opacity: 0.6;
    cursor: default;
  }

  /* Conversation/Commits/Files tab strip — PRs only (docs/CONTRACT.md
     "Reading in-app"); issues keep the plain Conversation view. */
  .tabstrip {
    display: flex;
    flex-direction: row;
    gap: 18px;
    border-bottom: 1px solid var(--divider);
    margin-top: 4px;
  }
  .tab {
    font-size: 13px;
    color: var(--muted);
    background: none;
    border: none;
    border-bottom: 2px solid transparent;
    padding: 0 0 8px 0;
    margin-bottom: -1px;
    cursor: pointer;
    font-family: inherit;
  }
  .tab:hover {
    color: var(--text);
  }
  .tab.on {
    color: var(--text);
    font-weight: 600;
    border-bottom-color: var(--accent);
  }

  .commits-tab {
    display: flex;
    flex-direction: column;
  }
  .crow {
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 10px;
    width: 100%;
    padding: 10px 0;
    border-bottom: 1px solid var(--divider);
    background: none;
    border-left: none;
    border-right: none;
    border-top: none;
    text-align: left;
    cursor: pointer;
    font-family: inherit;
    color: inherit;
    transition: background 0.12s;
  }
  .crow:last-child {
    border-bottom: none;
  }
  .crow:hover {
    background: var(--surface-active);
  }
  .sha {
    font-size: 11px;
    color: var(--muted);
    flex-shrink: 0;
  }
  .crow-msg {
    font-size: 13px;
    flex-grow: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .crow-time {
    width: 40px;
    text-align: right;
    flex-shrink: 0;
  }

  /* The Files tab's per-file diff (docs/CONTRACT.md "Reading in-app"): line
     numbers, additions/deletions tinted, context plain, monospace 12px. */
  .files-tab {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .pfile {
    border: 1px solid var(--divider);
    border-radius: 6px;
    overflow: hidden;
  }
  .pfile .file-head {
    padding: 7px 10px;
    background: var(--surface-sunken);
  }
  .diff {
    font-family: ui-monospace, Menlo, monospace;
    font-size: 12px;
  }
  .dline {
    display: flex;
    flex-direction: row;
    line-height: 1.7;
  }
  .dln {
    width: 34px;
    flex-shrink: 0;
    text-align: right;
    padding-right: 8px;
    color: var(--muted);
    user-select: none;
  }
  .dtxt {
    flex-grow: 1;
    padding-left: 8px;
    white-space: pre;
    overflow-x: auto;
  }
  .d-context {
    background: var(--surface-card);
  }
  .d-add {
    background: var(--diff-add-bg);
  }
  .d-add .dln {
    color: var(--accent);
  }
  .d-add .dtxt {
    color: var(--diff-add-fg);
  }
  .d-del {
    background: var(--diff-del-bg);
  }
  .d-del .dln {
    color: var(--danger);
  }
  .d-del .dtxt {
    color: var(--diff-del-fg);
  }
  .d-hunk {
    color: var(--muted);
  }
  .d-hunk .dtxt {
    padding-left: 8px;
  }
  .too-large {
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 8px;
    padding: 10px;
  }

  /* A review comment rendered under its diff line (item 4) or, for an
     outdated one, in the bottom group — same block either way. Indented to
     the diff line's text column (.dln's 34px + 8px padding, then .dtxt's
     own 8px padding = 50px) and set back to the app's normal proportional
     type, since it sits inside `.diff`'s monospace block. */
  .review-comment {
    margin: 4px 0 4px 50px;
    padding: 6px 0 6px 10px;
    border-left: 3px solid var(--accent);
    font-family: -apple-system, BlinkMacSystemFont, "SF Pro Text", "Helvetica Neue", sans-serif;
  }
  .rc-head {
    gap: 6px;
    margin-bottom: 3px;
  }
  .rc-head .login {
    font-size: 12px;
  }
  .rc-head .muted {
    font-size: 11px;
  }
  .rc-body :global(p) {
    font-size: 12px;
    margin: 0 0 6px 0;
  }
  .outdated-comments {
    margin-top: 4px;
    padding-top: 10px;
    border-top: 1px solid var(--divider);
  }
  .outdated-label {
    font-size: 11px;
    font-weight: 500;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--muted);
    margin-bottom: 4px;
  }
  .outdated-comments .review-comment {
    margin-left: 0;
  }
</style>
