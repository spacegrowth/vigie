// Relative-time formatting for event rows. Unix seconds in, short strings out.

export function relativeTime(unixSeconds: number, now: number = Date.now() / 1000): string {
  const diff = Math.max(0, Math.round(now - unixSeconds));
  if (diff < 60) return "just now";
  const mins = Math.round(diff / 60);
  if (mins < 60) return `${mins}m ago`;
  const hours = Math.round(mins / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.round(hours / 24);
  if (days < 7) return `${days}d ago`;
  const weeks = Math.round(days / 7);
  if (weeks < 5) return `${weeks}w ago`;
  const months = Math.round(days / 30);
  if (months < 12) return `${months}mo ago`;
  const years = Math.round(days / 365);
  return `${years}y ago`;
}

/** Same buckets as `relativeTime` but without the trailing " ago" — the
 * mockups use this bare form ("4m", "1h", "2d") for a row's trailing
 * timestamp column, reserving "…ago" for descriptive sentences elsewhere
 * ("priya · opened 4m ago"). */
export function shortRelativeTime(unixSeconds: number, now: number = Date.now() / 1000): string {
  const diff = Math.max(0, Math.round(now - unixSeconds));
  if (diff < 60) return "now";
  const mins = Math.round(diff / 60);
  if (mins < 60) return `${mins}m`;
  const hours = Math.round(mins / 60);
  if (hours < 24) return `${hours}h`;
  const days = Math.round(hours / 24);
  if (days < 7) return `${days}d`;
  const weeks = Math.round(days / 7);
  if (weeks < 5) return `${weeks}w`;
  const months = Math.round(days / 30);
  if (months < 12) return `${months}mo`;
  const years = Math.round(days / 365);
  return `${years}y`;
}

export function absoluteTime(unixSeconds: number): string {
  return new Date(unixSeconds * 1000).toLocaleString(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  });
}

/** Day-bucket label used to group the feed: "Today", "Yesterday", or a date. */
export function dayLabel(unixSeconds: number, now: Date = new Date()): string {
  const d = new Date(unixSeconds * 1000);
  const startOfDay = (dt: Date) => new Date(dt.getFullYear(), dt.getMonth(), dt.getDate()).getTime();
  const diffDays = Math.round((startOfDay(now) - startOfDay(d)) / 86_400_000);
  if (diffDays === 0) return "Today";
  if (diffDays === 1) return "Yesterday";
  return d.toLocaleDateString(undefined, { month: "short", day: "numeric", year: diffDays > 300 ? "numeric" : undefined });
}

/** Wall-clock time only, no seconds and no date: "14:32" or "2:32 PM"
 * depending on the locale. Used where a message names a moment later today —
 * the rate-limit banner's "Polling resumes at …" — and where
 * `absoluteTime`'s date half would only be noise. */
export function clockTime(unixSeconds: number): string {
  return new Date(unixSeconds * 1000).toLocaleTimeString(undefined, {
    hour: "numeric",
    minute: "2-digit",
  });
}
