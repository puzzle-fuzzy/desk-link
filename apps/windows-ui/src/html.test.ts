import { describe, expect, test } from "bun:test";

import { escapeHtml } from "./html";

describe("HTML text escaping", () => {
  test("escapes every character that can leave a text context", () => {
    expect(escapeHtml(`<script data-name="DeskLink">'连接' & 控制</script>`)).toBe(
      "&lt;script data-name=&quot;DeskLink&quot;&gt;&#039;连接&#039; &amp; 控制&lt;/script&gt;",
    );
  });

  test("preserves Chinese, emoji and long diagnostic text", () => {
    const value = `远程连接 🖥️ ${"状态".repeat(1_000)}`;
    expect(escapeHtml(value)).toBe(value);
  });
});
