import { describe, expect, test } from "vitest";
import {
  breadcrumbLevels,
  makeEntry,
  popStack,
  popToIndex,
  pushStack,
  replaceStack,
  sameTarget,
  setTopScrollTop,
  setTopTab,
  siblingIndex,
  stepTarget,
  stepTopTarget,
  type DetailNavEntry,
  type DetailTarget,
} from "./detail-nav";

const pr482: DetailTarget = { kind: "thread", repoId: 1, number: 482 };
const pr500: DetailTarget = { kind: "thread", repoId: 1, number: 500 };
const commitA: DetailTarget = { kind: "commit", repoId: 1, sha: "60ec448aaaa" };
const commitB: DetailTarget = { kind: "commit", repoId: 1, sha: "abc1234bbbb" };
const commitC: DetailTarget = { kind: "commit", repoId: 2, sha: "def5678cccc" };

describe("sameTarget", () => {
  test("two threads with the same repo+number are the same target", () => {
    expect(sameTarget(pr482, { kind: "thread", repoId: 1, number: 482 })).toBe(true);
  });
  test("two commits with the same repo+sha are the same target", () => {
    expect(sameTarget(commitA, { kind: "commit", repoId: 1, sha: "60ec448aaaa" })).toBe(true);
  });
  test("different kinds are never the same target", () => {
    expect(sameTarget(pr482, commitA)).toBe(false);
  });
  test("null is never equal to anything, including itself", () => {
    expect(sameTarget(null, null)).toBe(false);
    expect(sameTarget(null, pr482)).toBe(false);
  });
});

describe("makeEntry", () => {
  test("starts on Conversation, scrolled to the top", () => {
    const entry = makeEntry(pr482);
    expect(entry.tab).toBe("conversation");
    expect(entry.scrollTop).toBe(0);
  });
  test("no siblings given — the target is folded in as the only sibling", () => {
    const entry = makeEntry(pr482);
    expect(entry.siblings).toEqual([pr482]);
  });
  test("dedupes repeated siblings by identity, not by array position", () => {
    const entry = makeEntry(pr482, [pr500, pr482, { ...pr482 }]);
    expect(entry.siblings).toEqual([pr500, pr482]);
  });
});

describe("replaceStack / pushStack / popStack", () => {
  test("replaceStack always yields a one-entry stack", () => {
    const stack = replaceStack(makeEntry(pr500));
    expect(stack).toHaveLength(1);
    expect(stack[0].target).toEqual(pr500);
  });

  test("pushStack adds a level on top without disturbing the one underneath", () => {
    const base = replaceStack(makeEntry(pr482, [pr482, pr500]));
    const withCommit = pushStack(base, makeEntry(commitA, [commitA, commitB]));
    expect(withCommit).toHaveLength(2);
    expect(withCommit[0]).toEqual(base[0]); // the PR level is untouched
    expect(withCommit[1].target).toEqual(commitA);
  });

  test("popStack drops the top level and returns exactly the level underneath", () => {
    const base = replaceStack(makeEntry(pr482));
    const pushed = pushStack(base, makeEntry(commitA));
    expect(popStack(pushed)).toEqual(base);
  });

  test("popStack at the root (one entry) is a no-op — the caller closes/exits instead", () => {
    const base = replaceStack(makeEntry(pr482));
    expect(popStack(base)).toBe(base);
  });

  test("a replace after a push discards the whole chain, not just the top", () => {
    const withCommit = pushStack(replaceStack(makeEntry(pr482)), makeEntry(commitA));
    const replaced = replaceStack(makeEntry(pr500));
    expect(replaced).toHaveLength(1);
    expect(replaced[0].target).toEqual(pr500);
    // (the old `withCommit` stack itself is untouched by building `replaced`)
    expect(withCommit).toHaveLength(2);
  });
});

describe("popToIndex", () => {
  const level0 = makeEntry(pr482);
  const level1 = makeEntry(commitA);
  const level2 = makeEntry(commitB);
  const stack = [level0, level1, level2];

  test("pops back to an earlier level — a breadcrumb click", () => {
    expect(popToIndex(stack, 0)).toEqual([level0]);
    expect(popToIndex(stack, 1)).toEqual([level0, level1]);
  });

  test("a click on the current (last) level is a no-op", () => {
    expect(popToIndex(stack, 2)).toBe(stack);
  });

  test("an out-of-range index (stale click on a shrunk stack) is a no-op", () => {
    expect(popToIndex(stack, 5)).toBe(stack);
    expect(popToIndex(stack, -1)).toBe(stack);
  });
});

