import { describe, expect, test } from "bun:test";
import {
  FileTaskStore, MAX_FILE_TASK_VISIBLE_ITEMS, NO_FILE_CAPABILITIES,
  fileTaskActions, formatTaskBytes, isFileTaskSignal, isTransferByteCount,
  renderFileTask, taskAcknowledgedPercent, updateFileTaskRegion,
  type FileTaskItem, type FileTaskSignal,
} from "./file-transfer-task";

const taskId = "1234567890abcdef1234567890abcdef";
function task(changes: Partial<FileTaskSignal> = {}): FileTaskSignal {
  return {
    kind: "fileTask", taskId, revision: "1", direction: "upload", name: "测试目录",
    state: "sending", scanComplete: true, scannedItems: 2, completedItems: 0, failedItems: 0,
    totalBytes: "4294967297", sentBytes: "4294967296", acknowledgedBytes: "2147483648",
    activeItem: null, items: [], ...changes,
  };
}
function item(changes: Partial<FileTaskItem> = {}): FileTaskItem {
  return {
    id: "1", path: "子目录/中文文件.txt", kind: "file", state: "sending",
    totalBytes: "4294967297", sentBytes: "4294967296", acknowledgedBytes: "2147483648", ...changes,
  };
}
const options = { busy: false, error: "", connected: true };

async function inspect(html: string) {
  const controls: Array<{ action: string | null; itemId: string | null; disabled: boolean }> = [];
  const progress: Array<{ value: string | null; label: string | null }> = [];
  const rows: string[] = [];
  const content = await new HTMLRewriter()
    .on("button", { element(element) {
      controls.push({ action: element.getAttribute("data-controller-file-task-action"),
        itemId: element.getAttribute("data-controller-file-task-retry-item"), disabled: element.hasAttribute("disabled") });
    } })
    .on("progress", { element(element) {
      progress.push({ value: element.getAttribute("value"), label: element.getAttribute("aria-label") });
    } })
    .on("li[data-file-task-item]", { element(element) { rows.push(element.getAttribute("data-file-task-item")!); } })
    .transform(new Response(html)).text();
  return { controls, progress, rows, content };
}

