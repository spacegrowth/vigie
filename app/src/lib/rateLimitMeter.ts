// Pure formatting behind StatusBar.svelte's API-budget meter: a thin track
// beside the request count, always visible once the app has a reading, that
// answers "how am I doing on GitHub's budget" without looking alarming at a
// glance and without hiding the number that used to be the whole story.
// Kept out of the component so the thresholds and wording are
// unit-testable without mounting it — see rateLimitMeter.test.ts.

/** GitHub's documented default when a response carries no `x-ratelimit-limit`
 * header at all — an authenticated request's hourly budget. Used only as a
 * fallback for computing the meter's fill; the count itself is always the
 * real `remaining`, header or no header. */
export const DEFAULT_RATE_LIMIT = 5000;

/** Below this fraction of the budget, the fill takes `--warn`. */
const WARN_FRACTION = 0.25;
/** Below this fraction, the fill takes `--danger`. Checked after `WARN_FRACTION`
 * so a reading under both lands on the more severe one. */
const DANGER_FRACTION = 0.1;

export type MeterTone = "muted" | "warn" | "danger";

export interface RateLimitMeter {
  /** 0..1 — the fill's share of the track's fixed width. */
  fraction: number;
  /** Which status colour the fill takes; `"muted"` means the ordinary quiet
   * gauge colour, not a status colour at all. */
  tone: MeterTone;
  /** "4,019" — sits beside the track in `--muted` text, so the state reads
   * without colour. */
  countText: string;
  /** The full tooltip: whose budget this is, the count against the limit, and
   * the countdown — the countdown lives here and nowhere else in the DOM. */
  title: string;
}

/** Everything StatusBar.svelte's meter needs from one `PollResult`/
 * `BackfillResult` reading, at the moment `now`. `limit` and `resetAt` are
 * `null` exactly when the response carried no matching header — a limit
 * falls back to `DEFAULT_RATE_LIMIT` for the fill math (GitHub omits it far
 * less often than it omits the reset time), and a missing reset just leaves
 * the countdown out of the tooltip rather than guessing one.
 *
 * Callers only call this once they have a `remaining` reading at all — with
 * none yet, StatusBar.svelte renders nothing rather than an empty track. */
export function rateLimitMeter(
  remaining: number,
  limit: number | null,
  resetAt: number | null,
  now: number,
): RateLimitMeter {
  const cap = limit != null && limit > 0 ? limit : DEFAULT_RATE_LIMIT;
  const fraction = Math.min(1, Math.max(0, remaining / cap));
  const tone: MeterTone =
    fraction < DANGER_FRACTION ? "danger" : fraction < WARN_FRACTION ? "warn" : "muted";
  const countText = remaining.toLocaleString();
  const resetText = formatResetIn(resetAt, now);
  // The fill needs a denominator, so `cap` falls back to the usual 5,000 —
  // but the sentence must not state a fallback as an observed fact. Only say
  // "of N" when GitHub actually told us what N is.
  const observedLimit = limit != null && limit > 0;
  const title =
    `GitHub's hourly budget for this account · ${countText}` +
    (observedLimit ? ` of ${cap.toLocaleString()}` : "") +
    " left" +
    (resetText ? ` · ${resetText}` : "");
  return { fraction, tone, countText, title };
}

/** "resets in 23 min" / "resets in 1 min" / "resets in under a minute" /
 * "resets any moment" (once the reset time has technically passed but the
 * next poll hasn't caught up yet) / `""` when `resetAt` is `null` — GitHub
 * sent no `x-ratelimit-reset` to say when the budget refills. */
export function formatResetIn(resetAt: number | null, now: number): string {
  if (resetAt == null) return "";
  const diffSecs = resetAt - now;
  if (diffSecs <= 0) return "resets any moment";
  if (diffSecs < 60) return "resets in under a minute";
  const mins = Math.round(diffSecs / 60);
  return `resets in ${mins} min`;
}
