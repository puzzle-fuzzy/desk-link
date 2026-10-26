import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { inputRestrictionPresentation, restrictionAfterRuntime } from "./input-restriction";
import type { ControllerRuntimeSummary } from "./types";

test("distinct input restrictions give actionable guidance and None clears the message", () => {
  expect(inputRestrictionPresentation("blocked")?.detail).toContain("本机处理");
  expect(inputRestrictionPresentation("unsupportedKey")?.title).toContain("按键");
  expect(inputRestrictionPresentation("invalidInput")?.title).toContain("输入");
  expect(inputRestrictionPresentation(null)).toBeNull();
});

test("old restrictions cannot survive a disconnect or new stream generation", () => {
  const connected = {state: "connected", streamId: 1} as ControllerRuntimeSummary;
  expect(restrictionAfterRuntime("blocked", connected, connected)).toBe("blocked");
  expect(restrictionAfterRuntime("blocked", connected, {...connected, streamId: 2})).toBeNull();
  expect(restrictionAfterRuntime("blocked", connected, {...connected, state: "reconnecting"})).toBeNull();
});

test("actual InputState signal patches its status region without touching the remote session", () => {
  const source = readFileSync(new URL("./controller.ts", import.meta.url), "utf8");
  const start = source.indexOf('    case "inputRestriction":', source.indexOf("function handleSignal("));
  const branch = source.slice(start, source.indexOf('    case "status":', start));
  const updates: unknown[] = [];
  const execute = new Function("signal", "update", `let inputRestrictionReason = null; const updateInputRestriction = () => update(inputRestrictionReason); switch(signal.kind) { ${branch} }`);
  execute({kind: "inputRestriction", reason: "blocked"}, (value: unknown) => updates.push(value));
  execute({kind: "inputRestriction", reason: null}, (value: unknown) => updates.push(value));
  expect(updates).toEqual(["blocked", null]);
});