describe("目录与大文件任务边界和展示", () => {
  test("4 GiB 与完整 u64 的字节数及进度都按整数计算", () => {
    expect(isFileTaskSignal(task())).toBe(true);
    expect(formatTaskBytes("4294967297")).toBe("4.0 GB");
    expect(taskAcknowledgedPercent(task())).toBe(49);
    const full = task({ totalBytes: "18446744073709551615", sentBytes: "18446744073709551614", acknowledgedBytes: "18446744073709551614" });
    expect(isFileTaskSignal(full)).toBe(true);
    expect(taskAcknowledgedPercent(full)).toBe(99);
    expect(formatTaskBytes(full.totalBytes)).toBe("16.0 EB");
    // Adjacent numbers above Number.MAX_SAFE_INTEGER remain distinct in state.
    const store = new FileTaskStore();
    expect(store.accept(task({ totalBytes: "9007199254740993", sentBytes: "9007199254740992" }))).toBe(true);
    expect(store.current?.totalBytes).toBe("9007199254740993");
    expect(store.current?.sentBytes).toBe("9007199254740992");
  });

  test.each([0, 4294967297, "-1", "01", "1.2", " 1", "18446744073709551616", "1e9", ""])("拒绝非规范 u64 数值 %#", (invalid) => {
    expect(isTransferByteCount(invalid)).toBe(false);
    expect(isFileTaskSignal({ ...task(), totalBytes: invalid })).toBe(false);
  });

  test("扫描中没有伪造百分比，已发送和接收端确认分别展示", async () => {
    const result = await inspect(renderFileTask(task({ state: "scanning", scanComplete: false }), options));
    expect(result.progress).toEqual([{ value: null, label: "接收端已确认的总进度" }]);
    expect(result.content).toContain("总量仍在统计");
    expect(result.content).toContain("已发送 4.0 GB · 接收端已确认 2.0 GB");
    expect(result.content).not.toContain("已校验并保存");
  });

  test("Rust serde 的可空 message 与 VecDeque 数组能进入真实任务状态", () => {
    const store = new FileTaskStore();
    const native = JSON.parse(JSON.stringify(task({ message: null, items: [item({ message: null })] })));
    expect(store.accept(native)).toBe(true);
    expect(store.current?.message).toBeNull();
    expect(store.current?.items[0]?.message).toBeNull();
  });

  test("字节确认 100% 仍保留最终校验等待，完成只由终态表示", async () => {
    const result = await inspect(renderFileTask(task({ state: "verifying", sentBytes: "4294967297", acknowledgedBytes: "4294967297" }), options));
    expect(result.progress[0]?.value).toBe("100");
    expect(result.content).toContain("正在核对完整内容并完成落盘");
    expect(result.content).not.toContain("已校验并保存");
    expect(fileTaskActions("verifying")).toEqual(["cancel"]);
    expect(taskAcknowledgedPercent(task({ state: "completed", totalBytes: "0", sentBytes: "0", acknowledgedBytes: "0" }))).toBe(100);
  });

  test("部分成功保持失败项身份与逐项和整体重试入口", async () => {
    const failed = item({ id: "2", state: "failed", message: "磁盘空间不足" });
    const result = await inspect(renderFileTask(task({ state: "partial", completedItems: 1, failedItems: 1,
      items: [item({ state: "completed" }), failed] }), options));
    expect(result.controls).toEqual([
      { action: "retry", itemId: null, disabled: false },
      { action: null, itemId: "2", disabled: false },
    ]);
    expect(result.content).toContain("部分完成");
    expect(result.content).toContain("磁盘空间不足");
    expect(result.rows).toEqual(["1", "2"]);
  });

  test("目录列表始终为有界窗口，过量或重复身份快照不进入状态", async () => {
    const recent = Array.from({ length: MAX_FILE_TASK_VISIBLE_ITEMS }, (_, i) => item({ id: String(i + 1) }));
    const store = new FileTaskStore();
    expect(store.accept(task({ scannedItems: 100_000, items: recent }))).toBe(true);
    const rendered = await inspect(renderFileTask(store.current, options));
    expect(rendered.rows).toHaveLength(64);
    expect(rendered.content).toContain("共扫描 100000 项");
    expect(store.accept(task({ revision: "2", items: [...recent, item({ id: "65" })] }))).toBe(false);
    expect(store.accept(task({ revision: "2", items: [item(), item()] }))).toBe(false);
    expect(store.current?.revision).toBe("1");
  });

  test("旧修订不能回退进度，快照仅复制有界窗口", () => {
    const store = new FileTaskStore();
    const first = task({ revision: "9007199254740993", items: [item()] });
    expect(store.accept(first)).toBe(true);
    first.items[0]!.path = "later mutation";
    expect(store.current?.items[0]?.path).toBe("子目录/中文文件.txt");
    expect(store.accept(task({ revision: "9007199254740992" }))).toBe(false);
    expect(store.accept(task({ revision: "9007199254740993" }))).toBe(false);
    expect(store.accept(task({ revision: "9007199254740994" }))).toBe(true);
  });

  test("新认证连接可恢复同一任务的新进程修订，随后仍拒绝旧修订", () => {
    const store = new FileTaskStore();
    expect(store.accept(task({ revision: "9007199254740993" }))).toBe(true);
    store.beginConnection();
    expect(store.accept(task({ revision: "1", state: "paused" }))).toBe(true);
    expect(store.accept(task({ revision: "0", state: "sending" }))).toBe(false);
    expect(store.current?.state).toBe("paused");
  });

  test("原生认证 peer 先绑定，切换电脑清除任务并拒绝旧电脑迟到进度", () => {
    const store = new FileTaskStore();
    const first = "ab".repeat(16);
    const next = "cd".repeat(16);
    expect(store.beginPeer(first)).toBe(true);
    expect(store.accept(task({ peerId: first }))).toBe(true);
    expect(store.beginPeer(first.toUpperCase())).toBe(true);
    expect(store.current?.taskId).toBe(taskId);
    expect(store.beginPeer(next)).toBe(true);
    expect(store.current).toBeNull();
    expect(store.accept(task({ peerId: first, revision: "2" }))).toBe(false);
    expect(store.accept(task({ revision: "2" }))).toBe(false);
    expect(store.accept(task({ peerId: next, revision: "1", state: "paused" }))).toBe(true);
    expect(store.beginPeer("../peer")).toBe(false);
    expect(store.peerId).toBe(next);
  });

  test("同设备替换验证密钥形成新的认证作用域并清除旧任务", () => {
    const store = new FileTaskStore();
    const deviceId = "ab".repeat(16);
    const first = deviceId + "cd".repeat(32);
    const rekeyed = deviceId + "ef".repeat(32);
    expect(store.beginPeer(first)).toBe(true);
    expect(store.accept(task({ peerId: first }))).toBe(true);
    expect(store.beginPeer(rekeyed)).toBe(true);
    expect(store.current).toBeNull();
    expect(store.accept(task({ peerId: first, revision: "2" }))).toBe(false);
    expect(store.accept(task({ peerId: rekeyed, revision: "1" }))).toBe(true);
  });

  test("未协商功能默认为关闭，断线或执行操作时不能重复提交动作", async () => {
    expect(NO_FILE_CAPABILITIES).toEqual({ kind: "fileCapabilities", largeFiles: false, directories: false, blockResume: false });
    for (const state of [{ ...options, connected: false }, { ...options, busy: true }]) {
      const result = await inspect(renderFileTask(task({ state: "paused" }), state));
      expect(result.controls.map((entry) => entry.action)).toEqual(["resume", "cancel"]);
      expect(result.controls.every((entry) => entry.disabled)).toBe(true);
    }
  });

  test("远端名称和错误均转义为文本", async () => {
    const result = await inspect(renderFileTask(task({ name: '<img src=x onerror="bad">',
      message: "<script>bad</script>", items: [item({ path: '<input autofocus onfocus="bad">', state: "failed" })] }), options));
    expect(result.content).not.toMatch(/<(img|script|input)( |\>)/);
    expect(result.content).toContain("&lt;script&gt;");
  });

  test("进度 repaint 只写独立任务区域并保留列表展开状态", () => {
    let html = "";
    let replaced = 0;
    const beforeList = { scrollTop: 120 };
    const afterList = { scrollTop: 0 };
    const before = { open: true, querySelector: () => beforeList };
    const after = { open: false, querySelector: () => afterList };
    const region = {
      hidden: true,
      ownerDocument: { activeElement: null },
      querySelector: () => replaced ? after : before,
      set innerHTML(value: string) { html = value; replaced += 1; },
    } as unknown as HTMLElement;
    updateFileTaskRegion(region, task(), options);
    expect(replaced).toBe(1);
    expect(region.hidden).toBe(false);
    expect(after.open).toBe(true);
    expect(afterList.scrollTop).toBe(120);
    expect(html).toContain("接收端已确认");
  });
});
