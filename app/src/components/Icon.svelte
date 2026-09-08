<script lang="ts">
  import { ICONS, type IconName } from "../lib/icons";

  let { name, size = 16 }: { name: IconName; size?: number } = $props();
</script>

<svg viewBox="0 0 24 24" width={size} height={size} aria-hidden="true">
  <!-- eslint-disable-next-line svelte/no-at-html-tags -->
  {@html ICONS[name]}
</svg>

<style>
  svg {
    fill: none;
    stroke: currentColor;
    stroke-width: 1.6;
    stroke-linecap: round;
    stroke-linejoin: round;
    flex-shrink: 0;
  }
</style>
