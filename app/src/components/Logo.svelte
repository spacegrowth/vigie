<script lang="ts">
  // The Vigie mark, in one place so a future swap touches only this file.
  //
  // The geometry is the app icon's own mark group (app/src-tauri/icons/
  // source.svg), converted from its 1024 grid to this 24x24 one and scaled
  // about the centre exactly as the icon scales it. Keep the two in step:
  // the Dock icon, the menu bar glyph and this sidebar mark are the same
  // drawing, and they look wrong the moment they diverge.
  //
  // Why it is drawn this way: a plain tapered tube with a bright wide end
  // reads as a torch, not a telescope. The stepped draw-tube — four
  // segments widening toward the objective, rings at the joints, and a dark
  // cap at the wide end — is what makes it read as a spyglass at small
  // sizes, and the highlight sits at the EYEPIECE, the end you look
  // through, rather than at the front where it would look lit.
  //
  // `variant="flat"` (the sidebar): the mark alone in the brand green.
  // `variant="icon"`: the mark on the icon's own tile, wash included.
  // `variant="mark"`: white, for a saturated background.
  let { size = 16, variant = "flat" }: { size?: number; variant?: "icon" | "mark" | "flat" } = $props();

  let tube = $derived(variant === "mark" ? "#ffffff" : "var(--accent)");
  let ring = $derived(variant === "mark" ? "#bfe3cf" : "var(--accent-deep)");
  let eyepiece = $derived(variant === "mark" ? "#ffffff" : "#cfe8d6");
  let star = $derived(variant === "mark" ? "#fff7ed" : "var(--accent)");
</script>

<svg class:tile={variant === "icon"} viewBox="0 0 24 24" width={size} height={size} aria-hidden="true">
  {#if variant === "icon"}
    <defs>
      <linearGradient id="vigie-tile" x1="0" y1="0" x2="1" y2="1">
        <stop offset="0" stop-color="#e8ece5" />
        <stop offset="1" stop-color="#d3ddcf" />
      </linearGradient>
      <linearGradient id="vigie-wash" x1="0" y1="0" x2="0" y2="1">
        <stop offset="0" stop-color="#1b4d31" stop-opacity="0" />
        <stop offset="1" stop-color="#1b4d31" stop-opacity="0.26" />
      </linearGradient>
      <clipPath id="vigie-clip"><rect width="24" height="24" rx="5.32" /></clipPath>
    </defs>
    <rect width="24" height="24" rx="5.32" fill="url(#vigie-tile)" />
    <g clip-path="url(#vigie-clip)">
      <path d="M0 13.5C6 10.5 18 10.5 24 13.5V24H0z" fill="url(#vigie-wash)" />
    </g>
  {/if}
  <g transform="translate(12,12) scale(1.1) translate(-12,-12)">
    <g transform="rotate(-38 12 12)">
      <rect x="3.516" y="10.57" width="2.812" height="2.859" rx="0.609" fill={eyepiece} />
      <rect x="5.859" y="10.141" width="4.922" height="3.717" rx="0.703" fill={tube} />
      <rect x="10.312" y="9.713" width="5.039" height="4.575" rx="0.797" fill={tube} />
      <rect x="14.883" y="9.141" width="5.273" height="5.719" rx="0.938" fill={tube} />
      <rect x="5.812" y="10.141" width="0.422" height="3.717" rx="0.211" fill={ring} />
      <rect x="10.266" y="9.713" width="0.422" height="4.575" rx="0.211" fill={ring} />
      <rect x="14.836" y="9.141" width="0.422" height="5.719" rx="0.211" fill={ring} />
      <rect x="19.594" y="9.998" width="0.609" height="4.003" rx="0.305" fill={ring} />
    </g>
    <circle cx="19.266" cy="4.594" r="0.891" fill={star} />
    <circle cx="21.234" cy="6.984" r="0.516" fill={star} />
  </g>
</svg>

<style>
  svg {
    flex-shrink: 0;
  }
  svg.tile {
    border-radius: 22%;
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.18);
  }
</style>
