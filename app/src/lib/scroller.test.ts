import { describe, expect, test } from "vitest";
import { computeThumbGeometry, MIN_THUMB_HEIGHT } from "./scroller";

describe("computeThumbGeometry", () => {
  test("content exactly fills the viewport — no overflow, hidden", () => {
    const geometry = computeThumbGeometry({ scrollTop: 0, scrollHeight: 200, clientHeight: 200 });
    expect(geometry).toEqual({ visible: false, height: 0, top: 0 });
  });

  test("content shorter than the viewport — no overflow, hidden", () => {
    const geometry = computeThumbGeometry({ scrollTop: 0, scrollHeight: 120, clientHeight: 200 });
    expect(geometry.visible).toBe(false);
  });

  test("scrolled to the top — thumb at the top of the track", () => {
    // clientHeight 200 of scrollHeight 1000 -> proportional height 40,
    // track range 160.
    const geometry = computeThumbGeometry({ scrollTop: 0, scrollHeight: 1000, clientHeight: 200 });
    expect(geometry.visible).toBe(true);
    expect(geometry.height).toBe(40);
    expect(geometry.top).toBe(0);
  });

  test("scrolled to the bottom — thumb's far edge reaches the track's far edge", () => {
    const maxScrollTop = 1000 - 200; // 800
    const geometry = computeThumbGeometry({ scrollTop: maxScrollTop, scrollHeight: 1000, clientHeight: 200 });
    expect(geometry.top + geometry.height).toBe(200);
  });

  test("scrolled halfway — thumb halfway down the track", () => {
    const geometry = computeThumbGeometry({ scrollTop: 400, scrollHeight: 1000, clientHeight: 200 });
    // track range = 200 - 40 = 160; halfway through maxScrollTop (800) -> 80
    expect(geometry.top).toBe(80);
  });

  test("very long content clamps the thumb to the minimum height, not a sliver", () => {
    // Naive proportional height here would be (200/10000)*200 = 4px.
    const geometry = computeThumbGeometry({ scrollTop: 0, scrollHeight: 10000, clientHeight: 200 });
    expect(geometry.height).toBe(MIN_THUMB_HEIGHT);
  });

  test("minimum-height clamp still respects the shrunken track range for offset", () => {
    const geometry = computeThumbGeometry({ scrollTop: 4900, scrollHeight: 10000, clientHeight: 200 });
    // track range = 200 - 24 = 176; halfway through maxScrollTop (9800) -> 88
    expect(geometry.height).toBe(MIN_THUMB_HEIGHT);
    expect(geometry.top).toBe(88);
  });

  test("negative overscroll (rubber-banding above the top) clamps to 0, not off-track", () => {
    const geometry = computeThumbGeometry({ scrollTop: -50, scrollHeight: 1000, clientHeight: 200 });
    expect(geometry.top).toBe(0);
  });

  test("overscroll past the bottom clamps to the track's far edge", () => {
    const geometry = computeThumbGeometry({ scrollTop: 850, scrollHeight: 1000, clientHeight: 200 });
    // maxScrollTop is 800, so 850 is 50px of rubber-band past it.
    expect(geometry.top).toBe(160);
  });

  test("thumb height never exceeds the track itself", () => {
    // Only barely overflowing: proportional height would be close to
    // clientHeight already, must not be pushed past it by the min clamp.
    const geometry = computeThumbGeometry({ scrollTop: 0, scrollHeight: 201, clientHeight: 200 });
    expect(geometry.height).toBeLessThanOrEqual(200);
  });
});
