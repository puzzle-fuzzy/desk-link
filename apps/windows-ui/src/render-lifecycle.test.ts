import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

// Exercise the actual render function with a small host instead of adding a
// browser/DOM dependency to this lifecycle-order regression test.
const mainSource = readFileSync(resolve(import.meta.dir, "main.ts"), "utf8");
const renderStart = mainSource.indexOf("function render(): void {");
const renderEnd = mainSource.indexOf("\nfunction scheduleRender()", renderStart);
if (renderStart < 0 || renderEnd < 0) throw new Error("Unable to locate application renderer");
const renderSource = new Bun.Transpiler({ loader: "ts" }).transformSync(mainSource.slice(renderStart, renderEnd));

function rendererHost(accountLoading: boolean) {
  const events: string[] = [];
  const environment = {
    accountLoading,
    account: { signedIn: false },
    accountLocalMode: false,
    prepareControllerRender: () => events.push("release-controller-surface"),
    app: { set innerHTML(_markup: string) { events.push("replace-dom"); } },
    renderAccountLoading: () => "loading",
    renderAccountGate: () => "account",
    bindAccountInteractions: () => events.push("bind-account"),
    document: { querySelector: () => null },
    renderedView: null,
    activeView: "controller",
    loading: false,
    feedback: null,
    renderSidebar: () => "",
    renderWorkspaceTopbar: () => "",
    renderLoading: () => "",
    renderCurrentView: () => "workspace",
    renderHostApproval: () => "",
    renderLucideIcons: () => {},
    bindInteractions: () => events.push("bind-workspace"),
    bindUtilityMenuDismiss: () => {},
    focusNewApproval: () => {},
    updateWindowsUpdateRegions: () => {},
  };
  const createRenderer = new Function(...Object.keys(environment), `${renderSource}
    return {
      render,
      returnToWorkspace() { accountLoading = false; accountLocalMode = true; }
    };
  `);
  const renderer = createRenderer(...Object.values(environment)) as {
    render(): void;
    returnToWorkspace(): void;
  };
  return { events, ...renderer };
}

describe("application render lifecycle", () => {
  test("releases remote input and presentation before the account-loading gate removes its DOM", () => {
    const host = rendererHost(true);
    host.render();
    expect(host.events).toEqual(["release-controller-surface", "replace-dom"]);
  });

  test("releases the remote surface before showing login and rebinds the workspace on return", () => {
    const host = rendererHost(false);
    host.render();
    expect(host.events).toEqual(["release-controller-surface", "replace-dom", "bind-account"]);

    host.events.length = 0;
    host.returnToWorkspace();
    host.render();
    expect(host.events).toEqual(["release-controller-surface", "replace-dom", "bind-workspace"]);
  });
});
