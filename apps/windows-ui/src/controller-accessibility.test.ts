import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const source = readFileSync(resolve(import.meta.dir, "controller.ts"), "utf8");

describe("Windows controller accessibility wiring", () => {
  test("associates the transfer toolbar control with its panel", () => {
    expect(source).toContain('data-controller-transfer aria-expanded="${transferPanelOpen}" aria-controls="remote-transfer-panel"');
    expect(source).toContain('id="remote-transfer-panel" class="remote-transfer-panel"');
  });
});
