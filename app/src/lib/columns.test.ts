import { beforeEach, describe, expect, test } from "vitest";
import { computeResizedWidth, loadColumnWidths, saveColumnWidth, type ColumnDef } from "./columns";

// vitest's default "node" environment has no `localStorage` (unlike the
// real Tauri webview these modules ship in) — a small in-memory stand-in,
// not a new jsdom dependency, is enough for columns.ts's own try/catch
// wrapper to exercise the real code path rather than a mock of it.
class MemoryStorage implements Storage {
  private map = new Map<string, string>();
  get length() {
    return this.map.size;
  }
  clear(): void {
    this.map.clear();
  }
  getItem(key: string): string | null {
    return this.map.has(key) ? this.map.get(key)! : null;
  }
  key(index: number): string | null {
    return [...this.map.keys()][index] ?? null;
  }
  removeItem(key: string): void {
    this.map.delete(key);
  }
  setItem(key: string, value: string): void {
    this.map.set(key, value);
  }
}
globalThis.localStorage = new MemoryStorage();

describe("computeResizedWidth", () => {
  const base = { min: 44, max: 140, titleWidth: 400, titleMin: 120 };

  test("grows freely when both the column's own bounds and the title's room allow it", () => {
    expect(computeResizedWidth({ ...base, startWidth: 60, delta: 20 })).toBe(80);
  });

  test("shrinks freely toward the column's own minimum", () => {
    expect(computeResizedWidth({ ...base, startWidth: 60, delta: -10 })).toBe(50);
  });

  test("never shrinks past the column's configured minimum", () => {
    expect(computeResizedWidth({ ...base, startWidth: 60, delta: -1000 })).toBe(44);
  });

  test("never grows past the column's configured maximum, even with room to spare on the title", () => {
    expect(computeResizedWidth({ ...base, startWidth: 60, delta: 1000, titleWidth: 2000 })).toBe(140);
  });

  test("stops short of its configured max when the title column would drop below its floor first", () => {
    // Title is at 200px with a 120px floor: 80px of headroom (60 + 80 =
    // 140) happens to land exactly on the column's own 140px ceiling here
    // — the next test picks numbers where the title constraint clearly
    // binds tighter than the configured max.
    const result = computeResizedWidth({ ...base, startWidth: 60, delta: 1000, titleWidth: 200 });
    expect(result).toBe(140);
  });

  test("the title-room ceiling actually binds when it is the tighter constraint", () => {
    const result = computeResizedWidth({ min: 44, max: 400, titleWidth: 160, titleMin: 120, startWidth: 60, delta: 1000 });
    // Only 40px of title headroom (160 - 120) — the column can grow to
    // 100px, not its 400px configured max.
    expect(result).toBe(100);
  });

  test("title already at its floor: the dragged column cannot grow at all", () => {
    const result = computeResizedWidth({ ...base, startWidth: 60, delta: 50, titleWidth: 120 });
    expect(result).toBe(60);
  });

  test("title already below its floor (a pre-existing squeeze): still refuses to grow the column further", () => {
    const result = computeResizedWidth({ ...base, startWidth: 60, delta: 50, titleWidth: 90 });
    expect(result).toBe(60);
  });

  test("small negative delta clamped exactly at the minimum boundary", () => {
    expect(computeResizedWidth({ ...base, startWidth: 44, delta: -1 })).toBe(44);
  });

  test("zero delta is a no-op", () => {
    expect(computeResizedWidth({ ...base, startWidth: 70, delta: 0 })).toBe(70);
  });
});

describe("loadColumnWidths / saveColumnWidth", () => {
  const columns: ColumnDef[] = [
    { key: "login", default: 60, min: 44, max: 140 },
    { key: "repo", default: 130, min: 60, max: 220 },
  ];

  beforeEach(() => {
    localStorage.clear();
  });

  test("returns every column's default when nothing is stored", () => {
    expect(loadColumnWidths("commits", columns)).toEqual({ login: 60, repo: 130 });
  });

  test("round-trips a saved width", () => {
    saveColumnWidth("commits", "login", 90);
    expect(loadColumnWidths("commits", columns)).toEqual({ login: 90, repo: 130 });
  });

  test("saving one column never disturbs another already-saved column, or another view", () => {
    saveColumnWidth("commits", "login", 90);
    saveColumnWidth("commits", "repo", 180);
    saveColumnWidth("issues", "repo", 200);
    expect(loadColumnWidths("commits", columns)).toEqual({ login: 90, repo: 180 });
    expect(loadColumnWidths("issues", [columns[1]])).toEqual({ repo: 200 });
  });

  test("falls back to the default when the stored value is outside the column's current [min, max]", () => {
    saveColumnWidth("commits", "login", 90);
    // A later column-def change tightens the range so 90 no longer fits.
    const tightened: ColumnDef[] = [{ key: "login", default: 60, min: 44, max: 80 }];
    expect(loadColumnWidths("commits", tightened)).toEqual({ login: 60 });
  });

  test("falls back to the default when the stored value is the wrong type", () => {
    localStorage.setItem("vigie.columnWidths.v1", JSON.stringify({ commits: { login: "90px" } }));
    expect(loadColumnWidths("commits", columns)).toEqual({ login: 60, repo: 130 });
  });

  test("falls back to the default when the stored value is NaN or non-finite", () => {
    localStorage.setItem("vigie.columnWidths.v1", JSON.stringify({ commits: { login: NaN, repo: Infinity } }));
    // JSON.stringify drops NaN/Infinity to null, exercising the "missing"
    // path rather than a literal non-finite number surviving the round
    // trip — still must fall back to defaults either way.
    expect(loadColumnWidths("commits", columns)).toEqual({ login: 60, repo: 130 });
  });

  test("falls back to the default when localStorage holds corrupt JSON", () => {
    localStorage.setItem("vigie.columnWidths.v1", "{not json");
    expect(loadColumnWidths("commits", columns)).toEqual({ login: 60, repo: 130 });
  });

  test("falls back to the default when the stored root isn't an object", () => {
    localStorage.setItem("vigie.columnWidths.v1", JSON.stringify("nope"));
    expect(loadColumnWidths("commits", columns)).toEqual({ login: 60, repo: 130 });
  });

  test("a boundary value exactly at min or max is trusted, not rejected", () => {
    saveColumnWidth("commits", "login", 44);
    saveColumnWidth("commits", "repo", 220);
    expect(loadColumnWidths("commits", columns)).toEqual({ login: 44, repo: 220 });
  });
});
