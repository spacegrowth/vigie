// Pure-ish behavior of the feed's two view-filter preference stores (packet
// gm-feed-r1): the "My team / Everyone" seed-then-never-override rule, and
// the repo selector's plain select/clear. Each test builds a *fresh* store
// via the exported factory rather than the shared singleton, so cases don't
// leak state into each other. No `localStorage`/`window` exists under
// vitest's default node environment, which is deliberately what exercises
// these stores' try/catch fallbacks — the same ones `stores.ts`'s
// `teamFilterStore` relies on for the tray popover's separate webview.
import { describe, expect, it } from "vitest";
import { get } from "svelte/store";
import { createFeedModeStore, createRepoFilterStore, effectiveFilterMode, scopeEmptyMessage } from "./feed-filters";

describe("feedModeStore", () => {
  it("starts with no explicit choice", () => {
    const store = createFeedModeStore();
    expect(get(store.mode)).toBeNull();
  });

  it("seedFromSettings fills in the starting value once, from Settings.filter_mode", () => {
    const store = createFeedModeStore();
    store.seedFromSettings("team");
    expect(get(store.mode)).toBe("team");
  });

  it("seedFromSettings never overrides an explicit choice already made", () => {
    const store = createFeedModeStore();
    store.select("all");
    store.seedFromSettings("team");
    expect(get(store.mode)).toBe("all");
  });

  it("seedFromSettings is a no-op once a value already exists, even a seeded one", () => {
    const store = createFeedModeStore();
    store.seedFromSettings("team");
    store.seedFromSettings("all");
    expect(get(store.mode)).toBe("team");
  });

  it("select sets the mode directly, in either direction", () => {
    const store = createFeedModeStore();
    store.select("team");
    expect(get(store.mode)).toBe("team");
    store.select("all");
    expect(get(store.mode)).toBe("all");
  });
});

describe("repoFilterStore", () => {
  it("starts with no repo selected (\"All repos\")", () => {
    const store = createRepoFilterStore();
    expect(get(store.selected)).toBeNull();
  });

  it("select narrows to one repo id", () => {
    const store = createRepoFilterStore();
    store.select(42);
    expect(get(store.selected)).toBe(42);
  });

  it("select(null) clears back to \"All repos\"", () => {
    const store = createRepoFilterStore();
    store.select(42);
    store.select(null);
    expect(get(store.selected)).toBeNull();
  });
});

describe("effectiveFilterMode", () => {
  it("uses the explicit choice when one has been made", () => {
    expect(effectiveFilterMode("all", "team")).toBe("all");
    expect(effectiveFilterMode("team", "all")).toBe("team");
  });

  it("falls back to the settings default when nothing has been chosen yet", () => {
    expect(effectiveFilterMode(null, "team")).toBe("team");
    expect(effectiveFilterMode(null, "all")).toBe("all");
  });
});

describe("scopeEmptyMessage", () => {
  it("returns null when nothing narrows the view", () => {
    expect(scopeEmptyMessage("commits", { mode: "all" })).toBeNull();
  });

  it("names the repo when one is selected", () => {
    expect(scopeEmptyMessage("commits", { repoLabel: "paperclipai/paperclip", mode: "all" })).toBe(
      "No commits in paperclipai/paperclip.",
    );
  });

  it("names 'your team' under team mode with no specific team selected", () => {
    expect(scopeEmptyMessage("comments", { mode: "team" })).toBe("No comments for your team.");
  });

  it("names the selected team instead of the generic 'your team' wording", () => {
    expect(scopeEmptyMessage("comments", { teamLabel: "Platform", mode: "team" })).toBe(
      "No comments for Platform.",
    );
  });

  it("a selected team's own name wins even under 'Everyone' mode — team narrows independently of mode", () => {
    expect(scopeEmptyMessage("comments", { teamLabel: "Platform", mode: "all" })).toBe("No comments for Platform.");
  });

  it("combines a repo and a team narrowing", () => {
    expect(
      scopeEmptyMessage("commits", { repoLabel: "paperclipai/paperclip", teamLabel: "Platform", mode: "all" }),
    ).toBe("No commits in paperclipai/paperclip for Platform.");
  });
});
