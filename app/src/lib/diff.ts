// Parses a GitHub-style unified diff `patch` string (CommitFile.patch) into
// per-line records with running old/new line numbers, for the detail pane's
// Files tab (docs/CONTRACT.md "Reading in-app": "the patch as a unified
// diff: line numbers, additions ... tinted, deletions ... tinted, context
// plain").

export type DiffLineType = "context" | "add" | "del" | "hunk";

export interface DiffLine {
  type: DiffLineType;
  /** The line's text with its leading +/-/space marker stripped; the raw
   * "@@ ... @@" text itself for a `hunk` line. */
  text: string;
  /** The line number to show in the gutter: the old-file number for
   * `context`/`del`, the new-file number for `add`, null for a hunk header. */
  lineNumber: number | null;
  /** The new-file line number, independent of what `lineNumber` shows in the
   * gutter — added for the detail pane's Files tab, which needs to match a
   * review comment's `line` (always a new-file number, per GitHub's API)
   * against a rendered diff line regardless of its type. `context` and `add`
   * both exist in the new file, so both get one; `del` and `hunk` don't. */
  newLineNumber: number | null;
}

const HUNK_HEADER = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/;

/** `patch.split("\n")` leaves a trailing empty element when the string ends
 * in a newline (GitHub's patches usually don't, but this stays correct
 * either way rather than rendering a stray blank row). */
function patchLines(patch: string): string[] {
  const lines = patch.split("\n");
  if (lines.length > 0 && lines[lines.length - 1] === "") lines.pop();
  return lines;
}

export function parseDiff(patch: string): DiffLine[] {
  let oldLine = 0;
  let newLine = 0;
  const out: DiffLine[] = [];
  for (const raw of patchLines(patch)) {
    const hunk = raw.match(HUNK_HEADER);
    if (hunk) {
      oldLine = Number(hunk[1]);
      newLine = Number(hunk[2]);
      out.push({ type: "hunk", text: raw, lineNumber: null, newLineNumber: null });
      continue;
    }
    if (raw.startsWith("\\")) {
      // "\ No newline at end of file" — a marker, not a line of the file;
      // it must not advance either counter or it throws off every
      // subsequent line's gutter number.
      continue;
    }
    if (raw.startsWith("+")) {
      out.push({ type: "add", text: raw.slice(1), lineNumber: newLine, newLineNumber: newLine });
      newLine++;
    } else if (raw.startsWith("-")) {
      out.push({ type: "del", text: raw.slice(1), lineNumber: oldLine, newLineNumber: null });
      oldLine++;
    } else {
      out.push({
        type: "context",
        text: raw.startsWith(" ") ? raw.slice(1) : raw,
        lineNumber: oldLine,
        newLineNumber: newLine,
      });
      oldLine++;
      newLine++;
    }
  }
  return out;
}
