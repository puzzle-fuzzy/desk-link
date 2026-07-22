import { describe, expect, test } from "bun:test";

import {
  PRESENTATION_DROP_DECODE_QUEUE_HINT,
  PRESENTATION_DROP_SEVERE_DECODE_QUEUE_HINT,
  PRESENTATION_DROP_PRESSURE_THRESHOLD,
  PRESENTATION_DROP_SEVERE_THRESHOLD,
  presentationDropDecodeQueueHint,
} from "./video-render-pressure";

describe("显示合并帧到自动画质压力的转换", () => {
  test("ignores occasional compositor drops", () => {
    expect(presentationDropDecodeQueueHint(PRESENTATION_DROP_PRESSURE_THRESHOLD - 1)).toBe(0);
  });

  test("uses a queue overload hint for sustained drops", () => {
    expect(presentationDropDecodeQueueHint(PRESENTATION_DROP_PRESSURE_THRESHOLD)).toBe(
      PRESENTATION_DROP_DECODE_QUEUE_HINT,
    );
  });

  test("uses severe pressure only for a large one-second sample", () => {
    expect(presentationDropDecodeQueueHint(PRESENTATION_DROP_SEVERE_THRESHOLD)).toBe(
      PRESENTATION_DROP_SEVERE_DECODE_QUEUE_HINT,
    );
  });

  test("bounds invalid input to no pressure", () => {
    expect(presentationDropDecodeQueueHint(Number.NaN)).toBe(0);
    expect(presentationDropDecodeQueueHint(-10)).toBe(0);
  });
});
