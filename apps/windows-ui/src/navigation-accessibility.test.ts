import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const source = readFileSync(resolve(import.meta.dir, "main.ts"), "utf8");

describe("Windows navigation accessibility semantics", () => {
  test("exposes the workspace as the main landmark", () => {
    expect(source).toContain('<main class="workspace ');
    expect(source).toContain("</main>");
  });

  test("identifies the utility control and menu relationship", () => {
    expect(source).toContain('aria-haspopup="menu" aria-expanded="${utilityMenuOpen}" aria-controls="utility-menu"');
    expect(source).toContain('role="menu" aria-label="更多功能" aria-orientation="vertical"');
  });
});
