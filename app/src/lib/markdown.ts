// A small, dependency-free markdown-to-HTML renderer for the detail pane
// (docs/CONTRACT.md: PR/issue bodies, comments, reviews and commit messages
// are markdown; the packet calls for "a ~200-line markdown-to-HTML with
// escaping" rather than pulling in a `marked`-sized library). Escapes raw
// HTML by default — nothing in the input is ever treated as trusted markup.
//
// Supports: headers (# .. ######), fenced code blocks (```), blockquotes
// (>), unordered/ordered lists, bold/italic, inline code, links, and plain
// paragraphs with soft line breaks. Anything not recognized falls through
// as an escaped paragraph — never as raw HTML.

function escapeHtml(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}

/** Applies inline formatting to an already-HTML-escaped line: inline code
 * first (so markup inside `code` isn't itself interpreted), then links,
 * then bold, then italic. */
function renderInline(escaped: string): string {
  // Inline code: `...`
  let out = escaped.replace(/`([^`]+)`/g, (_m, code) => `<code>${code}</code>`);
  // Links: [text](url) — only http(s) URLs are ever emitted as href.
  // `renderInline` only ever runs on text `escapeHtml` has already passed
  // over (every caller below escapes first), so a literal quote in the
  // original URL already arrived here as `&quot;` — escaping again would
  // double-encode it and corrupt the link.
  out = out.replace(/\[([^\]]+)\]\((https?:\/\/[^\s)]+)\)/g, (_m, text, url) => `<a href="${url}">${text}</a>`);
  // Bold: **text** or __text__
  out = out.replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>");
  out = out.replace(/__([^_]+)__/g, "<strong>$1</strong>");
  // Italic: *text* or _text_ (after bold, so ** is already consumed)
  out = out.replace(/\*([^*]+)\*/g, "<em>$1</em>");
  out = out.replace(/(^|[^\w])_([^_]+)_(?!\w)/g, "$1<em>$2</em>");
  return out;
}

interface ListState {
  ordered: boolean;
  items: string[];
}

export function renderMarkdown(source: string | null | undefined): string {
  if (!source) return "";
  const lines = source.replace(/\r\n/g, "\n").split("\n");

  const out: string[] = [];
  let para: string[] = [];
  let list: ListState | null = null;
  let quote: string[] = [];
  let inFence = false;
  let fenceLines: string[] = [];
  let fenceLang = "";

  function flushParagraph() {
    if (para.length > 0) {
      out.push(`<p>${renderInline(para.join(" "))}</p>`);
      para = [];
    }
  }
  function flushList() {
    if (list) {
      const tag = list.ordered ? "ol" : "ul";
      out.push(`<${tag}>${list.items.map((i) => `<li>${renderInline(i)}</li>`).join("")}</${tag}>`);
      list = null;
    }
  }
  function flushQuote() {
    if (quote.length > 0) {
      out.push(`<blockquote>${renderInline(quote.join(" "))}</blockquote>`);
      quote = [];
    }
  }
  function flushAll() {
    flushParagraph();
    flushList();
    flushQuote();
  }

  for (const rawLine of lines) {
    const fence = rawLine.match(/^```\s*(\S*)\s*$/);
    if (fence) {
      if (!inFence) {
        flushAll();
        inFence = true;
        fenceLang = fence[1] ?? "";
        fenceLines = [];
      } else {
        const cls = fenceLang ? ` class="language-${escapeHtml(fenceLang)}"` : "";
        out.push(`<pre><code${cls}>${fenceLines.map(escapeHtml).join("\n")}</code></pre>`);
        inFence = false;
      }
      continue;
    }
    if (inFence) {
      fenceLines.push(rawLine);
      continue;
    }

    const line = escapeHtml(rawLine);
    const trimmed = rawLine.trim();

    if (trimmed === "") {
      flushAll();
      continue;
    }

    const header = trimmed.match(/^(#{1,6})\s+(.*)$/);
    if (header) {
      flushAll();
      const level = header[1].length;
      out.push(`<h${level}>${renderInline(escapeHtml(header[2]))}</h${level}>`);
      continue;
    }

    const quoteLine = trimmed.match(/^>\s?(.*)$/);
    if (quoteLine) {
      flushParagraph();
      flushList();
      quote.push(escapeHtml(quoteLine[1]));
      continue;
    }
    flushQuote();

    const bullet = trimmed.match(/^[-*]\s+(.*)$/);
    const numbered = trimmed.match(/^\d+[.)]\s+(.*)$/);
    if (bullet || numbered) {
      flushParagraph();
      const ordered = !!numbered;
      const text = escapeHtml((bullet ?? numbered)![1]);
      if (!list || list.ordered !== ordered) {
        flushList();
        list = { ordered, items: [] };
      }
      list.items.push(text);
      continue;
    }
    flushList();

    // A horizontal rule on its own line.
    if (/^(-{3,}|\*{3,}|_{3,})$/.test(trimmed)) {
      flushParagraph();
      out.push("<hr>");
      continue;
    }

    para.push(line);
  }

  if (inFence) {
    // Unterminated fence — still render what we have rather than drop it.
    out.push(`<pre><code>${fenceLines.map(escapeHtml).join("\n")}</code></pre>`);
  }
  flushAll();

  return out.join("\n");
}
