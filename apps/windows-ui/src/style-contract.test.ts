import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const root = resolve(import.meta.dir, "styles");
const layout = readFileSync(resolve(root, "layout.css"), "utf8").replace(/\r\n/g, "\n");
const base = readFileSync(resolve(root, "base.css"), "utf8");
const interaction = readFileSync(resolve(root, "interaction.css"), "utf8");
const remote = readFileSync(resolve(root, "remote.css"), "utf8");

describe("Windows interface layers", () => {
  test("keeps visual themes out of the layout layer", () => {
    expect(layout).not.toMatch(/(?:^|[;{\n])\s*(?:--[\w-]+|color|background[\w-]*|font[\w-]*|border[\w-]*|box-shadow|animation[\w-]*|transition[\w-]*)\s*:/);
    expect(layout).toContain('@import "./remote.css"');
    expect(base).toContain("--accent:");
    expect(base).toContain("@media (prefers-color-scheme: dark)");
  });

  test("keeps hidden panels, focus, native controls and high contrast usable", () => {
    expect(base).toMatch(/\[hidden\]\s*\{\s*display:\s*none\s*!important/);
    expect(base).toContain(":focus-visible");
    expect(base).toContain("outline: 2px solid Highlight");
    expect(base).toContain(":disabled");
    expect(base).not.toMatch(/appearance:\s*none/);
    expect(interaction).toContain("@media (forced-colors: active)");
    expect(interaction).toContain("@media (prefers-reduced-motion: reduce)");
  });

  test("preserves responsive geometry and remote input boundaries", () => {
    expect(layout).toContain("@media (width <= 640px)");
    expect(/data-remote-fullscreen=["']?true/.test(remote)).toBe(true);
    expect(/data-remote-toolbar-visible=["']?false/.test(remote)).toBe(true);
    expect(remote).toContain("pointer-events: none");
    expect(remote).toContain("touch-action: none");
    expect(remote).toContain("object-fit: contain");
    expect(layout).toContain(".controller-connect-layout {\n  flex: none;");
    const updateLayer = Number(/html\[data-remote-fullscreen=true\] \.windows-update-banner\s*\{[^}]*z-index:\s*(\d+)/.exec(layout)?.[1]);
    const remoteLayer = Number(/html\[data-remote-fullscreen=true\] \.remote-session\s*\{[^}]*z-index:\s*(\d+)/.exec(remote)?.[1]);
    const approvalLayer = Number(/\.approval-backdrop\s*\{[^}]*z-index:\s*(\d+)/.exec(layout)?.[1]);
    expect(updateLayer).toBeGreaterThan(remoteLayer);
    expect(updateLayer).toBeLessThan(approvalLayer);
  });

  test("keeps identity text and approval content inside their containers", () => {
    expect(layout).toContain(".approval-identity dd");
    expect(layout).toContain("overflow-wrap: anywhere");
    expect(layout).toContain(".saved-device-row");
    expect(layout).toContain("text-overflow: ellipsis");
    expect(layout).toContain(".approval-dialog");
    expect(layout).toContain("overflow: auto");
  });
});
