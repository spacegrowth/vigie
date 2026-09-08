<script lang="ts">
  // The AI-agent brand glyph itself, in the agent's colour — a bundled
  // simple-icons path (lib/agent-icons.ts) at a given `size`, or, for an
  // agent with no bundled icon (`Agent.icon` unset), the family's first
  // letter at the same size. Pure presentational: no link, no title/tooltip
  // of its own — callers (the feed-row avatar badge, the detail-pane chip)
  // own that.
  import { AGENT_ICONS } from "../lib/agent-icons";
  import type { Agent } from "../lib/trailers";

  let { agent, size = 9 }: { agent: Agent; size?: number } = $props();

  let icon = $derived(agent.icon ? AGENT_ICONS[agent.icon] : undefined);
  let letter = $derived(agent.family.charAt(0).toUpperCase());
</script>

{#if icon}
  <svg
    width={size}
    height={size}
    viewBox={icon.viewBox}
    fill="currentColor"
    role="img"
    aria-label={icon.title}
    style:color={agent.color}
  >
    <path d={icon.path} />
  </svg>
{:else}
  <span
    class="letter"
    style:width="{size}px"
    style:height="{size}px"
    style:font-size="{Math.round(size * 0.85)}px"
    style:color={agent.color}
  >{letter}</span>
{/if}

<style>
  svg {
    display: block;
    flex-shrink: 0;
  }
  .letter {
    display: flex;
    align-items: center;
    justify-content: center;
    line-height: 1;
    font-weight: 700;
    flex-shrink: 0;
    user-select: none;
  }
</style>
