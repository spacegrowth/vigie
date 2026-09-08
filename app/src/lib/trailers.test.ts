import { describe, expect, test } from "vitest";
import { AGENT_ICONS } from "./agent-icons";
import { AGENT_RULES, collapsePreview, detectAgent, parseCoAuthor, splitTrailers } from "./trailers";

describe("splitTrailers", () => {
  test("a message with no trailers returns the body unchanged", () => {
    const message = "Fix the flaky poll test\n\nThe retry loop double-counted its own attempts.";
    expect(splitTrailers(message)).toEqual({ body: message, trailers: [] });
  });

  test("a message with a two-line trailer block splits body from trailers", () => {
    const message =
      "Fix the flaky poll test\n\n" +
      "The retry loop double-counted its own attempts.\n\n" +
      "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>\n" +
      "Claude-Session: https://claude.ai/code/session_018T71Pe2CMbyqPUGibUoBEH";
    expect(splitTrailers(message)).toEqual({
      body: "Fix the flaky poll test\n\nThe retry loop double-counted its own attempts.",
      trailers: [
        { key: "Co-Authored-By", value: "Claude Fable 5.1 <noreply@anthropic.com>" },
        { key: "Claude-Session", value: "https://claude.ai/code/session_018T71Pe2CMbyqPUGibUoBEH" },
      ],
    });
  });

  test("a single hyphenated trailer line is still recognised on its own", () => {
    const message = "Tweak the retry backoff\n\nCo-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>";
    expect(splitTrailers(message)).toEqual({
      body: "Tweak the retry backoff",
      trailers: [{ key: "Co-Authored-By", value: "Claude Fable 5.1 <noreply@anthropic.com>" }],
    });
  });

  test("a last paragraph that merely contains a colon is not treated as trailers", () => {
    const message = "Fixed the bug.\n\nNote: see the linked issue for the repro steps.";
    expect(splitTrailers(message)).toEqual({ body: message, trailers: [] });
  });

  test("a message that is only a title has no trailers", () => {
    expect(splitTrailers("Bump version to 1.2.3")).toEqual({ body: "Bump version to 1.2.3", trailers: [] });
  });

  test("an empty message returns an empty body and no trailers", () => {
    expect(splitTrailers("")).toEqual({ body: "", trailers: [] });
  });
});

describe("detectAgent", () => {
  test("Claude — anthropic.com domain", () => {
    expect(detectAgent("Claude Fable 5.1 <noreply@anthropic.com>")).toMatchObject({ id: "claude", family: "Claude", color: "#d97757" });
  });

  test("Claude — name starts with Claude on another domain", () => {
    expect(detectAgent("Claude <claude@example.com>")).toMatchObject({ id: "claude" });
  });

  test("ChatGPT/Codex — openai.com domain", () => {
    expect(detectAgent("Codex <noreply@openai.com>")).toMatchObject({ id: "chatgpt", family: "ChatGPT", color: "#10a37f" });
  });

  test("ChatGPT — name contains ChatGPT", () => {
    expect(detectAgent("ChatGPT <bot@example.com>")).toMatchObject({ id: "chatgpt" });
  });

  test("Gemini — google.com domain with gemini in the name", () => {
    expect(detectAgent("Gemini <noreply@google.com>")).toMatchObject({ id: "gemini", family: "Gemini", color: "#4285f4" });
  });

  test("Jules — google.com domain with jules in the name", () => {
    expect(detectAgent("Jules <jules@google.com>")).toMatchObject({ id: "gemini" });
  });

  test("a plain google.com human co-author does not match Gemini", () => {
    expect(detectAgent("Priya Shah <priya@google.com>")).toBeNull();
  });

  test("Kimi — moonshot domain", () => {
    expect(detectAgent("Kimi <noreply@moonshot.cn>")).toMatchObject({ id: "kimi", family: "Kimi", color: "#1f1f1f" });
  });

  test("Kimi — name contains Kimi", () => {
    expect(detectAgent("Kimi K2 <bot@example.com>")).toMatchObject({ id: "kimi" });
  });

  test("Grok — x.ai domain", () => {
    expect(detectAgent("Grok <noreply@x.ai>")).toMatchObject({ id: "grok", family: "Grok", color: "#000000" });
  });

  test("Grok — name contains Grok", () => {
    expect(detectAgent("Grok 4 <bot@example.com>")).toMatchObject({ id: "grok" });
  });

  test("GitHub Copilot — github.com domain with Copilot in the name", () => {
    expect(detectAgent("Copilot <noreply@github.com>")).toMatchObject({ id: "copilot", family: "Copilot", color: "#8957e5" });
  });

  test("a plain github.com human co-author does not match Copilot", () => {
    expect(detectAgent("Marcus Lee <marcus@github.com>")).toBeNull();
  });

  test("Cursor — cursor.com domain", () => {
    expect(detectAgent("Cursor <noreply@cursor.com>")).toMatchObject({ id: "cursor", family: "Cursor", color: "#000000" });
  });

  test("Cursor — cursor.sh domain", () => {
    expect(detectAgent("Cursor Agent <noreply@cursor.sh>")).toMatchObject({ id: "cursor" });
  });

  test("an unknown human co-author resolves to null", () => {
    expect(detectAgent("Priya Shah <priya@example.com>")).toBeNull();
  });

  test("a co-author with no matching product resolves to null", () => {
    expect(detectAgent("Some Bot <bot@nowhere.example>")).toBeNull();
  });

  test("DeepSeek — deepseek.com domain", () => {
    expect(detectAgent("DeepSeek <noreply@deepseek.com>")).toMatchObject({ id: "deepseek", family: "DeepSeek", color: "#4d6bfe" });
  });

  test("DeepSeek — name contains DeepSeek", () => {
    expect(detectAgent("DeepSeek V3 <bot@example.com>")).toMatchObject({ id: "deepseek" });
  });

  test("Mistral — mistral.ai domain", () => {
    expect(detectAgent("Mistral <noreply@mistral.ai>")).toMatchObject({ id: "mistral", family: "Mistral", color: "#fa520f" });
  });

  test("Mistral — name contains Mistral", () => {
    expect(detectAgent("Mistral Agent <bot@example.com>")).toMatchObject({ id: "mistral" });
  });
});

