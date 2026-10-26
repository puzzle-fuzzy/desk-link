import { describe, expect, test } from "bun:test";
import { FileQueueView, type QueuedFileView } from "./file-queue-view";

const files: QueuedFileView[] = Array.from({ length: 20 }, (_, index) => ({
  id: `file-${index + 1}`,
  name: index === 16 ? "同名文件.txt" : `文件 ${index + 1}.txt`,
  size: 1_024,
}));
const options = {
  disabled: false,
  removingId: null,
  formatBytes: (size: number) => `${size} B`,
  icon: (name: "file-up" | "loader-circle" | "x") => `<i data-icon="${name}"></i>`,
};

async function removalControls(html: string): Promise<Array<{ id: string; label: string | null; disabled: boolean }>> {
  const controls: Array<{ id: string; label: string | null; disabled: boolean }> = [];
  await new HTMLRewriter().on("button[data-controller-file-queue-remove]", {
    element(element) {
      controls.push({
        id: element.getAttribute("data-controller-file-queue-remove")!,
        label: element.getAttribute("aria-label"),
        disabled: element.hasAttribute("disabled"),
      });
    },
  }).transform(new Response(html)).text();
  return controls;
}

function rootFixture(ids: string[], focusedId: string | "summary" | null, disabled = false) {
  const body = {};
  const focusCalls: string[] = [];
  const summary = { focus: () => focusCalls.push("summary") };
  const details = {
    open: true,
    querySelector: () => summary,
    addEventListener: () => {},
  };
  const list = { scrollTop: 240 };
  const buttons = ids.map((id) => ({
    dataset: { controllerFileQueueRemove: id },
    disabled,
    focus: () => focusCalls.push(id),
  }));
  const ownerDocument = {
    body,
    activeElement: focusedId === "summary" ? summary : buttons.find((button) => button.dataset.controllerFileQueueRemove === focusedId) ?? body,
  };
  const root = {
    ownerDocument,
    querySelector: (selector: string) => selector.includes("-details]") ? details : list,
    querySelectorAll: () => buttons,
  } as unknown as HTMLElement;
  return { root, details, list, buttons, ownerDocument, focusCalls };
}

describe("完整等待文件队列", () => {
  test("20 个文件的真实渲染保留第 17 项可访问移除入口", async () => {
    const view = new FileQueueView();
    const html = view.render(files, options);
    const controls = await removalControls(html);
    expect(controls.map((control) => control.id)).toEqual(files.map((file) => file.id));
    expect(controls[16]).toEqual({
      id: "file-17",
      label: "从队列移除 第 17 项 同名文件.txt",
      disabled: false,
    });
    expect(html).toContain("<summary>查看全部 20 个等待文件</summary>");
    expect(html).not.toContain(" tabindex=\"-1\"");
  });

  test("重名文件按独立 ID 操作，名称和属性均正确转义", async () => {
    const sameName = '<img src=x onerror="bad">.txt';
    const html = new FileQueueView().render([
      { id: 'first"id', name: sameName, size: 1 },
      { id: "second-id", name: sameName, size: 1 },
    ], options);
    // HTMLRewriter exposes the serialized attribute value, before browser entity decoding.
    const escapedName = "&lt;img src=x onerror=&quot;bad&quot;&gt;.txt";
    expect(await removalControls(html)).toEqual([
      { id: "first&quot;id", label: `从队列移除 第 1 项 ${escapedName}`, disabled: false },
      { id: "second-id", label: `从队列移除 第 2 项 ${escapedName}`, disabled: false },
    ]);
    expect(html).not.toContain("<img");
  });

  test("忙碌操作禁用全部等待项，只标记对应移除项", async () => {
    const html = new FileQueueView().render(files, { ...options, disabled: true, removingId: "file-17" });
    const controls = await removalControls(html);
    expect(controls.every((control) => control.disabled)).toBe(true);
    expect(controls.filter((control) => control.label?.startsWith("正在从队列移除"))).toEqual([
      { id: "file-17", label: "正在从队列移除 第 17 项 同名文件.txt", disabled: true },
    ]);
  });

  test("移除第 17 项后保持展开与滚动，键盘焦点移到下一项", () => {
    const view = new FileQueueView();
    const original = rootFixture(files.map((file) => file.id), "file-17");
    view.capture(original.root);
    const remaining = files.filter((file) => file.id !== "file-17");
    expect(view.render(remaining, options)).toContain("data-controller-file-queue-details open");
    const updated = rootFixture(remaining.map((file) => file.id), null);
    updated.list.scrollTop = 0;
    view.restore(updated.root);
    expect(updated.details.open).toBe(true);
    expect(updated.list.scrollTop).toBe(240);
    expect(updated.focusCalls).toEqual(["file-18"]);
    expect(remaining).toEqual([...files.slice(0, 16), ...files.slice(17)]);
  });

  test("忙碌 repaint 使焦点落到 body 后，完成时仍恢复原键盘位置", () => {
    const view = new FileQueueView();
    view.capture(rootFixture(files.map((file) => file.id), "file-17").root);
    const busy = rootFixture(files.map((file) => file.id), null, true);
    view.restore(busy.root);
    expect(busy.focusCalls).toEqual([]);
    view.capture(busy.root);
    const complete = rootFixture(files.filter((file) => file.id !== "file-17").map((file) => file.id), null);
    view.restore(complete.root);
    expect(complete.focusCalls).toEqual(["file-18"]);
  });

  test("用户主动移开焦点时不抢回，删除最后一项时聚焦前一项", () => {
    const view = new FileQueueView();
    view.capture(rootFixture(files.map((file) => file.id), "file-20").root);
    const updated = rootFixture(files.slice(0, 19).map((file) => file.id), null);
    view.restore(updated.root);
    expect(updated.focusCalls).toEqual(["file-19"]);
    updated.ownerDocument.activeElement = {};
    view.capture(updated.root);
    const later = rootFixture(files.slice(0, 19).map((file) => file.id), null);
    view.restore(later.root);
    expect(later.focusCalls).toEqual([]);
  });

  test("小队列不需要额外展开步骤", () => {
    const html = new FileQueueView().render(files.slice(0, 4), options);
    expect(html).not.toContain("<details");
    expect(html).toContain('aria-label="全部等待发送的文件"');
  });
});
