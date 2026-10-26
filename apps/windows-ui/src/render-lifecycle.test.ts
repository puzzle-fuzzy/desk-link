import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { retainSessionSurface } from "./session-surface";

// Exercise the actual render function with a small host instead of adding a
// browser/DOM dependency to this lifecycle-order regression test.
const mainSource = readFileSync(resolve(import.meta.dir, "main.ts"), "utf8");
const renderStart = mainSource.indexOf("function render(): void {");
const renderEnd = mainSource.indexOf("\nfunction scheduleRender()", renderStart);
if (renderStart < 0 || renderEnd < 0) throw new Error("Unable to locate application renderer");
const renderSource = new Bun.Transpiler({ loader: "ts" }).transformSync(mainSource.slice(renderStart, renderEnd));

function rendererHost(accountLoading: boolean, connected = false) {
  const events: string[] = [];
  const environment = {
    accountLoading,
    account: { signedIn: false },
    accountLocalMode: connected,
    prepareControllerRender: () => events.push("release-controller-surface"),
    app: {
      querySelector: (selector: string) => selector === "[data-remote-viewport]" ? (connected ? {} : null) : {
        set outerHTML(_markup: string) { events.push(`patch:${selector}`); },
        set innerHTML(_markup: string) { events.push(`patch:${selector}`); },
      },
      set innerHTML(_markup: string) { events.push("replace-dom"); },
    },
    renderAccountLoading: () => "loading",
    renderAccountGate: () => "account",
    bindAccountInteractions: () => events.push("bind-account"),
    document: { querySelector: () => null },
    renderedView: connected ? "controller" : null,
    renderedControllerSurface: connected ? "1:1" : null,
    controllerSurfaceIdentity: () => connected ? "1:1" : null,
    retainSessionSurface,
    updatePersistentControllerRegions: () => events.push("update-controller-regions"),
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
      ,changeConfig() { controllerSurfaceIdentity = () => "1:2"; }
      ,leaveSession() { activeView = "settings"; }
      ,showAccount() { accountLocalMode = false; }
    };
  `);
  const renderer = createRenderer(...Object.values(environment)) as {
    render(): void;
    returnToWorkspace(): void;
    changeConfig(): void;
    leaveSession(): void;
    showAccount(): void;
  };
  return { events, ...renderer };
}

describe("application render lifecycle", () => {
  test("Host and menu shell refreshes preserve the focused canvas and decoder without rebinding input", () => {
    const host = rendererHost(false, true);
    host.render();
    host.render();
    expect(host.events.filter((event) => event === "release-controller-surface" || event === "replace-dom")).toEqual([]);
    expect(host.events.filter((event) => event === "update-controller-regions")).toHaveLength(2);
  });

  test("configuration generations and actually leaving the session still release resources", () => {
    for (const action of ["changeConfig", "leaveSession", "showAccount"] as const) {
      const host = rendererHost(false, true);
      host[action]();
      host.render();
      expect(host.events[0]).toBe("release-controller-surface");
      expect(host.events[1]).toBe("replace-dom");
    }
  });
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
