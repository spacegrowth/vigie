<script lang="ts">
  import Icon from "./Icon.svelte";
  import Logo from "./Logo.svelte";
  import AccountSwitcher from "./AccountSwitcher.svelte";
  import type { IconName } from "../lib/icons";
  import type { BadgeCounts } from "../lib/stores";

  const PRONUNCIATION = "vee-ZHEE · French for “lookout”";

  export type ViewName = "feed" | "prs" | "commits" | "comments" | "issues" | "summary" | "people" | "repos" | "teams" | "watched" | "settings";

  let {
    active,
    badges,
    onNavigate,
  }: {
    active: ViewName;
    badges: BadgeCounts;
    // lastPolledAt / pollIntervalSecs / now: App.svelte still passes these
    // (StatusBar.svelte is the poll readout now, not this sidebar's old
    // footer) — accepted but unused, so App.svelte doesn't need a touch
    // just to drop a prop it's already sending.
    lastPolledAt?: number | null;
    pollIntervalSecs?: number;
    now?: number;
    onNavigate: (v: ViewName) => void;
  } = $props();

  const ACTIVITY: { id: ViewName; label: string; icon: IconName; badgeKey?: keyof BadgeCounts }[] = [
    { id: "feed", label: "Feed", icon: "feed", badgeKey: "feed" },
    { id: "prs", label: "Pull requests", icon: "pull_requests", badgeKey: "prs" },
    { id: "commits", label: "Commits", icon: "commits", badgeKey: "commits" },
    { id: "comments", label: "Comments", icon: "comments", badgeKey: "comments" },
    { id: "issues", label: "Issues", icon: "issues", badgeKey: "issues" },
    { id: "summary", label: "Summary", icon: "summary" },
  ];
  const WATCHING: { id: ViewName; label: string; icon: IconName }[] = [
    { id: "people", label: "People", icon: "people" },
    { id: "repos", label: "Repos", icon: "repos" },
    { id: "teams", label: "Teams", icon: "team" },
    { id: "watched", label: "Watched", icon: "eye" },
  ];

  function badgeFor(key?: keyof BadgeCounts): number {
    return key ? badges[key] : 0;
  }
</script>

<nav class="side" data-tauri-drag-region>
  <div class="brand" data-tauri-drag-region title={PRONUNCIATION}>
    <Logo size={30} variant="flat" />
    <div class="brand-text">
      <span class="brand-name">Vigie</span>
      <span class="brand-pron">vee-ZHEE · lookout</span>
    </div>
  </div>

  <div class="group-label">Activity</div>
  {#each ACTIVITY as item (item.id)}
    <button class="nav" class:on={active === item.id} onclick={() => onNavigate(item.id)}>
      <Icon name={item.icon} />
      <span>{item.label}</span>
      {#if badgeFor(item.badgeKey) > 0}
        <span class="count">{badgeFor(item.badgeKey) > 99 ? "99+" : badgeFor(item.badgeKey)}</span>
      {/if}
    </button>
  {/each}

  <div class="group-label">Watching</div>
  {#each WATCHING as item (item.id)}
    <button class="nav" class:on={active === item.id} onclick={() => onNavigate(item.id)}>
      <Icon name={item.icon} />
      <span>{item.label}</span>
    </button>
  {/each}

  <div class="settings-gap"></div>
  <button class="nav" class:on={active === "settings"} onclick={() => onNavigate("settings")}>
    <Icon name="settings" />
    <span>Settings</span>
  </button>

  <AccountSwitcher {onNavigate} />
</nav>

<style>
  .side {
    width: 200px;
    background: var(--surface-sunken);
    border-right: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    padding: 40px 10px 12px 10px; /* top clears the traffic lights (overlay title bar) */
    box-sizing: border-box;
    gap: 2px;
    flex-shrink: 0;
  }
  .brand {
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 8px;
    padding: 0 10px 8px 10px;
    color: var(--text);
  }
  .brand-text {
    display: flex;
    flex-direction: column;
    line-height: 1.3;
    min-width: 0;
  }
  .brand-name {
    font-size: 14px;
    font-weight: 650;
    color: var(--text);
  }
  .brand-pron {
    font-size: 11px;
    font-weight: 400;
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .group-label {
    font-size: 10px;
    font-weight: 500;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--muted);
    padding: 10px 10px 4px 10px;
  }
  .nav {
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 8px;
    padding: 5px 10px;
    border-radius: 6px;
    font-size: 13px;
    color: var(--text);
    background: none;
    border: none;
    cursor: pointer;
    text-align: left;
    font-family: inherit;
    transition: background 0.12s;
  }
  .nav :global(svg) {
    stroke: var(--muted);
  }
  .nav:hover:not(.on) {
    background: var(--surface-active);
  }
  .nav.on {
    background: var(--surface-active);
    font-weight: 500;
  }
  .nav.on :global(svg) {
    stroke: var(--text);
  }
  .count {
    margin-left: auto;
    font-size: 11px;
    color: var(--surface);
    background: var(--accent);
    border-radius: 9px;
    padding: 1px 6px;
    min-width: 8px;
    text-align: center;
  }
  .settings-gap {
    height: 10px;
  }
</style>
