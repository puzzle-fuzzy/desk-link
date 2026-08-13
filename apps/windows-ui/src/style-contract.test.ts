import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const root = resolve(import.meta.dir);
const canonicalFont =
  'v-sans, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif, "Apple Color Emoji", "Segoe UI Emoji", "Segoe UI Symbol"';

describe("Windows product style contract", () => {
  test("keeps every root theme on the canonical Windows font stack", () => {
    const source = [
      readFileSync(resolve(root, "styles.css"), "utf8"),
      readFileSync(resolve(root, "product-ui.css"), "utf8"),
    ].join("\n");
    expect(source).toContain("font-family: " + canonicalFont + ";");
    expect(source).not.toMatch(/font-family:\s*(?:Inter|["']Segoe UI Variable Text["'])/);
  });

  test("keeps accessibility resilience in the active product stylesheet", () => {
    const source = readFileSync(resolve(root, "product-ui.css"), "utf8");
    expect(source).toContain("@media (forced-colors: active)");
    expect(source).toContain("outline: 2px solid Highlight");
    expect(source).toContain("background: Highlight");
    expect(source).toContain("color: HighlightText");
    expect(source).toContain("@media (prefers-reduced-motion: reduce)");
  });

  test("keeps wide-screen typography and layout contracts", () => {
    const source = readFileSync(resolve(root, "product-ui.css"), "utf8");
    expect(source).toContain("@media (min-width: 1920px)");
    expect(source).toContain("font-size: 16px;");
    expect(source).toContain("grid-template-columns: 248px minmax(0, 1fr)");
    expect(source).toContain("width: min(100%, 1360px)");
  });

  test("keeps user-controlled identity text wrap-safe", () => {
    const source = readFileSync(resolve(root, "styles.css"), "utf8");
    expect(source).toContain(".approval-fingerprint");
    expect(source).toContain("overflow-wrap: anywhere");
    expect(source).toContain(".saved-device-public-id");
    expect(source).toContain("text-overflow: ellipsis");
  });
});
