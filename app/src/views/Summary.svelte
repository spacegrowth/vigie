<script lang="ts">
  // Summary v3 (docs/CONTRACT.md "Digests" + "Pulls"): oriented around the
  // three questions a lead actually has — what's stuck (Stuck panel, Aging
  // WIP), what's risky (Worth a careful read, repo concentration), who
  // needs help (the roster/engineer-focus framing rule) — rather than raw
  // counts of things. The old activity-mix bar (kind counts) is gone,
  // replaced by Aging WIP in the same slot. All calendar math (period ->
  // unix start/end, in local time) happens here; the engine only aggregates
  // what's already stored.
  //
  // SHORTCUT (packet gm-scope-r1): unlike every other Activity view, this
  // page does NOT read the app-wide repo filter or "My team / Everyone" mode
  // (lib/feed-filters.ts's `repoFilterStore`/`feedModeStore`) — its two
  // engine calls, `digest` and `open_pulls`, take only `team_id` (one
  // specific team's exact membership) and have no `repo_id` parameter at all
  // and no equivalent of `mode`'s "any team member, or a signed-in account"
  // union (docs/CONTRACT.md "Digests"/"Pulls" — see also engine.rs's
  // `digest`/`open_pulls`, neither of which is in this packet's boundary
  // unless they "genuinely need repo scoping", which reaching this Rust
  // change for `mode` too would be scope creep past a `list_events`-sized
  // fix). Showing the same repo-select/mode-toggle here regardless would
  // narrow nothing behind it — exactly the "control that lies" the packet
  // calls out — so it's left off this page entirely; the `TeamSwitcher`
  // below and its own team-specific `teamFilterStore` scoping are unchanged
  // and unrelated to this gap. Ceiling: Summary can't be scoped by repo, and
  // "Everyone" here always means "no team_id", not the coarser "team you're
  // on, anywhere" `mode: \"team\"` means everywhere else. Upgrade path:
  // extend `gitmon::Engine::digest`/`open_pulls` with `repo_id: Option<u64>`
  // and `mode: FilterMode` params — `store.rs`'s `digest_rows` /
  // `digest_hour_buckets` / `pr_timing_samples` / `unreviewed_merges` already
  // take a generic `logins: Option<&HashSet<String>>`, so `mode`'s
  // "any-team-member-or-signed-in" set (already computed in
  // `Engine::list_events`) can reuse them unchanged; `repo_id` needs one more
  // SQL clause in each of those plus `store.open_pulls`/
  // `open_pull_person_counts`.
  import { onMount, onDestroy } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { api } from "../lib/api";
  import { scroller } from "../lib/scroller";
  import { reposStore, teamFilterStore, teamsStore, detailStore, type DetailTarget } from "../lib/stores";
  import { relativeTime } from "../lib/time";
  import { ALL_EVENT_KINDS } from "../lib/types";
  import type { Digest, OpenPull, PersonSuggestion, PollResult } from "../lib/types";
  import {
    CONCENTRATION_THRESHOLD,
    DEFAULT_ROSTER_SORT,
    computeAttentionRules,
    computeCarefulRead,
    computeKpiTiles,
    formatConcentration,
    groupOpenPulls,
    longestWaitingReviewer,
    oldestAwaitingReview,
    pct,
    previousRange,
    rosterRows,
    sortRoster,
    tzOffsetSecs,
    type RosterSortKey,
  } from "../lib/summary";
  import Avatar from "../components/Avatar.svelte";
  import Icon from "../components/Icon.svelte";
  import TeamSwitcher from "../components/TeamSwitcher.svelte";
  import KpiRow from "../components/summary/KpiRow.svelte";
  import EngineerRoster from "../components/summary/EngineerRoster.svelte";
  import EngineerFocus from "../components/summary/EngineerFocus.svelte";
  import InFlightPulls from "../components/summary/InFlightPulls.svelte";
  import NeedsAttention from "../components/summary/NeedsAttention.svelte";
  import AgingWip from "../components/summary/AgingWip.svelte";
  import StuckPanel from "../components/summary/StuckPanel.svelte";
  import CarefulRead from "../components/summary/CarefulRead.svelte";

  let {
    initialActor = null,
    onOpenPerson,
  }: {
    /** Preselected from the People view's row menu ("Open summary"). */
    initialActor?: string | null;
    onOpenPerson: (login: string) => void;
  } = $props();

  type PeriodKind = "today" | "week" | "month" | "custom";

  let periodKind = $state<PeriodKind>("today");
  /** Periods back from the current one (0). Only meaningful for
   * today/week/month; custom has no notion of stepping. */
  let offset = $state(0);
  let customStart = $state("");
  let customEnd = $state("");
  let selectedActor = $state<string | null>(initialActor);

  function setPeriod(kind: PeriodKind) {
    if (kind !== periodKind) offset = 0;
    periodKind = kind;
  }
  function stepBack() {
    offset -= 1;
  }
  function stepForward() {
    if (offset < 0) offset += 1; // never step into the future
  }

  // --- calendar math (plain Date, local time; no date library) ---

  interface Range {
    start: number;
    end: number;
    label: string;
  }

  function isoDate(d: Date): string {
    const y = d.getFullYear();
    const m = String(d.getMonth() + 1).padStart(2, "0");
    const day = String(d.getDate()).padStart(2, "0");
    return `${y}-${m}-${day}`;
  }
  const todayStr = isoDate(new Date());

  function weekdayShort(d: Date): string {
    return d.toLocaleDateString(undefined, { weekday: "short" });
  }
  function monthShort(d: Date): string {
    return d.toLocaleDateString(undefined, { month: "short" });
  }
  /** Days to subtract from `d` to land on the Monday of its week. */
  function mondayOffset(d: Date): number {
    const day = d.getDay(); // 0 Sun .. 6 Sat
    return day === 0 ? -6 : 1 - day;
  }

  function rangeForToday(off: number): Range {
    const now = new Date();
    const day = new Date(now.getFullYear(), now.getMonth(), now.getDate() + off);
    const next = new Date(day.getFullYear(), day.getMonth(), day.getDate() + 1);
    return {
      start: day.getTime() / 1000,
      end: next.getTime() / 1000,
      label: day.toLocaleDateString(undefined, { weekday: "short", month: "short", day: "numeric" }),
    };
  }

  function rangeForWeek(off: number): Range {
    const now = new Date();
    const monday = new Date(now.getFullYear(), now.getMonth(), now.getDate() + mondayOffset(now) + off * 7);
    const sunday = new Date(monday.getFullYear(), monday.getMonth(), monday.getDate() + 6);
    const next = new Date(monday.getFullYear(), monday.getMonth(), monday.getDate() + 7);
    const sameMonth = monday.getMonth() === sunday.getMonth() && monday.getFullYear() === sunday.getFullYear();
    const label = sameMonth
      ? `${weekdayShort(monday)} ${monday.getDate()} – ${weekdayShort(sunday)} ${sunday.getDate()} ${monthShort(sunday)}`
      : `${weekdayShort(monday)} ${monday.getDate()} ${monthShort(monday)} – ${weekdayShort(sunday)} ${sunday.getDate()} ${monthShort(sunday)}`;
    return { start: monday.getTime() / 1000, end: next.getTime() / 1000, label };
  }

  function rangeForMonth(off: number): Range {
    const now = new Date();
    const first = new Date(now.getFullYear(), now.getMonth() + off, 1);
    const next = new Date(first.getFullYear(), first.getMonth() + 1, 1);
    return { start: first.getTime() / 1000, end: next.getTime() / 1000, label: first.toLocaleDateString(undefined, { month: "long", year: "numeric" }) };
  }

  /** `null` for an incomplete or inverted range — the caller must treat that
   * as "nothing to show yet", never fall back to calling the engine with a
   * guess (the packet's "refused in the UI before any call"). */
  function rangeForCustom(startStr: string, endStr: string): Range | null {
    if (!startStr || !endStr || endStr < startStr) return null;
    const s = new Date(`${startStr}T00:00:00`);
    const e = new Date(`${endStr}T00:00:00`);
    if (Number.isNaN(s.getTime()) || Number.isNaN(e.getTime())) return null;
    const next = new Date(e.getFullYear(), e.getMonth(), e.getDate() + 1);
    const sameMonth = s.getMonth() === e.getMonth() && s.getFullYear() === e.getFullYear();
    const label =
      s.getTime() === e.getTime()
        ? `${weekdayShort(s)} ${s.getDate()} ${monthShort(s)}`
        : sameMonth
          ? `${weekdayShort(s)} ${s.getDate()} – ${weekdayShort(e)} ${e.getDate()} ${monthShort(e)}`
          : `${weekdayShort(s)} ${s.getDate()} ${monthShort(s)} – ${weekdayShort(e)} ${e.getDate()} ${monthShort(e)}`;
    return { start: s.getTime() / 1000, end: next.getTime() / 1000, label };
  }

  let customRangeError = $derived.by(() => {
    if (periodKind !== "custom" || !customStart || !customEnd) return null;
    return customEnd < customStart ? "End date must be on or after the start date." : null;
  });

  let range = $derived.by((): Range | null => {
    if (periodKind === "today") return rangeForToday(offset);
    if (periodKind === "week") return rangeForWeek(offset);
    if (periodKind === "month") return rangeForMonth(offset);
    return rangeForCustom(customStart, customEnd);
  });

  // --- scope: team switcher + optional person ---

  const { selected: teamFilter } = teamFilterStore;
  const { items: teams } = teamsStore;

  /** The selected team's member logins, for the roster's zero-activity rows
   * and the "needs attention" zero-activity rule — `null` under "All
   * teams", where there's no single membership list to diff the digest
   * against (see lib/summary.ts's `zeroActivityLogins`). */
  let teamMemberLogins = $derived.by((): string[] | null => {
    if ($teamFilter == null) return null;
    const t = $teams.find((t) => t.id === $teamFilter);
    return t ? t.logins : null;
  });

  let personQuery = $state("");
  let personResults = $state<PersonSuggestion[]>([]);
  let personSearching = $state(false);
  let showPersonResults = $state(false);

  async function searchPerson(q: string) {
    personSearching = true;
    try {
      // `suggestActivePeople`, not `suggestPeople`: the latter is the
      // Teams "add a member" search, which EXCLUDES anyone already in the
      // team it is given — passing this page's team filter to it hid
      // exactly the people being searched for.
      personResults = await api.suggestActivePeople(q, 8, $teamFilter);
    } catch {
      personResults = [];
    } finally {
      personSearching = false;
    }
  }

  function pickPerson(login: string) {
    selectedActor = login;
    personQuery = "";
    showPersonResults = false;
  }

  // --- loading the digest + open PRs ---
  //
  // `digestData` (current window, current scope) is the one call that gates
  // the whole page, same as before this packet. Everything else here is
  // independent of it and of each other: the previous-window digest (KPI
  // deltas), the scope's open PRs (KPI gauges + roster/focus PR lists), and
  // — only once a person is selected — that person's "Reviewing" list plus
  // a team-wide digest/open-PRs pair for "Needs attention" (always
  // team-wide, regardless of who's selected). Each is fetched and applied
  // on its own so one failing (most likely `open_pulls`, or `digest`'s new
  // args, before the parallel engine packet lands — an "unknown command"
  // rejection) degrades only the section that depends on it, per the
  // packet's error-states requirement.

  let digestData = $state<Digest | null>(null);
  let digestPrevious = $state<Digest | null>(null);
  let loading = $state(true);
  let loadError = $state<string | null>(null);

  /** This scope's (team + selected actor) live open PRs — feeds the KPI
   * row's open/awaiting/stale gauges, the team view's In-flight section,
   * and (once a person is selected) their "In flight" list. `null` before
   * the first successful load or after a failed one (see `pullsScopeError`)
   * — never a fabricated empty list standing in for "unknown". */
  let pullsScope = $state<OpenPull[] | null>(null);
  let pullsScopeError = $state<string | null>(null);

  /** Engineer focus only: open PRs where the selected actor is a requested
   * reviewer. */
  let pullsReviewing = $state<OpenPull[] | null>(null);
  let pullsReviewingError = $state<string | null>(null);

  /** Always team-wide (unfiltered by actor), for "Needs attention" — equal
   * to `digestData`/`pullsScope` when no person is selected (no extra call
   * needed), fetched separately only once an actor narrows those two. */
  let digestTeamWide = $state<Digest | null>(null);
  let pullsTeamWide = $state<OpenPull[] | null>(null);
  let pullsTeamWideError = $state<string | null>(null);

  /** The earliest `occurred_at` Vigie has ever stored, from one capped
   * (500-row) fetch at mount — a proxy for "since Vigie started watching"
   * (the contract's `Repo` carries no added-at timestamp to check against
   * directly). `null` when nothing has ever been stored.
   *
   * SHORTCUT: only the most recent 500 events are inspected (the store's
   * own `MAX_EVENT_LIMIT` clamp), so past ~500 total events on repos old
   * enough that their earliest activity has scrolled off this page, this
   * proxy under-reaches and the empty-period hint below can go missing
   * when it technically should show. Ceiling: ~500 stored events per
   * install. Upgrade path: add a real `added_at` to the contract's `Repo`
   * (crates/gitmon, docs/CONTRACT.md — out of this packet's Boundaries)
   * and compare against `min(repos.added_at)` instead of scanning events. */
  let oldestKnownAt = $state<number | null>(null);

  // Bumped on every call; a response is applied only if its request is still
  // the latest one issued. Fast period/scope switching can otherwise let an
  // older, slower response land after a newer one and overwrite it.
  let requestSeq = 0;

  /** Awaits `p`, never throwing — folds a rejection into the same shape a
   * success has, so a batch of independent calls (`Promise.all` below) can
   * apply each other's outcomes without one failure aborting the rest. */
  async function settle<T>(p: Promise<T>): Promise<{ ok: true; value: T } | { ok: false; error: string }> {
    try {
      return { ok: true, value: await p };
    } catch (e) {
      return { ok: false, error: e instanceof Error ? e.message : "Couldn't load that." };
    }
  }

  async function loadAll() {
    const requestId = ++requestSeq;
    const r = range;
    if (!r) {
      // Custom period incomplete or invalid: refused before any call. This
      // request never went async, so it's still the latest — apply it
      // unconditionally (it also invalidates any older call still in flight).
      digestData = null;
      digestPrevious = null;
      pullsScope = null;
      pullsScopeError = null;
      pullsReviewing = null;
      pullsReviewingError = null;
      digestTeamWide = null;
      pullsTeamWide = null;
      pullsTeamWideError = null;
      loading = false;
      return;
    }

    loading = true;
    loadError = null;
    const tz = tzOffsetSecs();
    const prev = previousRange(r);
    const team = $teamFilter;
    const actor = selectedActor;

    // The core digest gates the page, same as the pre-packet behaviour.
    try {
      const cur = await api.digest(r.start, r.end, team, actor, tz);
      if (requestId !== requestSeq) return; // superseded — drop it
      digestData = cur;
      loading = false;
    } catch (e) {
      if (requestId !== requestSeq) return;
      loadError = e instanceof Error ? e.message : "Couldn't load the summary.";
      digestData = null;
      loading = false;
      return;
    }

    // Everything below is secondary: fetched in parallel, applied
    // independently, never gating the sections above this point.
    const [prevRes, scopeRes, reviewingRes, teamDigestRes, teamPullsRes] = await Promise.all([
      settle(api.digest(prev.start, prev.end, team, actor, tz)),
      settle(api.openPulls(team, actor ?? undefined)),
      actor ? settle(api.openPulls(team, undefined, actor)) : Promise.resolve(null),
      actor ? settle(api.digest(r.start, r.end, team, null, tz)) : Promise.resolve(null),
      actor ? settle(api.openPulls(team)) : Promise.resolve(null),
    ]);
    if (requestId !== requestSeq) return; // superseded — drop it

    digestPrevious = prevRes.ok ? prevRes.value : null;

    if (scopeRes.ok) {
      pullsScope = scopeRes.value;
      pullsScopeError = null;
    } else {
      pullsScope = null;
      pullsScopeError = scopeRes.error;
    }

    if (actor) {
      if (reviewingRes && reviewingRes.ok) {
        pullsReviewing = reviewingRes.value;
        pullsReviewingError = null;
      } else {
        pullsReviewing = null;
        pullsReviewingError = reviewingRes && !reviewingRes.ok ? reviewingRes.error : null;
      }
      digestTeamWide = teamDigestRes && teamDigestRes.ok ? teamDigestRes.value : null;
      if (teamPullsRes && teamPullsRes.ok) {
        pullsTeamWide = teamPullsRes.value;
        pullsTeamWideError = null;
      } else {
        pullsTeamWide = null;
        pullsTeamWideError = teamPullsRes && !teamPullsRes.ok ? teamPullsRes.error : null;
      }
    } else {
      // No actor selected: team-wide *is* the current scope — reuse it
      // rather than issuing the same call twice.
      pullsReviewing = null;
      pullsReviewingError = null;
      digestTeamWide = digestData;
      pullsTeamWide = pullsScope;
      pullsTeamWideError = pullsScopeError;
    }
  }

  $effect(() => {
    range; // re-run whenever the period, offset, or custom dates change
    $teamFilter;
    selectedActor;
    loadAll();
  });

  let unlistenNewEvents: (() => void) | undefined;
  onMount(async () => {
    unlistenNewEvents = await listen<PollResult>("new-events", () => loadAll());
    teamsStore.refresh();
    try {
      const events = await api.listEvents({ limit: 500 });
      oldestKnownAt = events.length > 0 ? Math.min(...events.map((e) => e.occurred_at)) : null;
    } catch {
      oldestKnownAt = null;
    }
  });
  onDestroy(() => unlistenNewEvents?.());

  const { items: repoItems } = reposStore;
  function repoLabel(repoId: number): string {
    const r = $repoItems.find((r) => r.id === repoId);
    return r ? `${r.owner}/${r.name}` : "";
  }

  // --- KPI row (packet §2) ---

  let kpiTiles = $derived.by(() => {
    const r = range;
    if (!digestData || !r) return [];
    return computeKpiTiles({
      current: digestData,
      previous: digestPrevious,
      pullsNow: pullsScope,
      previousWindowEnd: r.start,
      now: Date.now() / 1000,
    });
  });

  // --- engineer roster (packet §3) ---

  let rosterSortKey = $state<RosterSortKey>(DEFAULT_ROSTER_SORT.key);
  let rosterSortDir = $state<"asc" | "desc">(DEFAULT_ROSTER_SORT.dir);

  function onRosterSort(key: RosterSortKey) {
    if (rosterSortKey === key) {
      rosterSortDir = rosterSortDir === "asc" ? "desc" : "asc";
    } else {
      rosterSortKey = key;
      rosterSortDir = key === "login" ? "asc" : "desc";
    }
  }

  let rosterSorted = $derived.by(() => {
    if (!digestData) return [];
    return sortRoster(rosterRows(digestData.people, teamMemberLogins), rosterSortKey, rosterSortDir);
  });

  // --- in-flight PRs, team view (packet §5) ---

  let inFlightGroups = $derived.by(() => (pullsScope ? groupOpenPulls(pullsScope) : []));

  // --- needs attention (packet §6), always team-wide ---

  let attentionRules = $derived.by(() => {
    if (!digestTeamWide) return [];
    return computeAttentionRules({
      pulls: pullsTeamWide ?? [],
      digestTeamWide,
      teamMemberLogins,
      now: Date.now() / 1000,
    });
  });

  // --- Most discussed (packet §A3 — top 5 of Digest.threads by events,
  // which already comes back sorted that way; no engine work needed) ---

  let mostDiscussed = $derived(digestData ? digestData.threads.slice(0, 5) : []);
  let maxThreadEvents = $derived.by(() => mostDiscussed.reduce((m, t) => Math.max(m, t.events), 0));
  let maxRepoTotal = $derived.by(() => (digestData ? digestData.repos.reduce((m, r) => Math.max(m, r.total), 0) : 0));

  /** The reader-mode stepper's list (packet 002): the Most discussed
   * section's own rendered order (its full top-10, not just the 5 shown —
   * stepping "next" from an off-screen thread should still work). */
  let threadSiblings = $derived(
    digestData ? digestData.threads.map((t): DetailTarget => ({ kind: "thread", repoId: t.repo_id, number: t.number })) : [],
  );

  let isEmptyPeriod = $derived.by(() => {
    if (!digestData) return false;
    const d = digestData;
    return ALL_EVENT_KINDS.every((k) => d.totals[k] === 0);
  });

  // Shown above Most discussed when the window is empty: the period asked
  // for predates anything Vigie has ever recorded, so the silence isn't
  // news, it's the start of the record.
  let periodPredatesKnownActivity = $derived(range != null && (oldestKnownAt == null || range.start < oldestKnownAt));

  // --- Aging WIP (packet §A1, replaces the old activity-mix bar) ---

  let agingPulls = $derived(pullsScope ?? []);

  // --- Stuck panel (packet §A2) — always team-wide, like Needs attention ---

  let stuckOldest = $derived.by(() =>
    pullsTeamWide ? oldestAwaitingReview(pullsTeamWide, Date.now() / 1000) : { oldest: null, excludedAbandonedCount: 0 },
  );
  let stuckReviewerWait = $derived.by(() => (pullsTeamWide ? longestWaitingReviewer(pullsTeamWide, Date.now() / 1000) : null));

  // --- Worth a careful read (packet §A4) — same scope as the roster/focus
  // PR lists (team-wide, or one person's once selected). ---

  let carefulRead = $derived.by(() => computeCarefulRead(pullsScope ?? [], digestData?.threads ?? [], Date.now() / 1000));
