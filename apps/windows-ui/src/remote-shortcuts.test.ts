import { describe, expect, test } from "bun:test";
import { RemoteInputDispatcher } from "./remote-input-dispatcher";
import { REMOTE_SHORTCUTS, remoteShortcutInput } from "./remote-shortcuts";
import type { ControllerInput } from "./types";

function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>((next) => { resolve = next; });
  return { promise, resolve };
}

describe("远端快捷键菜单动作", () => {
  test("可发现Esc和系统组合键，不提供无法模拟的安全注意序列", () => {
    expect(REMOTE_SHORTCUTS.map((shortcut) => shortcut.id)).toEqual([
      "escape", "windows", "alt-tab", "alt-f4", "control-escape", "task-manager", "show-desktop", "lock",
    ]);
    expect(remoteShortcutInput("control-alt-delete")).toBeNull();
    expect(remoteShortcutInput("<script>")).toBeNull();
  });

  test.each(REMOTE_SHORTCUTS.map((shortcut) => [shortcut.id] as const))("%s完整发送同stream的down/up", async (id) => {
    const sent: Array<[ControllerInput, string]> = [];
    const dispatcher = new RemoteInputDispatcher(async (input, { streamId }) => { sent.push([input, streamId]); });
    const down = remoteShortcutInput(id)!;
    await dispatcher.enqueueShortcut(down, { streamId: "9", inputEpoch: "1" });
    expect(sent).toEqual([[down, "9"], [{ ...down, pressed: false }, "9"]]);
  });

  test("组合完整发送前其他鼠标和文字不得插入", async () => {
    const downAck = deferred();
    const sent: string[] = [];
    const dispatcher = new RemoteInputDispatcher(async (input) => {
      sent.push(`${input.key ?? input.kind}:${input.pressed}`);
      if (sent.length === 1) await downAck.promise;
    }, undefined, async (text) => { sent.push(text); });
    const chord = dispatcher.enqueueShortcut(remoteShortcutInput("alt-tab")!, { streamId: "9", inputEpoch: "1" });
    dispatcher.enqueue({ kind: "mouseButton", button: "left", pressed: true }, { streamId: "9", inputEpoch: "1" });
    const text = dispatcher.enqueueText("中文", { streamId: "9", inputEpoch: "1" });
    downAck.resolve();
    await chord;
    await text;
    await dispatcher.drain();
    expect(sent).toEqual(["tab:true", "tab:false", "mouseButton:true", "中文"]);
  });

  test("失焦取消尚未开始的组合，不影响正常释放输入", async () => {
    const blocked = deferred();
    const sent: ControllerInput[] = [];
    const dispatcher = new RemoteInputDispatcher(async (input) => {
      sent.push(input);
      if (sent.length === 1) await blocked.promise;
    });
    dispatcher.enqueue({ kind: "mouseMove", x: 1, y: 1 }, { streamId: "9", inputEpoch: "1" });
    const chord = dispatcher.enqueueShortcut(remoteShortcutInput("task-manager")!, { streamId: "9", inputEpoch: "1" });
    const rejected = chord.catch((error: Error) => error);
    dispatcher.cancelShortcuts();
    dispatcher.enqueue({ kind: "key", key: "shift", pressed: false, modifiers: 0 }, { streamId: "9", inputEpoch: "1" });
    blocked.resolve();
    expect(await rejected).toBeInstanceOf(Error);
    await dispatcher.drain();
    expect(sent.map((input) => input.key ?? input.kind)).toEqual(["mouseMove", "shift"]);
  });

  test.each(["blur", "stream"])("%s取消正在发送的组合仍向旧stream清理", async (reason) => {
    const blocked = deferred();
    const sent: Array<[ControllerInput, string]> = [];
    const dispatcher = new RemoteInputDispatcher(async (input, { streamId }) => {
      sent.push([input, streamId]);
      if (sent.length === 1) await blocked.promise;
    });
    const down = remoteShortcutInput("alt-f4")!;
    const chord = dispatcher.enqueueShortcut(down, { streamId: "9", inputEpoch: "1" });
    const rejected = chord.catch((error: Error) => error);
    if (reason === "stream") dispatcher.discardAll();
    else dispatcher.cancelShortcuts();
    dispatcher.enqueue({ kind: "key", key: "enter", pressed: true }, { streamId: "10", inputEpoch: "1" });
    blocked.resolve();
    expect(await rejected).toBeInstanceOf(Error);
    await dispatcher.drain();
    expect(sent).toEqual([[down, "9"], [{ ...down, pressed: false }, "9"], [{ kind: "key", key: "enter", pressed: true }, "10"]]);
  });

  test("按下IPC失败也尝试清理修饰键，后续组合可继续", async () => {
    const sent: ControllerInput[] = [];
    const dispatcher = new RemoteInputDispatcher(async (input) => {
      sent.push(input);
      if (sent.length === 1) throw new Error("uncertain IPC");
    });
    await expect(dispatcher.enqueueShortcut(remoteShortcutInput("task-manager")!, { streamId: "9", inputEpoch: "1" })).rejects.toThrow("uncertain IPC");
    await dispatcher.enqueueShortcut(remoteShortcutInput("escape")!, { streamId: "9", inputEpoch: "1" });
    expect(sent.map((input) => [input.key, input.pressed, input.modifiers])).toEqual([
      ["escape", true, 3], ["escape", false, 3], ["escape", true, 0], ["escape", false, 0],
    ]);
  });
});