describe("setTopTab / setTopScrollTop — tab and scroll restore", () => {
  test("setTopTab only touches the top level, leaving the rest of the stack alone", () => {
    const base = replaceStack(makeEntry(pr482));
    const onCommits = setTopTab(base, "commits");
    expect(onCommits[0].tab).toBe("commits");

    const withCommit = pushStack(onCommits, makeEntry(commitA));
    expect(withCommit[0].tab).toBe("commits"); // still remembered underneath

    // Popping back to the PR restores the tab it was left on — this is the
    // exact bug report: "landing back on Conversation when the user was on
    // Commits".
    const back = popStack(withCommit);
    expect(back[0].tab).toBe("commits");
  });

  test("setting the same tab again is a no-op (stable identity, no re-render)", () => {
    const base = setTopTab(replaceStack(makeEntry(pr482)), "commits");
    expect(setTopTab(base, "commits")).toBe(base);
  });

  test("setTopScrollTop is remembered the same way across a push/pop", () => {
    const scrolled = setTopScrollTop(replaceStack(makeEntry(pr482)), 340);
    const withCommit = pushStack(scrolled, makeEntry(commitA));
    const back = popStack(withCommit);
    expect(back[0].scrollTop).toBe(340);
  });

  test("a no-op update on an empty stack doesn't throw", () => {
    expect(setTopTab([], "files")).toEqual([]);
  });
});

describe("siblingIndex / stepTarget — stepper siblings per level", () => {
  test("finds the target's position in its own siblings", () => {
    const entry = makeEntry(pr482, [pr482, pr500]);
    expect(siblingIndex(entry)).toBe(0);
  });

  test("-1 when the target isn't in its own siblings snapshot at all", () => {
    const entry: DetailNavEntry = { target: commitC, siblings: [commitA, commitB], tab: "conversation", scrollTop: 0 };
    expect(siblingIndex(entry)).toBe(-1);
    expect(stepTarget(entry, 1)).toBeNull();
  });

  test("steps to the next/previous item in the list", () => {
    const entry = makeEntry(commitA, [commitA, commitB]);
    expect(stepTarget(entry, 1)).toEqual(commitB);
    expect(stepTarget(entry, -1)).toBeNull(); // already first
  });

  test("stepping past either end is a no-op", () => {
    const entry = makeEntry(commitB, [commitA, commitB]);
    expect(stepTarget(entry, 1)).toBeNull(); // already last
  });
});

describe("stepTopTarget — the stepper follows the level", () => {
  test("after drilling into a commit, stepping walks THAT level's siblings, not the PR's", () => {
    // Opened from a PR's Commits tab: the PR level has PR siblings, the
    // pushed commit level has that PR's own commit list.
    const prLevel = setTopTab(replaceStack(makeEntry(pr482, [pr482, pr500])), "commits");
    const withCommit = pushStack(prLevel, makeEntry(commitA, [commitA, commitB]));

    const stepped = stepTopTarget(withCommit, 1);
    expect(stepped).toHaveLength(2);
    expect(stepped[1].target).toEqual(commitB); // stepped within the PR's commits
    expect(stepped[0]).toEqual(prLevel[0]); // the PR level underneath is untouched
  });

  test("stepping to a new item resets that level back to Conversation, scrolled to top", () => {
    const level = setTopScrollTop(setTopTab(replaceStack(makeEntry(commitA, [commitA, commitB])), "files"), 200);
    const stepped = stepTopTarget(level, 1);
    expect(stepped[0].target).toEqual(commitB);
    expect(stepped[0].tab).toBe("conversation");
    expect(stepped[0].scrollTop).toBe(0);
  });

  test("stepping past the end of the level's own siblings is a no-op", () => {
    const level = replaceStack(makeEntry(pr500, [pr482, pr500]));
    expect(stepTopTarget(level, 1)).toBe(level);
  });

  test("a no-op step on an empty stack doesn't throw", () => {
    expect(stepTopTarget([], 1)).toEqual([]);
  });
});

describe("breadcrumbLevels — chain and truncation", () => {
  test("one level: no truncation, that level is current", () => {
    const stack = [makeEntry(pr482)];
    const { levels, truncated } = breadcrumbLevels(stack);
    expect(truncated).toBe(false);
    expect(levels).toEqual([{ index: 0, target: pr482, current: true }]);
  });

  test("two levels: the deeper one is current, the PR level is not", () => {
    const stack = [makeEntry(pr482), makeEntry(commitA)];
    const { levels, truncated } = breadcrumbLevels(stack);
    expect(truncated).toBe(false);
    expect(levels).toEqual([
      { index: 0, target: pr482, current: false },
      { index: 1, target: commitA, current: true },
    ]);
  });

  test("exactly maxVisible (3) levels: no truncation, all three shown", () => {
    const stack = [makeEntry(pr482), makeEntry(commitA), makeEntry(commitB)];
    const { levels, truncated } = breadcrumbLevels(stack, 3);
    expect(truncated).toBe(false);
    expect(levels.map((l) => l.index)).toEqual([0, 1, 2]);
  });

  test("more than maxVisible levels: truncated, only the last 3 kept", () => {
    const stack = [makeEntry(pr482), makeEntry(commitA), makeEntry(commitB), makeEntry(commitC)];
    const { levels, truncated } = breadcrumbLevels(stack, 3);
    expect(truncated).toBe(true);
    expect(levels.map((l) => l.index)).toEqual([1, 2, 3]);
    expect(levels[levels.length - 1].current).toBe(true);
  });

  test("an empty stack has no levels and isn't marked truncated", () => {
    expect(breadcrumbLevels([])).toEqual({ levels: [], truncated: false });
  });
});
