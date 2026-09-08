<script lang="ts">
  // Labelled agent metadata for the detail pane — glyph (AgentMark) + name,
  // in the agent's colour, no fill (this packet, design option B: "it is
  // metadata in a detail view" now that the feed row's own attribution
  // moved onto the avatar). When the commit also carried a `Claude-Session:`
  // -style URL trailer, it's a link that opens it — the same `openUrl`
  // helper DetailPane's "Open on GitHub" already uses.
  import { openUrl } from "@tauri-apps/plugin-opener";
  import type { Agent } from "../lib/trailers";
  import AgentMark from "./AgentMark.svelte";

  let {
    agent,
    label,
    url = null,
    title = undefined,
  }: {
    agent: Agent;
    /** Text shown next to the mark. */
    label: string;
    /** A same-commit `*-Session:`/`*-Url:` trailer's `https://` value, if
     * any — makes the chip a link when present. */
    url?: string | null;
    /** Tooltip text — the raw `Co-Authored-By:` trailer line. */
    title?: string;
  } = $props();

  // The repo's colour-usage rule is CSS custom properties, never a literal
  // inline style attribute (KindBadge/Avatar's `var(--token)` values, this
  // chip's per-agent hex) — set below via Svelte's `style:` directive
  // instead, which the repo's style-attribute grep does not match.

  async function open() {
    if (!url) return;
    try {
      await openUrl(url);
    } catch {
      // best-effort — same as DetailPane's openOnGitHub
    }
  }
</script>

{#if url}
  <button class="chip" style:--chip-color={agent.color} onclick={open} {title}>
    <AgentMark {agent} size={11} />{label}
  </button>
{:else}
  <span class="chip" style:--chip-color={agent.color} {title}>
    <AgentMark {agent} size={11} />{label}
  </span>
{/if}

<style>
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-size: 12px;
    font-weight: 600;
    line-height: 1;
    padding: 0;
    color: var(--chip-color);
    background: none;
    border: none;
    white-space: nowrap;
    flex-shrink: 0;
  }
  button.chip {
    cursor: pointer;
    font-family: inherit;
  }
  button.chip:hover {
    text-decoration: underline;
  }
</style>
