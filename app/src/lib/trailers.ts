// Git trailer parsing for commit messages (types.ts's `Event.body` /
// `CommitDetail.message`): pulls the `Co-Authored-By:` / `Claude-Session:`
// style footer an AI coding agent appends off a commit message, and matches
// a `Co-Authored-By` value against a fixed table of known agents so the
// detail pane and feed rows can show a brand-coloured chip instead of raw
// trailer text.

export interface Trailer {
  key: string;
  value: string;
}

export interface Agent {
  /** Stable id, e.g. "claude". */
  id: string;
  /** Short family name for a compact chip — "Claude", "ChatGPT", "Gemini",
   * "Kimi", "Grok", "Copilot", "Cursor". */
  family: string;
  /** The co-author's own name from the trailer, e.g. "Claude Fable 5.1" —
   * what a full (non-compact) chip labels itself with. */
  label: string;
  /** Brand colour: the chip's dot/text colour, and (at ~14% alpha) its
   * background. app.css (its own header comment) is a single, light-only
   * theme — the app never reacts to a dark system setting — so this is the
   * one value each agent needs; there is no dark variant to pick between. */
  color: string;
  /** Key into `agent-icons.ts`'s `AGENT_ICONS`, when a bundled brand glyph
   * exists for this agent — `undefined` for an agent with no icon entry,
   * which AgentMark falls back to the family's first letter for. */
  icon?: string;
}

export interface AgentRule {
  id: string;
  family: string;
  color: string;
  icon?: string;
  /** `domain` is the lowercased part of the email after `@` (empty when the
   * trailer value carried no `<...>` address). */
  match(name: string, domain: string): boolean;
}

// Fixed agent table (packet: "colours as constants in trailers.ts, not CSS
// classes per agent"). A domain that's exclusive to one product (anthropic.com,
// openai.com, moonshot*, x.ai, cursor.com/.sh, deepseek.com, mistral.ai) is
// enough on its own; a domain real humans also use for everyday mail
// (google.com, github.com) additionally requires the agent's name in the
// co-author line.
// Exported for trailers.test.ts's "every rule's icon exists in
// agent-icons.ts" coverage test — not used as an editable table anywhere
// else, `detectAgent` below is still the one real entry point.
export const AGENT_RULES: AgentRule[] = [
  {
    id: "claude",
    family: "Claude",
    color: "#d97757",
    icon: "claude",
    match: (name, domain) => domain === "anthropic.com" || /^claude\b/i.test(name),
  },
  {
    id: "chatgpt",
    family: "ChatGPT",
    color: "#10a37f",
    icon: "openai",
    match: (name, domain) => domain === "openai.com" || /\b(chatgpt|codex)\b/i.test(name),
  },
  {
    id: "gemini",
    family: "Gemini",
    color: "#4285f4",
    icon: "googlegemini",
    match: (name, domain) => domain === "google.com" && /\b(gemini|jules)\b/i.test(name),
  },
  {
    id: "kimi",
    family: "Kimi",
    color: "#1f1f1f",
    icon: "kimi",
    match: (name, domain) => domain.includes("moonshot") || /\bkimi\b/i.test(name),
  },
  {
    id: "grok",
    family: "Grok",
    color: "#000000",
    icon: "x",
    match: (name, domain) => domain === "x.ai" || /\bgrok\b/i.test(name),
  },
  {
    id: "copilot",
    family: "Copilot",
    color: "#8957e5",
    icon: "githubcopilot",
    match: (name, domain) => domain === "github.com" && /\bcopilot\b/i.test(name),
  },
  {
    id: "cursor",
    family: "Cursor",
    color: "#000000",
    icon: "cursor",
    match: (name, domain) => domain === "cursor.com" || domain === "cursor.sh" || /\bcursor\b/i.test(name),
  },
  {
    id: "deepseek",
    family: "DeepSeek",
    color: "#4d6bfe",
    icon: "deepseek",
    match: (name, domain) => domain === "deepseek.com" || /\bdeepseek\b/i.test(name),
  },
  {
    id: "mistral",
    family: "Mistral",
    color: "#fa520f",
    icon: "mistralai",
    match: (name, domain) => domain === "mistral.ai" || /\bmistral\b/i.test(name),
  },
];

