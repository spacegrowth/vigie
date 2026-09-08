<script lang="ts">
  // The engineer roster table (packet §3): default view when no person is
  // selected. One row per `DigestPerson`, plus a zero-activity row per team
  // member the digest never mentioned (when a specific team is selected —
  // "All teams" has no single membership list to diff against, so that
  // extra set of rows just doesn't apply there; see the note below the
  // header instead of silently going quiet about it).
  import { relativeTime } from "../../lib/time";
  import { type RosterRow, type RosterSortKey } from "../../lib/summary";
  import Avatar from "../Avatar.svelte";
  import Icon from "../Icon.svelte";

  let {
    rows,
    sortKey,
    sortDir,
    teamSelected,
    onSort,
    onSelect,
  }: {
    rows: RosterRow[];
    sortKey: RosterSortKey;
    sortDir: "asc" | "desc";
    /** Whether a specific team (not "All teams") is selected — gates the
     * zero-activity rows and the note explaining their absence. */
    teamSelected: boolean;
    onSort: (key: RosterSortKey) => void;
    onSelect: (login: string) => void;
  } = $props();

  const COLUMNS: { key: RosterSortKey; label: string; align?: "right" }[] = [
    { key: "login", label: "Engineer" },
    { key: "open_prs", label: "Open PRs", align: "right" },
    { key: "review_queue", label: "Review queue", align: "right" },
    { key: "merged", label: "Merged", align: "right" },
    { key: "reviews_given", label: "Reviews given", align: "right" },
    { key: "comments", label: "Comments", align: "right" },
    { key: "commits", label: "Commits", align: "right" },
    { key: "last_at", label: "Last active", align: "right" },
  ];

  function headerClick(key: RosterSortKey) {
    onSort(key);
  }
</script>

<div class="section-label">Engineer roster · {rows.length}</div>
{#if !teamSelected}
  <p class="muted note">Select a team to also list members with zero activity this window.</p>
{/if}

{#if rows.length === 0}
  <p class="muted">No one to show for this window.</p>
{:else}
  <div class="table-scroll">
    <table>
      <thead>
        <tr>
          {#each COLUMNS as col (col.key)}
            <th class:right={col.align === "right"}>
              <button class="th-btn" onclick={() => headerClick(col.key)}>
                {col.label}
                {#if sortKey === col.key}
                  <Icon name={sortDir === "asc" ? "chevron_up" : "chevron"} size={11} />
                {/if}
              </button>
            </th>
          {/each}
          <!-- Activity column removed (gm-sumpolish-r1 §2): the engine has
               no per-person series yet, only a team-wide one, so every row
               drew the exact same grey sparkline. Bring it back once
               `Digest` exposes a per-person series — see Sparkline.svelte. -->
        </tr>
      </thead>
      <tbody>
        {#each rows as row (row.login)}
          <tr class="row" class:zero={row.zeroActivity} onclick={() => onSelect(row.login)}>
            <td>
              <span class="who">
                <Avatar login={row.login} avatarUrl={row.avatar_url} size={24} />
                <span class="login">{row.login}</span>
                {#if row.zeroActivity}<span class="muted zero-tag">no activity</span>{/if}
              </span>
            </td>
            <td class="right num">{row.open_prs}</td>
            <td class="right num">{row.review_queue}</td>
            <td class="right num">{row.merged}</td>
            <td class="right num">{row.reviews_given}</td>
            <td class="right num">{row.comments}</td>
            <td class="right num">{row.commits}</td>
            <td class="right muted">{row.last_at ? relativeTime(row.last_at) : "never"}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
{/if}

<style>
  .note {
    margin: -2px 0 8px 0;
  }
  .table-scroll {
    overflow-x: auto;
  }
  table {
    width: 100%;
    border-collapse: collapse;
    font-size: 13px;
  }
  th {
    text-align: left;
    padding: 10px 20px 10px 0;
    border-bottom: 1px solid var(--border);
  }
  th.right {
    text-align: right;
  }
  .th-btn {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    background: none;
    border: none;
    font-family: inherit;
    font-size: 11px;
    font-weight: 500;
    text-transform: uppercase;
    letter-spacing: 0.03em;
    color: var(--label);
    cursor: pointer;
    padding: 0;
  }
  th.right .th-btn {
    flex-direction: row-reverse;
  }
  .row {
    cursor: pointer;
    transition: background 0.12s;
  }
  .row:hover {
    background: var(--surface-active);
  }
  .row.zero {
    opacity: 0.7;
  }
  td {
    /* gm-sumpolish-r1 §1: cramped before (8px/10px) — more vertical padding
       for a roomier row height, more horizontal padding as the gutter
       between numeric columns so they read as separate figures, not one
       smear of digits. */
    padding: 14px 20px 14px 0;
    border-bottom: 1px solid var(--divider);
  }
  td.right {
    text-align: right;
  }
  .num {
    font-variant-numeric: tabular-nums;
  }
  .who {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .login {
    font-weight: 600;
  }
  .zero-tag {
    font-size: 11px;
  }
</style>
