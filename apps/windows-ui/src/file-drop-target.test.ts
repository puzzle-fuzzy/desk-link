import { describe, expect, test } from "bun:test";
import { FileDropTargetGate, nativeFileDropHitsSurface } from "./file-drop-target";

function fixture({ blocked = false, occluded = false, hidden = false, connected = true } = {}) {
  const content = { closest: () => blocked ? {} : null };
  const other = { closest: () => null };
  const queries: Array<[number, number]> = [];
  const area = {
    isConnected: connected,
    getClientRects: () => hidden ? [] : [{}],
    contains: (element: unknown) => element === content,
    getBoundingClientRect: () => ({ left: 100, top: 50, right: 500, bottom: 350 }),
  };
  const document = {
    hidden: false,
    elementFromPoint(x: number, y: number) {
      queries.push([x, y]);
      return occluded ? other : content;
    },
    querySelectorAll: () => [area],
  } as unknown as Document;
  return { document, queries };
}

describe("原生文件拖放可见区域", () => {
  test.each([1, 1.5, 2])("%s 倍 DPI 使用 CSS 命中坐标", (pixelRatio) => {
    const { document, queries } = fixture();
    expect(nativeFileDropHitsSurface({ x: 150 * pixelRatio, y: 100 * pixelRatio }, pixelRatio, document)).toBe(true);
    expect(queries).toEqual([[150, 100]]);
    expect(nativeFileDropHitsSurface({ x: 90 * pixelRatio, y: 100 * pixelRatio }, pixelRatio, document)).toBe(false);
  });

  test("菜单、表单控件、其他页面覆盖和隐藏远控表面拒绝 drop", () => {
    for (const flags of [{ blocked: true }, { occluded: true }, { hidden: true }, { connected: false }]) {
      expect(nativeFileDropHitsSurface({ x: 150, y: 100 }, 1, fixture(flags).document)).toBe(false);
    }
  });

  test("窗口外、右下边界和无效 DPI/坐标均拒绝", () => {
    const { document } = fixture();
    for (const position of [{ x: -1, y: 100 }, { x: 500, y: 100 }, { x: 150, y: 350 }, { x: Number.NaN, y: 100 }]) {
      expect(nativeFileDropHitsSurface(position, 1, document)).toBe(false);
    }
    for (const ratio of [0, -1, Number.NaN, Number.POSITIVE_INFINITY]) {
      expect(nativeFileDropHitsSurface({ x: 150, y: 100 }, ratio, document)).toBe(false);
    }
  });

  test("文档后台时拒绝", () => {
    const { document } = fixture();
    Object.defineProperty(document, "hidden", { value: true });
    expect(nativeFileDropHitsSurface({ x: 150, y: 100 }, 1, document)).toBe(false);
  });
});

describe("拖放会话所有权", () => {
  const target = { streamId: "7", deviceId: "123 456 789", generation: 3 };

  test("提示与提交共用命中规则；移出接收区域不清除原目标", () => {
    const gate = new FileDropTargetGate();
    gate.enter(target);
    expect(gate.accept(target, false)).toBeNull();
    expect(gate.accept(target, true)).toEqual(target);
    expect(gate.accept(target, false)).toBeNull();
    expect(gate.accept(target, true)).toEqual(target);
    gate.clear();
    expect(gate.accept(target, true)).toBeNull();
  });

  test("stream、设备或连接代数变化后，本次拖动不能重新绑定新会话", () => {
    for (const replacement of [
      { ...target, streamId: "8" },
      { ...target, deviceId: "987 654 321" },
      { ...target, generation: 4 },
      null,
    ]) {
      const gate = new FileDropTargetGate();
      gate.enter(target);
      expect(gate.accept(replacement, true)).toBeNull();
      expect(gate.accept(target, true)).toBeNull();
    }
  });
});