describe("detectAgent — icon resolution", () => {
  test("Claude resolves to the bundled claude icon key", () => {
    expect(detectAgent("Claude Fable 5.1 <noreply@anthropic.com>")).toMatchObject({ icon: "claude" });
  });

  test("ChatGPT/Codex resolves to the bundled openai icon key", () => {
    expect(detectAgent("Codex <noreply@openai.com>")).toMatchObject({ icon: "openai" });
  });

  test("Gemini resolves to the bundled googlegemini icon key", () => {
    expect(detectAgent("Gemini <noreply@google.com>")).toMatchObject({ icon: "googlegemini" });
  });

  test("Grok resolves to the bundled x icon key", () => {
    expect(detectAgent("Grok <noreply@x.ai>")).toMatchObject({ icon: "x" });
  });

  test("DeepSeek resolves to the bundled deepseek icon key", () => {
    expect(detectAgent("DeepSeek <noreply@deepseek.com>")).toMatchObject({ icon: "deepseek" });
  });

  test("Mistral resolves to the bundled mistralai icon key", () => {
    expect(detectAgent("Mistral <noreply@mistral.ai>")).toMatchObject({ icon: "mistralai" });
  });

  test("every agent rule's icon (when set) exists in AGENT_ICONS", () => {
    for (const rule of AGENT_RULES) {
      if (rule.icon === undefined) continue;
      expect(AGENT_ICONS, `rule "${rule.id}" names icon "${rule.icon}"`).toHaveProperty(rule.icon);
    }
  });
});

describe("parseCoAuthor", () => {
  test("splits name and lowercased email", () => {
    expect(parseCoAuthor("Claude Fable 5.1 <Noreply@Anthropic.com>")).toEqual({
      name: "Claude Fable 5.1",
      email: "noreply@anthropic.com",
    });
  });

  test("falls back to the whole value as name when there's no <...>", () => {
    expect(parseCoAuthor("Priya Shah")).toEqual({ name: "Priya Shah", email: "" });
  });
});

describe("collapsePreview", () => {
  test("collapses internal whitespace", () => {
    expect(collapsePreview("a\n\n b   c")).toBe("a b c");
  });

  test("clamps to the given length", () => {
    expect(collapsePreview("x".repeat(500), 200)).toHaveLength(200);
  });

  test("leaves short text alone", () => {
    expect(collapsePreview("short body")).toBe("short body");
  });
});