</script>

<div class="head">
  <h1>Summary</h1>
  <span class="spacer"></span>
</div>

<TeamSwitcher />

{#if $repoItems.length > 1}
  <p class="muted repo-scope-note">
    Summary isn't scoped by the repo filter yet — it always covers every repo in {$teamFilter != null
      ? "the selected team"
      : "your scope"}.
  </p>
{/if}

<div class="controls">
  <div class="segmented">
    <button class:on={periodKind === "today"} onclick={() => setPeriod("today")}>Today</button>
    <button class:on={periodKind === "week"} onclick={() => setPeriod("week")}>This week</button>
    <button class:on={periodKind === "month"} onclick={() => setPeriod("month")}>This month</button>
    <button class:on={periodKind === "custom"} onclick={() => setPeriod("custom")}>Custom</button>
  </div>

  {#if periodKind !== "custom"}
    <div class="period-nav">
      <button class="step" onclick={stepBack} aria-label="Previous period">‹</button>
      <span class="range-label">{range?.label ?? ""}</span>
      <button class="step" onclick={stepForward} disabled={offset === 0} aria-label="Next period">›</button>
    </div>
  {:else}
    <div class="custom-range">
      <input type="date" bind:value={customStart} max={todayStr} aria-label="Start date" />
      <span class="muted">to</span>
      <input type="date" bind:value={customEnd} max={todayStr} aria-label="End date" />
      {#if customRangeError}<span class="error-text">{customRangeError}</span>{/if}
    </div>
  {/if}
</div>

<div
  class="person-filter"
  onfocusout={(e) => {
    const wrap = e.currentTarget as HTMLElement;
    if (!wrap.contains(e.relatedTarget as Node | null)) showPersonResults = false;
  }}
>
  {#if selectedActor}
    <span class="chip actor-chip">
      <Avatar login={selectedActor} size={18} />
      {selectedActor}
      <button class="chip-x" onclick={() => (selectedActor = null)} aria-label="Clear person filter">×</button>
    </span>
    <button class="btn-text" onclick={() => selectedActor && onOpenPerson(selectedActor)}>View in Feed</button>
  {:else}
    <input
      class="person-input"
      placeholder="Filter by person"
      bind:value={personQuery}
      onfocus={() => {
        showPersonResults = true;
        searchPerson(personQuery);
      }}
      oninput={() => {
        showPersonResults = true;
        searchPerson(personQuery);
      }}
    />
    {#if showPersonResults}
      <div class="dropdown" use:scroller>
        {#each personResults as r (r.login)}
          <button class="result-row" onclick={() => pickPerson(r.login)}>
            <Avatar login={r.login} avatarUrl={r.avatar_url} size={20} />
            <span class="login">{r.login}</span>
          </button>
        {:else}
          {#if !personSearching}<div class="empty muted">No matches.</div>{/if}
        {/each}
      </div>
    {/if}
  {/if}
</div>

<div class="body" use:scroller>
  {#if loading}
    <p class="muted">Loading…</p>
  {:else if periodKind === "custom" && !range}
    <p class="muted">{customRangeError ?? "Pick a start and end date to see a summary."}</p>
  {:else if loadError}
    <p class="error-text">{loadError}</p>
  {:else if !digestData}
    <p class="muted">Loading…</p>
  {:else}
    <KpiRow tiles={kpiTiles} />

    {#if selectedActor}
      <div class="focus-header">
        <Avatar login={selectedActor} size={32} />
        <span class="focus-login">{selectedActor}</span>
      </div>
      <EngineerFocus
        series={digestData.series}
        inFlight={pullsScope}
        inFlightError={pullsScopeError}
        reviewing={pullsReviewing}
        reviewingError={pullsReviewingError}
        {repoLabel}
      />
    {:else}
      <StuckPanel
        unreviewedMerges={digestTeamWide?.unreviewed_merges ?? 0}
        p90TtfrSecs={digestTeamWide?.pr_timing.p90_ttfr_secs ?? null}
        oldest={stuckOldest.oldest}
        excludedAbandonedCount={stuckOldest.excludedAbandonedCount}
        reviewerWait={stuckReviewerWait}
        pullsError={pullsTeamWideError}
        {repoLabel}
      />

      <EngineerRoster
        rows={rosterSorted}
        sortKey={rosterSortKey}
        sortDir={rosterSortDir}
        teamSelected={$teamFilter != null}
        onSort={onRosterSort}
        onSelect={(login) => (selectedActor = login)}
      />

      <div class="section-label">In-flight PRs · {pullsScope?.length ?? "—"}</div>
      {#if pullsScopeError}
        <p class="error-text">{pullsScopeError}</p>
      {:else if pullsScope === null}
        <p class="muted">Loading…</p>
      {:else}
        <InFlightPulls groups={inFlightGroups} total={pullsScope.length} {repoLabel} />
      {/if}
    {/if}

    <!-- Aging WIP (packet §A1): replaces the old activity-mix bar in this
         same slot — open PRs bucketed by age, clickable, with their own
         filtered list right below (self-contained, so its position on the
         page doesn't matter for "the list below"). -->
    <AgingWip pulls={agingPulls} now={Date.now() / 1000} {repoLabel} />

    {#if isEmptyPeriod}
      <div class="zero-state">
        <p>Nothing recorded for this period.</p>
        {#if periodPredatesKnownActivity}
          <p class="muted">Vigie only knows about activity since it started watching.</p>
        {/if}
      </div>
    {/if}

    <div class="section-label">Most discussed · {mostDiscussed.length}</div>
    {#if mostDiscussed.length === 0}
      <p class="muted">No pull request or issue activity in this window.</p>
    {:else}
      <div class="rows">
        {#each mostDiscussed as t (`${t.repo_id}:${t.number}`)}
          <button class="thread-row" onclick={() => detailStore.openThread(t.repo_id, t.number, { origin: "Summary", siblings: threadSiblings })}>
            <Icon name={t.kind === "pull" ? "pull_requests" : "issues"} size={16} />
            <span class="ref">{repoLabel(t.repo_id)}#{t.number}</span>
            <span class="thread-title">{t.title}</span>
            <span class="muted">{t.events} event{t.events === 1 ? "" : "s"}</span>
            <span class="rank-bar" role="img" aria-label={`${t.events} of ${maxThreadEvents} events on the top-thread scale`}>
              <span class="rank-fill" style:--w="{pct(t.events, maxThreadEvents)}%"></span>
            </span>
            <span class="muted last-active">last active {relativeTime(t.last_at)}</span>
          </button>
        {/each}
      </div>
    {/if}

    <CarefulRead items={carefulRead.items} usedCommentRounds={carefulRead.usedCommentRounds} {repoLabel} />

    <div class="section-label">Repos · {digestData.repos.length}</div>
    {#if digestData.repos.length === 0}
      <p class="muted">No repo activity in this window.</p>
    {:else}
      <div class="rows">
        {#each digestData.repos as r (r.repo_id)}
          <div class="repo-row">
            <span class="repo-name">{repoLabel(r.repo_id)}</span>
            {#if r.top_author_login && r.top_author_share > CONCENTRATION_THRESHOLD}
              <span class="muted concentration" title={`${r.top_author_login}: ${formatConcentration(r.top_author_share)} of this repo's commits this window`}>
                {formatConcentration(r.top_author_share)}
              </span>
            {/if}
            <span class="muted">{r.total} total</span>
            <span class="rank-bar" role="img" aria-label={`${r.total} of ${maxRepoTotal} events on the top-repo scale`}>
              <span class="rank-fill" style:--w="{pct(r.total, maxRepoTotal)}%"></span>
            </span>
          </div>
        {/each}
      </div>
    {/if}

    {#if digestTeamWide}
      <NeedsAttention rules={attentionRules} pullsError={pullsTeamWideError} />
    {:else}
      <div class="section-label">Needs attention</div>
      <p class="error-text">{pullsTeamWideError ?? "Couldn't load the team's open PRs."}</p>
    {/if}
  {/if}
</div>

<style>
  .head {
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 12px;
    padding: 18px 24px 12px 24px;
  }
  .head h1 {
    font-size: 17px;
    font-weight: 600;
    margin: 0;
  }
  .spacer {
    flex-grow: 1;
  }
  .repo-scope-note {
    padding: 0 24px 10px 24px;
    font-size: 11px;
    line-height: 1.4;
  }
  .controls {
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 14px;
    padding: 0 24px 10px 24px;
    flex-wrap: wrap;
  }
  .segmented {
    display: flex;
    flex-direction: row;
    border: 1px solid var(--border);
    border-radius: 7px;
    overflow: hidden;
  }
  .segmented button {
    font-size: 12px;
    padding: 5px 12px;
    border: none;
    border-right: 1px solid var(--border);
    background: none;
    color: var(--text);
    cursor: pointer;
    font-family: inherit;
    transition: background 0.12s;
  }
  .segmented button:last-child {
    border-right: none;
  }
  .segmented button:hover:not(.on) {
    background: var(--surface-active);
  }
  .segmented button.on {
    background: var(--accent);
    color: var(--surface);
    font-weight: 500;
  }
  .period-nav {
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 8px;
  }
  .step {
    background: none;
    border: 1px solid var(--border);
    color: var(--text);
    width: 24px;
    height: 24px;
    border-radius: 6px;
    cursor: pointer;
    font-size: 13px;
    line-height: 1;
    font-family: inherit;
    transition: background 0.12s;
  }
  .step:hover:not(:disabled) {
    background: var(--surface-active);
  }
  .step:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .range-label {
    font-size: 13px;
    font-weight: 500;
    min-width: 160px;
    text-align: center;
  }
  .custom-range {
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 8px;
  }
  .person-filter {
    position: relative;
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 0 24px 12px 24px;
  }
  .person-input {
    width: 220px;
  }
  .actor-chip {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 12px;
    padding: 4px 6px 4px 8px;
    border-radius: 12px;
    background: var(--surface-active);
  }
  .chip-x {
    background: none;
    border: none;
    color: var(--muted);
    cursor: pointer;
    font-size: 13px;
    line-height: 1;
    padding: 0 2px;
    font-family: inherit;
  }
  .chip-x:hover {
    color: var(--text);
  }
  .dropdown {
    position: absolute;
    top: 100%;
    left: 24px;
    z-index: 1;
    display: flex;
    flex-direction: column;
    min-width: 220px;
    max-height: 260px;
    overflow-y: auto;
    padding: 4px;
    background: var(--surface-card);
    border: 1px solid var(--border-card);
    border-radius: 8px;
    box-shadow: 0 4px 16px rgb(0 0 0 / 0.12);
  }
  .result-row {
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 8px;
    text-align: left;
    background: none;
    border: none;
    padding: 6px 8px;
    border-radius: 5px;
    cursor: pointer;
    font-family: inherit;
    color: inherit;
    font-size: 12px;
  }
  .result-row:hover {
    background: var(--surface-active);
  }
  .empty {
    padding: 6px 8px;
  }
  .body {
    padding: 0 24px 24px 24px;
    overflow-y: auto;
    flex-grow: 1;
  }
  .section-label {
    padding: 14px 0 8px 0;
    font-weight: 500;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    font-size: 11px;
    color: var(--muted);
  }
  .focus-header {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 14px 0 4px 0;
  }
  .focus-login {
    font-size: 16px;
    font-weight: 600;
  }
  /* Ranking-bar primitive, shared by the Most-discussed and Repos rows.
     Widths come in as a CSS custom property via a Svelte style: directive,
     never a literal inline style attribute (the app's inline-style budget). */
  .rank-bar {
    flex-shrink: 0;
    width: 72px;
    height: 4px;
  }
  .rank-fill {
    display: block;
    height: 100%;
    width: var(--w, 0%);
    background: var(--accent);
    border-radius: 2px;
  }
  .rows {
    display: flex;
    flex-direction: column;
  }
  .thread-row {
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 10px;
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
    width: 100%;
    transition: background 0.12s;
  }
  .thread-row:hover {
    background: var(--surface-active);
  }
  .login {
    font-size: 13px;
    font-weight: 600;
    flex-shrink: 0;
  }
  .ref {
    font-size: 12px;
    color: var(--muted);
    flex-shrink: 0;
  }
  .thread-title {
    font-size: 13px;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex-grow: 1;
    min-width: 0; /* let the ellipsis win over the row's fixed-width bars */
  }
  .last-active {
    flex-shrink: 0;
    width: 96px;
    text-align: right;
  }
  .repo-row {
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 10px;
    padding: 10px 0;
    border-bottom: 1px solid var(--divider);
  }
  .repo-name {
    font-size: 13px;
    font-weight: 500;
    flex-grow: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0; /* let the ellipsis win over the row's fixed-width rank bar */
  }
  .zero-state {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 6px;
    padding: 40px 0;
  }
  /* Concentration hint (packet §A5) — a quiet aside, not a metric to chase,
     so it stays plain text rather than a badge. */
  .concentration {
    flex-shrink: 0;
    font-size: 12px;
  }
</style>
