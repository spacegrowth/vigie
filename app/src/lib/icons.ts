// Inline SVG markup for every icon in the app — no icon font, no external
// icon library, per the bloat budget. Each entry is the *inner* markup of a
// 24x24 viewBox (one or more <circle>/<path> elements), rendered by
// Icon.svelte via `{@html ...}`. These strings are authored here, not user
// input, so that's a safe use of `{@html}`.
//
// The sidebar glyphs are copied verbatim from docs/design/*.dc.html (they
// are the agreed spec) — with one deliberate exception: `settings`. Every
// mockup draws Settings with a circle-plus-eight-rays glyph, which reads as
// a sun/brightness icon, not a gear; that was flagged, so this file draws
// an actual cog outline (a ring of teeth around a hole) instead, overriding
// the mockups for this one glyph per the packet.
export const ICONS = {
  feed: '<path d="M4 6h16M4 12h16M4 18h10"/>',
  pull_requests:
    '<circle cx="6" cy="5" r="2.5"/><circle cx="6" cy="19" r="2.5"/><circle cx="18" cy="19" r="2.5"/><path d="M6 7.5v9M18 16.5V11a3 3 0 0 0-3-3h-4"/>',
  commits: '<circle cx="12" cy="12" r="3.5"/><path d="M2 12h6.5M15.5 12H22"/>',
  comments: '<path d="M4 5h16v11H9l-5 4z"/>',
  issues: '<circle cx="12" cy="12" r="9"/><circle cx="12" cy="12" r="2.5"/>',
  // Three ascending bars on a baseline — copied verbatim from the sidebar's
  // "Summary" entry in docs/design/Main.dc.html and People.dc.html.
  summary: '<path d="M4 20V12M10 20V6M16 20v-9M4 20h16"/>',
  people:
    '<circle cx="9" cy="8" r="3.5"/><path d="M3 20c0-3.5 2.7-6 6-6s6 2.5 6 6M16 4.5a3.5 3.5 0 0 1 0 7M21 20c0-3-1.8-5.2-4.5-5.8"/>',
  repos: '<path d="M6 3h12v18H6zM9 7h6M9 11h6"/>',
  team: '<circle cx="12" cy="7" r="3.5"/><path d="M5 21c0-4 3-7 7-7s7 3 7 7"/>',
  // A cog outline: an 8-tooth ring with flat-topped teeth (computed, then
  // rendered and visually verified via QuickLook rather than eyeballed —
  // an earlier version with pointed teeth read as a star, not a gear)
  // around an axle hole — deliberately NOT the mockups' sun/rays glyph
  // (see module doc).
  settings:
    '<path d="M10.37,5.19 L10.55,2.81 L13.45,2.81 L13.63,5.19 L15.66,6.03 L17.47,4.48 L19.52,6.53 L17.97,8.34 L18.81,10.37 L21.19,10.55 L21.19,13.45 L18.81,13.63 L17.97,15.66 L19.52,17.47 L17.47,19.52 L15.66,17.97 L13.63,18.81 L13.45,21.19 L10.55,21.19 L10.37,18.81 L8.34,17.97 L6.53,19.52 L4.48,17.47 L6.03,15.66 L5.19,13.63 L2.81,13.45 L2.81,10.55 L5.19,10.37 L6.03,8.34 L4.48,6.53 L6.53,4.48 L8.34,6.03 Z"/><circle cx="12" cy="12" r="3"/>',
  spinner: '<path d="M12 2v4M12 18v4M4.9 4.9l2.8 2.8M16.3 16.3l2.8 2.8M2 12h4M18 12h4M4.9 19.1l2.8-2.8M16.3 7.7l2.8-2.8"/>',
  external: '<path d="M14 5h5v5M19 5l-9 9M8 5H6a1 1 0 0 0-1 1v12a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1v-2"/>',
  close: '<path d="M6 6l12 12M18 6L6 18"/>',
  // The abstract three-dot "sign in with GitHub" mark from
  // docs/design/SignIn.dc.html, copied verbatim (it is not the real
  // GitHub logo, just the mockup's stand-in glyph for the button).
  github: '<circle cx="6" cy="6" r="2.5"/><circle cx="18" cy="6" r="2.5"/><circle cx="12" cy="18" r="2.5"/><path d="M6 8.5v3a3 3 0 0 0 3 3h6a3 3 0 0 0 3-3v-3M12 14.5v1"/>',
  // The Watched sidebar entry and the detail pane's un-watched toggle state
  // (docs/design/Watched.dc.html) — outline, so it takes the default
  // stroke-only styling like every other sidebar glyph.
  eye: '<path d="M2 12s3.6-6.5 10-6.5 10 6.5 10 6.5-3.6 6.5-10 6.5S2 12 2 12z"/><circle cx="12" cy="12" r="3"/>',
  // The filled "Watching" state (docs/design/DetailFiles.dc.html's
  // `.watching` badge and FeedWatched.dc.html's row marker): the outer path
  // and inner circle carry their own fill/stroke, overriding Icon.svelte's
  // default outline styling, so this one always reads as filled regardless
  // of the current text color.
  eye_filled:
    '<path d="M2 12s3.6-6.5 10-6.5 10 6.5 10 6.5-3.6 6.5-10 6.5S2 12 2 12z" fill="currentColor" stroke="none"/><circle cx="12" cy="12" r="3" fill="var(--surface)" stroke="none"/>',
  // The reader-mode pane header's Back arrow (replaces the old Expand/
  // Collapse text toggle — the pane now enters reader mode via a double-
  // click/Enter gesture on a row and leaves it via this arrow, Esc, or
  // ⌘Enter): a plain leftward arrow, same stroke style as its neighbours.
  back: '<path d="M11 4L5 12l6 8M5 12h14"/>',
  // The split-layout pane header's full-view button (packet: "full view
  // discoverable and reachable from anywhere in the detail pane") — four
  // corner brackets pointing outward. Re-added verbatim: this is the same
  // glyph the old text-label Expand/Collapse toggle used before 8b8d475
  // replaced that toggle with the double-click/Enter gesture and the
  // reader-mode `back` arrow above; it never stopped meaning "go full view."
  expand: '<path d="M8 3H3v5M16 3h5v5M8 21H3v-5M16 21h5v-5"/>',
  // ThreadBlock's collapse/expand toggle: a single downward chevron, rotated
  // -90deg via CSS when the thread is collapsed rather than swapped for a
  // second glyph. Reader mode's next-item stepper (DetailPane.svelte) reuses
  // this same downward glyph unrotated; `chevron_up` below is its mirror for
  // the previous-item button, rather than rotating this one by CSS again —
  // two independent features shouldn't share a rotation-class contract.
  chevron: '<path d="M6 9l6 6 6-6"/>',
  chevron_up: '<path d="M6 15l6-6 6 6"/>',
  // The tray popover's icon-only KindBadge (this packet, "a symbol instead
  // of the word") — `pull_requests` above already reads as "open PR" on its
  // own, so `pr_opened` reuses it verbatim; these five are the glyphs it has
  // no match for. `pr_merged`/`pr_closed` keep `pull_requests`' left-hand
  // trunk (the two circles + connecting line — a PR is still a branch) and
  // swap its right-hand fork for a check or an X, so the three read as one
  // family with three different endings rather than one glyph recoloured
  // three times. The three `review_*` glyphs are what a `pr_reviewed` event
  // maps to (KindBadge picks one from the event's own title — see its own
  // comment) — a check-in-circle, a pencil and a loupe, each unrelated in
  // silhouette to `comments`' speech bubble so a review never reads as a
  // plain comment.
  pr_merged: '<circle cx="6" cy="5" r="2.5"/><circle cx="6" cy="19" r="2.5"/><path d="M6 7.5v9"/><path d="M9.5 13.5l2.5 2.5 5-6"/>',
  pr_closed: '<circle cx="6" cy="5" r="2.5"/><circle cx="6" cy="19" r="2.5"/><path d="M6 7.5v9"/><path d="M10 11.5l5 5M15 11.5l-5 5"/>',
  review_approved: '<circle cx="12" cy="12" r="9"/><path d="M8 12.5l2.5 2.5L16 9.5"/>',
  review_changes: '<path d="M14 4l6 6-9.5 9.5-7 1 1-7z"/><path d="M12.5 5.5l6 6"/>',
  review_commented: '<circle cx="10.5" cy="10.5" r="6.5"/><path d="M15.3 15.3L20 20"/>',
} as const;

export type IconName = keyof typeof ICONS;
