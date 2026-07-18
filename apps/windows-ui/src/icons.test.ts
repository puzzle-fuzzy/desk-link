import { describe, expect, test } from "bun:test";

import { icon } from "./icons";

describe("Lucide icon placeholders", () => {
  test("renders a library-owned icon name with shared styling and accessibility", () => {
    expect(icon("shield-check", "security-note-mark")).toBe(
      '<i data-lucide="shield-check" class="app-icon security-note-mark" aria-hidden="true"></i>',
    );
  });

  test("does not emit handwritten SVG markup", () => {
    const markup = icon("monitor-check");
    expect(markup).not.toContain("<svg");
    expect(markup).not.toContain("<path");
  });
});
