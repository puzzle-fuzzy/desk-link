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
});