/** Splits a git `Co-authored-by`-style value, "Name <email>", into its
 * parts. Falls back to the whole string as `name` when there's no
 * `<...>` address (git requires one, but nothing here depends on it). */
export function parseCoAuthor(value: string): { name: string; email: string } {
  const m = value.trim().match(/^(.*?)<([^<>]*)>\s*$/);
  return {
    name: (m ? m[1] : value).trim(),
    email: (m ? m[2] : "").trim().toLowerCase(),
  };
}

/** Matches a `Co-Authored-By` trailer value against the fixed agent table,
 * by email domain and/or name. `null` for a human co-author or anything the
 * table doesn't recognise. */
export function detectAgent(coAuthor: string): Agent | null {
  const { name, email } = parseCoAuthor(coAuthor);
  const domain = email.includes("@") ? email.slice(email.indexOf("@") + 1) : email;
  for (const rule of AGENT_RULES) {
    if (rule.match(name, domain)) {
      return { id: rule.id, family: rule.family, label: name || rule.family, color: rule.color, icon: rule.icon };
    }
  }
  return null;
}

const TRAILER_LINE = /^([A-Za-z0-9-]+):[ \t]+(\S.*)$/;

/** Splits a commit message into its body and trailing `Key: value` block
 * (simplified git trailer rules: the last paragraph, every line matching
 * `Token: value`; multi-line continuation is not required).
 *
 * A last paragraph of exactly one line only counts as a trailer block when
 * its token is hyphenated (`Co-Authored-By`, `Claude-Session`, `Signed-off-by`,
 * …) — real trailer keys read that way, while a lone "Word: sentence." line
 * (e.g. "Note: see the issue for details.") is ordinary prose that happens
 * to contain a colon, and is left in `body` untouched. Two or more
 * trailer-shaped lines together are unambiguous regardless of hyphenation. */
export function splitTrailers(message: string): { body: string; trailers: Trailer[] } {
  const lines = message.replace(/\r\n/g, "\n").split("\n");

  let end = lines.length - 1;
  while (end >= 0 && lines[end].trim() === "") end--;
  if (end < 0) return { body: "", trailers: [] };

  let start = end;
  while (start > 0 && lines[start - 1].trim() !== "") start--;
  const paragraph = lines.slice(start, end + 1);

  const matches = paragraph.map((l) => l.match(TRAILER_LINE));
  const isTrailerBlock =
    matches.every((m): m is RegExpMatchArray => m !== null) &&
    (paragraph.length > 1 || matches[0]![1].includes("-"));

  if (!isTrailerBlock) {
    return { body: lines.slice(0, end + 1).join("\n").trimEnd(), trailers: [] };
  }

  let bodyEnd = start;
  while (bodyEnd > 0 && lines[bodyEnd - 1].trim() === "") bodyEnd--;
  const body = lines.slice(0, bodyEnd).join("\n").trimEnd();
  const trailers = matches.map((m) => ({ key: m![1], value: m![2].trim() }));
  return { body, trailers };
}

/** Collapses whitespace and clamps to `max` characters. Mirrors the Rust
 * engine's own `preview()` (crates/gitmon/src/poll.rs) so a preview built
 * client-side from a trailer-stripped commit body reads about the same
 * length as the server-truncated `body_preview` it stands in for. */
export function collapsePreview(text: string, max = 200): string {
  const collapsed = text.split(/\s+/).filter(Boolean).join(" ");
  return collapsed.length > max ? collapsed.slice(0, max) : collapsed;
}
