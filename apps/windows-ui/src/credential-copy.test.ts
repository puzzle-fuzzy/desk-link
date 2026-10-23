import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const source = readFileSync(resolve(import.meta.dir, "main.ts"), "utf8");
const start = source.indexOf("async function copyCredential(");
const end = source.indexOf("\nasync function revokeController(", start);
if (start < 0 || end < 0) throw new Error("Unable to locate credential clipboard operation");
const operation = new Bun.Transpiler({ loader: "ts" }).transformSync(source.slice(start, end));

function clipboardHost(primaryAvailable: boolean, fallbackAllowed = true, sourceAvailable = true) {
  const copied: string[] = [];
  const temporaryInputs: Array<{ value: string; removed: boolean }> = [];
  let selectedValue = "";
  let focusRestored = false;
  class FocusedElement {
    isConnected = true;
    focus() { focusRestored = true; }
  }
  const environment = {
    navigator: { clipboard: { writeText: async (value: string) => {
      if (!primaryAvailable) throw new Error("Clipboard API unavailable");
      copied.push(value);
    } } },
    document: {
      activeElement: new FocusedElement(),
      getElementById: () => sourceAvailable ? { textContent: "123 456 789 012" } : null,
      createElement: () => {
        const input = {
          value: "", removed: false, readOnly: false, tabIndex: 0, style: {},
          setAttribute: () => {},
          select() { selectedValue = input.value; },
          remove() { input.removed = true; },
        };
        temporaryInputs.push(input);
        return input;
      },
      body: { append: () => {} },
      execCommand: () => {
        if (fallbackAllowed) copied.push(selectedValue);
        return fallbackAllowed;
      },
    },
    HTMLElement: FocusedElement,
    feedback: null,
    normalizeError: (error: unknown) => (error as Error).message,
    render: () => {},
  };
  const createOperation = new Function(...Object.keys(environment), `${operation}
    return { copyCredential, feedback: () => feedback };
  `);
  const host = createOperation(...Object.values(environment)) as {
    copyCredential(text: string, fallbackElementId: string, successMessage: string): Promise<void>;
    feedback(): { tone: string; message: string };
  };
  return { ...host, copied, temporaryInputs, focusRestored: () => focusRestored };
}

describe("credential clipboard operation", () => {
  test("copies the original ID through the Clipboard API", async () => {
    const host = clipboardHost(true);
    await host.copyCredential("123456789012", "device-id", "已复制");
    expect(host.copied).toEqual(["123456789012"]);
    expect(host.temporaryInputs).toHaveLength(0);
  });

  test("fallback copies the original value rather than its grouped display", async () => {
    const host = clipboardHost(false);
    await host.copyCredential("123456789012", "device-id", "已复制");
    expect(host.copied).toEqual(["123456789012"]);
    expect(host.temporaryInputs).toEqual([expect.objectContaining({ value: "", removed: true })]);
    expect(host.focusRestored()).toBe(true);
    expect(host.feedback()).toEqual({ tone: "success", message: "已复制" });
  });

  test("cleans the temporary secret and restores focus when Windows denies copying", async () => {
    const host = clipboardHost(false, false);
    await host.copyCredential("SECRET", "password", "已复制");
    expect(host.copied).toHaveLength(0);
    expect(host.temporaryInputs).toEqual([expect.objectContaining({ value: "", removed: true })]);
    expect(host.focusRestored()).toBe(true);
    expect(host.feedback().tone).toBe("error");
  });

  test("does not copy an unavailable credential after its source was removed", async () => {
    const host = clipboardHost(false, true, false);
    await host.copyCredential("SECRET", "removed-password", "已复制");
    expect(host.copied).toHaveLength(0);
    expect(host.temporaryInputs).toHaveLength(0);
    expect(host.feedback()).toEqual({ tone: "error", message: "此信息已不可用。" });
  });
});
