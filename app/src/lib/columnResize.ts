// The drag itself, shared by every resizable column handle (Commits,
// Issues, Comments, PullRequests, Watched) — the arithmetic lives in
// columns.ts, kept free of any DOM so it stays trivially unit-testable;
// this is only the pointer wiring that calls into it.
import { computeResizedWidth } from "./columns";

export interface ColumnResizeParams {
  /** The column's width right now — read once at pointerdown as the
   * drag's anchor, not re-read mid-drag. */
  width: number;
  min: number;
  max: number;
  /** The flexible title element's floor (see columns.ts). */
  titleMin: number;
  /** CSS selector for the row ancestor to search within via `closest` —
   * kept a string, not a bound element, so this file never has to know
   * which `{#each}` iteration produced the row a given handle lives in. */
  rowSelector: string;
  /** CSS selector, queried within that row, for the flexible title
   * element whose live width this column's growth is clamped against. */
  titleSelector: string;
  /** Called on every pointermove with the candidate width — cheap, no
   * persistence here (that's `onCommit`, once per drag). */
  onResize: (width: number) => void;
  /** Called once, on pointerup, with the final width — where a caller
   * persists to localStorage. */
  onCommit: (width: number) => void;
  /** Double-click the handle: reset to the column's default. */
  onReset: () => void;
}

/** A 4px hit area on one column's trailing edge (the visible 1px line and
 * its hover/dragging colors are `.col-handle` in app.css). Deliberately
 * not in the tab order: these views have no header row, so the handle
 * lives inside the column itself and repeats on every row rather than
 * once — a few hundred duplicate "resize" tab stops would be worse for a
 * keyboard user than no keyboard path at all for this refinement. */
export function columnResize(node: HTMLElement, params: ColumnResizeParams) {
  let current = params;
  node.style.touchAction = "none";

  function onPointerDown(e: PointerEvent) {
    if (e.pointerType === "mouse" && e.button !== 0) return;
    e.preventDefault();
    node.setPointerCapture(e.pointerId);
    const rowEl = node.closest(current.rowSelector);
    const titleEl = rowEl?.querySelector(current.titleSelector) as HTMLElement | null;
    const startWidth = current.width;
    const startX = e.clientX;
    let latest = startWidth;
    node.classList.add("dragging");

    function onMove(ev: PointerEvent) {
      const titleWidth = titleEl?.getBoundingClientRect().width ?? Number.POSITIVE_INFINITY;
      latest = computeResizedWidth({
        startWidth,
        delta: ev.clientX - startX,
        min: current.min,
        max: current.max,
        titleWidth,
        titleMin: current.titleMin,
      });
      current.onResize(latest);
    }
    function onUp(ev: PointerEvent) {
      node.releasePointerCapture(ev.pointerId);
      node.removeEventListener("pointermove", onMove);
      node.removeEventListener("pointerup", onUp);
      node.removeEventListener("pointercancel", onUp);
      node.classList.remove("dragging");
      current.onCommit(latest);
    }
    node.addEventListener("pointermove", onMove);
    node.addEventListener("pointerup", onUp);
    node.addEventListener("pointercancel", onUp);
  }

  // Every view but Watched nests this handle inside a row that is itself
  // a `<button>` (opening the item on click) — a click here must never
  // bubble into it just because a resize happened to start and end over
  // the same element.
  function onClick(e: MouseEvent) {
    e.stopPropagation();
  }
  function onDoubleClick(e: MouseEvent) {
    e.stopPropagation();
    current.onReset();
  }

  node.addEventListener("pointerdown", onPointerDown);
  node.addEventListener("click", onClick);
  node.addEventListener("dblclick", onDoubleClick);

  return {
    update(next: ColumnResizeParams) {
      current = next;
    },
    destroy() {
      node.removeEventListener("pointerdown", onPointerDown);
      node.removeEventListener("click", onClick);
      node.removeEventListener("dblclick", onDoubleClick);
    },
  };
}
