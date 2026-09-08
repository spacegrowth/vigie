// Svelte action behind the app's scroll indicator (app.css's
// `.scroller-thumb`): a thin, auto-hiding, hand-drawn bar for every long
// scrolling region — the diff/detail body, each list view, and the tray
// popover's list.
//
// Why hand-drawn rather than a styled native scrollbar: a *styled*
// `::-webkit-scrollbar` is WebKit's "classic" scrollbar, which reserves
// layout width once a region's content overflows — on a Mac whose "Show
// scroll bars" system setting is "Always", that shifts content sideways,
// and that isn't something this file can prevent. So the native scrollbar
// stays hidden everywhere (app.css, unconditionally) and this action draws
// its own indicator on top: absolutely positioned, so it never occupies
// layout width and never reflows anything when it appears or disappears.
//
// The indicator is appended as a sibling of the scrolling element inside
// that element's own parent (promoted to `position: relative` if it
// wasn't already one) rather than as a child of the scrolling element
// itself — a `position: absolute` descendant of an `overflow: auto`
// element scrolls together with the rest of that element's content, which
// would make the thumb slide away with the list instead of tracking it.
// Living in the parent instead, its `top`/`left`/`height` are computed
// from the scrolling element's own `offsetTop`/`offsetLeft`/`offsetWidth`
// (relative to that now-positioned parent) plus the pure geometry below,
// so it overlays correctly regardless of what else shares the parent.
export const THUMB_WIDTH = 4;
export const THUMB_INSET = 2;
export const MIN_THUMB_HEIGHT = 24;
const FADE_DELAY_MS = 700;

export interface ScrollMetrics {
  scrollTop: number;
  scrollHeight: number;
  clientHeight: number;
}

export interface ThumbGeometry {
  /** false when the region's content doesn't overflow — the indicator is
   * hidden entirely, not just faded, in that case. */
  visible: boolean;
  /** Thumb height in px, proportional to clientHeight/scrollHeight and
   * clamped to MIN_THUMB_HEIGHT so a very long list still shows a
   * grabbable thumb. */
  height: number;
  /** Thumb's top offset within the track (0..clientHeight - height). */
  top: number;
}

/** Pure geometry: the maths a scrollbar thumb needs, kept separate from
 * any DOM/observer wiring so it's directly unit-testable (scroller.test.ts) —
 * this is exactly the kind of arithmetic that looks right and is off by a
 * factor (inverted proportions, an unclamped edge, a divide-by-zero at
 * the no-overflow boundary). */
export function computeThumbGeometry({ scrollTop, scrollHeight, clientHeight }: ScrollMetrics): ThumbGeometry {
  if (scrollHeight <= clientHeight) {
    return { visible: false, height: 0, top: 0 };
  }
  const proportional = (clientHeight / scrollHeight) * clientHeight;
  const height = Math.min(clientHeight, Math.max(MIN_THUMB_HEIGHT, proportional));
  const maxScrollTop = scrollHeight - clientHeight;
  const trackRange = clientHeight - height;
  const rawTop = maxScrollTop > 0 ? (scrollTop / maxScrollTop) * trackRange : 0;
  // Clamped defensively: macOS rubber-bands scroll position past [0,
  // maxScrollTop] during overscroll, which would otherwise fling the
  // thumb off the track for a frame.
  const top = Math.max(0, Math.min(trackRange, rawTop));
  return { visible: true, height, top };
}

// Two scroll regions can share one parent (Teams.svelte's `.list` and
// `.detail` both live directly in `.split`), and each mounts/unmounts on
// its own schedule. Tracking "did *I* set this host's position" per
// instance would race: whichever instance happens to unmount first would
// blindly clear `position: relative` out from under the other, still
// mounted, still-relying-on-it instance. A host-keyed ref count instead
// makes exactly the first acquirer set it and exactly the last releaser
// clear it, independent of mount/unmount order.
const hostState = new WeakMap<HTMLElement, { count: number; weSetPosition: boolean }>();

function acquireHost(host: HTMLElement): void {
  const existing = hostState.get(host);
  if (existing) {
    existing.count += 1;
    return;
  }
  const weSetPosition = getComputedStyle(host).position === "static";
  if (weSetPosition) host.style.position = "relative";
  hostState.set(host, { count: 1, weSetPosition });
}

function releaseHost(host: HTMLElement): void {
  const state = hostState.get(host);
  if (!state) return;
  state.count -= 1;
  if (state.count <= 0) {
    if (state.weSetPosition) host.style.position = "";
    hostState.delete(host);
  }
}

