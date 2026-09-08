// Shared model behind the list views' resizable columns (Commits, Issues,
// Comments, PullRequests, Watched): one place that knows a column's name,
// default width, and how far it's allowed to move — so several views drag
// the same way, clamp the same way, and remember widths the same way,
// instead of each view growing its own copy of the same arithmetic.
//
// Every one of those views has exactly one flexible "title" element
// (Commits' `.msg`, Issues/Comments' `.ref`, PullRequests' `.pr-title`,
// Watched's `.wtitle`) that soaks up whatever width its neighbours don't
// take — nothing here ever sets *that* element's width. What gets stored
// and dragged is a view's *fixed* columns; growing one of those is only
// ever a request, clamped so the title column can never be squeezed
// thinner than `titleMin` (see computeResizedWidth) — that's the packet's
// "don't let it go extreme" rule, enforced once here rather than per view.
// The DOM/pointer wiring that calls into this lives in columnResize.ts —
// kept separate so everything below stays pure and directly testable.

export interface ColumnDef {
  key: string;
  /** px — applied on first render and restored by a handle's double-click reset. */
  default: number;
  min: number;
  max: number;
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

/** Pure resize arithmetic — no DOM, so it's directly testable.
 *
 * `titleWidth` is the flexible title element's *current* rendered width —
 * callers re-measure it on every pointermove rather than once at drag
 * start, so a mid-drag window resize is reflected immediately instead of
 * working off a stale snapshot. `titleMin` is the floor that width must
 * never cross.
 *
 * Growing the dragged column can never take the title element below that
 * floor: `hardMax` is however much wider `startWidth` could get while
 * leaving the title exactly at `titleMin`. The column's own configured
 * `max` is a second, independent ceiling — whichever binds tighter wins.
 * Shrinking (a negative `delta`) is bounded only by the column's own
 * `min`, since the title only ever gains room when a fixed column shrinks. */
export function computeResizedWidth(opts: {
  startWidth: number;
  delta: number;
  min: number;
  max: number;
  titleWidth: number;
  titleMin: number;
}): number {
  const roomFromTitle = Math.max(0, opts.titleWidth - opts.titleMin);
  const hardMax = Math.max(opts.min, Math.min(opts.max, opts.startWidth + roomFromTitle));
  return clamp(opts.startWidth + opts.delta, opts.min, hardMax);
}

// --- Persistence -----------------------------------------------------

/** Bumped whenever a view's column set changes shape (a key renamed, added,
 * or dropped) — widths stored under a previous version are simply never
 * read again rather than misapplied to columns they no longer describe. */
const STORAGE_KEY = "vigie.columnWidths.v1";

type StoredWidths = Record<string, Record<string, unknown>>;

function readStore(): StoredWidths {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    return parsed && typeof parsed === "object" ? (parsed as StoredWidths) : {};
  } catch {
    // Corrupt JSON, storage unavailable (private browsing, quota) — widths
    // just fall back to defaults, same as a first run.
    return {};
  }
}

function writeStore(all: StoredWidths): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(all));
  } catch {
    // Best-effort — losing the preference is fine, throwing isn't.
  }
}

/** Widths for `view`'s columns, one per `columns` entry. A stored value is
 * trusted only when it's a finite number inside that column's own
 * [min, max] — anything else (missing, the wrong type, NaN, out of range
 * because a column's bounds changed since it was saved) falls back to that
 * column's default rather than trusting a number nothing has validated. */
export function loadColumnWidths(view: string, columns: ColumnDef[]): Record<string, number> {
  const stored = readStore()[view] ?? {};
  const widths: Record<string, number> = {};
  for (const col of columns) {
    const value = stored[col.key];
    widths[col.key] = typeof value === "number" && Number.isFinite(value) && value >= col.min && value <= col.max ? value : col.default;
  }
  return widths;
}

export function saveColumnWidth(view: string, key: string, width: number): void {
  const all = readStore();
  all[view] = { ...(all[view] ?? {}), [key]: width };
  writeStore(all);
}
