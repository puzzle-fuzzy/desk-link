import { describe, expect, test } from "bun:test";
import { RemoteInputPresentation, sameInputContext, validInputContext } from "./remote-input-context";

const initial = { streamId: "41", inputEpoch: "7", configVersion: 3 };

describe("远程输入只绑定已经呈现的配置", () => {
  test("配置公告和解码输出不能启用输入，绘制确认后才开放当前context", () => {
    const gate = new RemoteInputPresentation();
    expect(gate.context).toBeNull();
    expect(gate.announce(initial, 2)).toEqual({ changed: true, contextChanged: false });
    expect(gate.context).toBeNull();
    const frameToken = {};
    const painted = gate.drew(initial, 2, frameToken)!;
    expect(painted.frameToken).toBe(frameToken);
    expect(gate.context).toBeNull();
    // Production calls activate from its subsequent visible rAF barrier.
    expect(gate.activate(painted, 2, "41", true)).toBe(true);
    expect(gate.context).toEqual({ streamId: "41", inputEpoch: "7" });
    expect(Object.isFrozen(gate.context)).toBe(true);
  });

  test("后续画面取代先前frameToken时，迟到屏障不能激活旧帧", () => {
    const gate = new RemoteInputPresentation();
    gate.announce(initial, 2);
    const firstToken = {};
    const secondToken = {};
    const first = gate.drew(initial, 2, firstToken)!;
    const second = gate.drew(initial, 2, secondToken)!;
    expect(first.frameToken).toBe(firstToken);
    expect(second.frameToken).toBe(secondToken);
    expect(gate.activate(first, 2, "41", true)).toBe(false);
    expect(gate.context).toBeNull();
    expect(gate.activate(second, 2, "41", true)).toBe(true);
  });

  test("相同epoch的画质配置也先暂停，但不要求清除已按下按键", () => {
    const gate = new RemoteInputPresentation();
    gate.announce(initial, 2);
    const first = gate.drew(initial, 2, {})!;
    gate.activate(first, 2, "41", true);
    const before = gate.context;
    const quality = { ...initial, configVersion: 4 };
    expect(gate.announce(quality, 2)).toEqual({ changed: true, contextChanged: false });
    expect(gate.context).toBeNull();
    expect(gate.activate(first, 2, "41", true)).toBe(false);
    expect(gate.drew(initial, 2, {})).toBeNull();
    const updated = gate.drew(quality, 2, {})!;
    expect(gate.activate(updated, 2, "41", true)).toBe(true);
    expect(sameInputContext(before, gate.context)).toBe(true);
  });

  test("重复配置公告保持已呈现context和待确认帧的身份", () => {
    const gate = new RemoteInputPresentation();
    gate.announce(initial, 2);
    const frame = gate.drew(initial, 2, {})!;
    gate.activate(frame, 2, "41", true);
    const current = gate.context;
    expect(gate.announce({ ...initial }, 2)).toEqual({ changed: false, contextChanged: false });
    expect(gate.context).toBe(current);
    expect(gate.painted).toBe(frame);
  });

  test("稳定呈现只更新最新frameToken，暂停时才为最新画面创建新候选", () => {
    const gate = new RemoteInputPresentation();
    gate.announce(initial, 2);
    const first = gate.drew(initial, 2, {})!;
    gate.activate(first, 2, "41", true);
    const context = gate.context;
    const latestToken = {};
    expect(gate.drew(initial, 2, latestToken)).toBeNull();
    expect(gate.context).toBe(context);
    expect(gate.painted).toBe(first);
    gate.suspend();
    const suspended = gate.painted!;
    expect(suspended).not.toBe(first);
    expect(suspended.frameToken).toBe(latestToken);
    expect(first.frameToken).not.toBe(latestToken);
    expect(gate.activate(first, 2, "41", true)).toBe(false);
    expect(gate.context).toBeNull();
    expect(gate.activate(suspended, 2, "41", true)).toBe(true);
    expect(gate.context).toBe(context);
  });

  test.each(["9007199254740991", "9007199254740992", "9007199254740993", "18446744073709551615"])(
    "stream和epoch以完整u64身份%s呈现并保留", (identity) => {
      const gate = new RemoteInputPresentation();
      const configuration = { ...initial, streamId: identity, inputEpoch: identity };
      expect(validInputContext(configuration)).toBe(true);
      gate.announce(configuration, 2);
      const frame = gate.drew(configuration, 2, {})!;
      expect(gate.activate(frame, 2, identity, true)).toBe(true);
      expect(gate.context).toEqual({ streamId: identity, inputEpoch: identity });
    },
  );

  test("超过2^53的相邻epoch和stream仍被识别为不同context", () => {
    const gate = new RemoteInputPresentation();
    const before = { ...initial, streamId: "9007199254740992", inputEpoch: "9007199254740992" };
    const after = { ...before, inputEpoch: "9007199254740993" };
    gate.announce(before, 2);
    const old = gate.drew(before, 2, {})!;
    gate.activate(old, 2, before.streamId, true);
    expect(gate.announce(after, 2)).toEqual({ changed: true, contextChanged: true });
    expect(gate.activate(old, 2, before.streamId, true)).toBe(false);
    const current = gate.drew(after, 2, {})!;
    expect(gate.activate(current, 2, "9007199254740993", true)).toBe(false);
    expect(gate.activate(current, 2, before.streamId, true)).toBe(true);
    expect(gate.context).toEqual({ streamId: before.streamId, inputEpoch: after.inputEpoch });
    expect(sameInputContext(before, after)).toBe(false);
  });

  test.each([
    { ...initial, streamId: "42" },
    { ...initial, inputEpoch: "8" },
    { ...initial, configVersion: 4 },
  ])("实际绘制配置不匹配时拒绝候选 %#", (different) => {
    const gate = new RemoteInputPresentation();
    gate.announce(initial, 2);
    expect(gate.drew(different, 2, {})).toBeNull();
    expect(gate.context).toBeNull();
  });

  test("新拓扑epoch使旧解码器和旧屏障都失效", () => {
    const gate = new RemoteInputPresentation();
    gate.announce(initial, 2);
    const old = gate.drew(initial, 2, {})!;
    gate.activate(old, 2, "41", true);
    const topology = { ...initial, inputEpoch: "8", configVersion: 4 };
    expect(gate.announce(topology, 2)).toEqual({ changed: true, contextChanged: true });
    expect(gate.context).toBeNull();
    expect(gate.drew(initial, 2, {})).toBeNull();
    expect(gate.activate(old, 2, "41", true)).toBe(false);
    const current = gate.drew(topology, 2, {})!;
    expect(gate.activate(current, 2, "41", true)).toBe(true);
    expect(gate.context).toEqual({ streamId: "41", inputEpoch: "8" });
  });

  test("相同stream和epoch的连接代数替换仍拒绝旧会话帧", () => {
    const gate = new RemoteInputPresentation();
    gate.announce(initial, 2);
    const old = gate.drew(initial, 2, {})!;
    expect(gate.announce(initial, 3)).toEqual({ changed: true, contextChanged: true });
    expect(gate.drew(initial, 2, {})).toBeNull();
    expect(gate.activate(old, 3, "41", true)).toBe(false);
    const current = gate.drew(initial, 3, {})!;
    expect(gate.activate(current, 2, "41", true)).toBe(false);
    expect(gate.context).toBeNull();
    expect(gate.activate(current, 3, "41", true)).toBe(true);
  });

  test("屏障时不可见、断线或runtime stream变化均不能启用输入", () => {
    const gate = new RemoteInputPresentation();
    gate.announce(initial, 2);
    const frame = gate.drew(initial, 2, {})!;
    expect(gate.activate(frame, 2, "41", false)).toBe(false);
    expect(gate.activate(frame, 2, null, true)).toBe(false);
    expect(gate.activate(frame, 2, "42", true)).toBe(false);
    expect(gate.context).toBeNull();
    expect(gate.activate(frame, 2, "41", true)).toBe(true);
  });

  test("隐藏暂停后，只有仍对应当前配置的已绘帧可重新确认", () => {
    const gate = new RemoteInputPresentation();
    gate.announce(initial, 2);
    const frame = gate.drew(initial, 2, {})!;
    gate.activate(frame, 2, "41", true);
    gate.suspend();
    expect(gate.context).toBeNull();
    expect(gate.painted).toBe(frame);
    expect(gate.activate(frame, 2, "41", true)).toBe(true);
    gate.suspend();
    gate.announce({ ...initial, inputEpoch: "8", configVersion: 4 }, 2);
    expect(gate.activate(frame, 2, "41", true)).toBe(false);
    expect(gate.context).toBeNull();
  });

  test("取消清空后旧绘制确认不能复活输入", () => {
    const gate = new RemoteInputPresentation();
    gate.announce(initial, 2);
    const frame = gate.drew(initial, 2, {})!;
    gate.activate(frame, 2, "41", true);
    gate.clear();
    expect(gate.context).toBeNull();
    expect(gate.painted).toBeNull();
    expect(gate.activate(frame, 2, "41", true)).toBe(false);
    expect(gate.drew(initial, 2, {})).toBeNull();
  });

  test.each([0, 1, -1, Number.NaN, Number.POSITIVE_INFINITY, Number.MAX_SAFE_INTEGER + 1, undefined,
    "0", "-1", "1.1", "01", "+1", " 1", "1\n", "18446744073709551616"])(
    "缺失或无效epoch %s不能由当前会话或默认值补齐", (epoch) => {
      const gate = new RemoteInputPresentation();
      const malformed = { ...initial, inputEpoch: epoch as unknown as string };
      gate.announce(malformed, 2);
      expect(validInputContext(malformed)).toBe(false);
      expect(gate.drew(malformed, 2, {})).toBeNull();
      expect(gate.context).toBeNull();
    },
  );

  test("缺失或零stream同样关闭输入", () => {
    for (const streamId of [0, 1, undefined, "0", "01", "+1", "1.1", "18446744073709551616"]) {
      const malformed = { ...initial, streamId: streamId as unknown as string };
      const gate = new RemoteInputPresentation();
      gate.announce(malformed, 2);
      expect(gate.drew(malformed, 2, {})).toBeNull();
      expect(gate.context).toBeNull();
    }
  });

  test("调用者修改配置对象不能把已排队候选重标为新context", () => {
    const gate = new RemoteInputPresentation();
    const configuration = { ...initial };
    gate.announce(configuration, 2);
    const frame = gate.drew(configuration, 2, {})!;
    configuration.inputEpoch = "99";
    configuration.streamId = "99";
    expect(frame.configuration).toEqual(initial);
    expect(gate.activate(frame, 2, "41", true)).toBe(true);
    expect(gate.context).toEqual({ streamId: "41", inputEpoch: "7" });
  });

  test("未经过drew的伪候选即使字段一致也不能激活", () => {
    const gate = new RemoteInputPresentation();
    gate.announce(initial, 2);
    expect(gate.activate({ configuration: initial, channelGeneration: 2, frameToken: {} }, 2, "41", true)).toBe(false);
    expect(gate.context).toBeNull();
  });
});