export function scroller(node: HTMLElement) {
  const host = node.parentElement;
  if (!host) {
    // No parent to anchor the indicator in (shouldn't happen for a
    // mounted element) — degrade to no indicator rather than throw.
    return { destroy() {} };
  }

  acquireHost(host);

  const thumb = document.createElement("div");
  thumb.className = "scroller-thumb";
  host.appendChild(thumb);

  let scrolling = false;
  let dragging = false;
  let fadeTimeout: ReturnType<typeof setTimeout> | undefined;

  function applyVisibility() {
    thumb.classList.toggle("visible", scrolling || dragging);
  }

  function scheduleFade() {
    clearTimeout(fadeTimeout);
    fadeTimeout = setTimeout(() => {
      scrolling = false;
      applyVisibility();
    }, FADE_DELAY_MS);
  }

  function update() {
    const geometry = computeThumbGeometry({
      scrollTop: node.scrollTop,
      scrollHeight: node.scrollHeight,
      clientHeight: node.clientHeight,
    });
    if (!geometry.visible) {
      thumb.style.display = "none";
      return;
    }
    thumb.style.display = "block";
    thumb.style.top = `${node.offsetTop + geometry.top}px`;
    thumb.style.left = `${node.offsetLeft + node.offsetWidth - THUMB_WIDTH - THUMB_INSET}px`;
    thumb.style.height = `${geometry.height}px`;
  }

  function onScroll() {
    update();
    scrolling = true;
    applyVisibility();
    scheduleFade();
  }

  function onThumbPointerDown(e: PointerEvent) {
    e.preventDefault();
    const geometry = computeThumbGeometry({
      scrollTop: node.scrollTop,
      scrollHeight: node.scrollHeight,
      clientHeight: node.clientHeight,
    });
    const maxScrollTop = node.scrollHeight - node.clientHeight;
    const trackRange = node.clientHeight - geometry.height;
    if (trackRange <= 0 || maxScrollTop <= 0) return;

    thumb.setPointerCapture(e.pointerId);
    const startY = e.clientY;
    const startScrollTop = node.scrollTop;
    dragging = true;
    thumb.classList.add("dragging");
    applyVisibility();

    // Scoped to `thumb` and removed on pointerup/cancel below — pointer
    // capture routes move/up events here even once the pointer leaves the
    // thumb or the container, so no document/window listener is needed.
    function onMove(ev: PointerEvent) {
      const deltaY = ev.clientY - startY;
      const scrollDelta = (deltaY / trackRange) * maxScrollTop;
      node.scrollTop = Math.max(0, Math.min(maxScrollTop, startScrollTop + scrollDelta));
      update();
    }
    function onUp(ev: PointerEvent) {
      thumb.releasePointerCapture(ev.pointerId);
      thumb.removeEventListener("pointermove", onMove);
      thumb.removeEventListener("pointerup", onUp);
      thumb.removeEventListener("pointercancel", onUp);
      dragging = false;
      thumb.classList.remove("dragging");
      applyVisibility();
      scheduleFade();
    }
    thumb.addEventListener("pointermove", onMove);
    thumb.addEventListener("pointerup", onUp);
    thumb.addEventListener("pointercancel", onUp);
  }

  // Re-measuring: ResizeObserver on the container catches its own box
  // changing (a window/split-pane resize). None of these views wrap their
  // rows in one single content element, so there's no single child to
  // hand ResizeObserver as "the content" — instead every direct child is
  // observed (any of their heights changing, e.g. a repo group gaining
  // rows, changes node.scrollHeight), and a MutationObserver keeps that
  // set of observed children in sync as the list re-renders (a day group
  // or a "Load older activity" batch appearing/disappearing).
  const ro = new ResizeObserver(update);
  ro.observe(node);
  // Children that have gone away must be released, or the observer keeps a
  // reference to every detached node the list has ever rendered — a slow
  // leak across "Load older" batches and filter changes.
  let observedChildren: Element[] = [];
  function resyncChildObservers() {
    const current = Array.from(node.children);
    for (const child of observedChildren) {
      if (!current.includes(child)) ro.unobserve(child);
    }
    for (const child of current) ro.observe(child);
    observedChildren = current;
  }
  const mo = new MutationObserver(() => {
    resyncChildObservers();
    update();
  });
  mo.observe(node, { childList: true });
  resyncChildObservers();

  node.addEventListener("scroll", onScroll, { passive: true });
  thumb.addEventListener("pointerdown", onThumbPointerDown);

  update();
  applyVisibility();

  return {
    destroy() {
      node.removeEventListener("scroll", onScroll);
      thumb.removeEventListener("pointerdown", onThumbPointerDown);
      ro.disconnect();
      mo.disconnect();
      clearTimeout(fadeTimeout);
      thumb.remove();
      releaseHost(host);
    },
  };
}
