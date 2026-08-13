export type ConnectionQualityTone = "measuring" | "good" | "attention" | "poor";

export interface ConnectionQualityInput {
  decodedFrames: number;
  displayedFpsX100: number | null;
  maxFrameGapMs: number | null;
  receivedVideoPackets: number;
  droppedVideoPackets: number;
}

export interface ConnectionQualitySummary {
  tone: ConnectionQualityTone;
  label: string;
  detail: string;
}

const MIN_MEASURED_FRAMES = 8;

export function connectionQualitySummary(input: ConnectionQualityInput): ConnectionQualitySummary {
  const fps = input.displayedFpsX100 === null ? null : input.displayedFpsX100 / 100;
  const totalPackets = Math.max(0, input.receivedVideoPackets) + Math.max(0, input.droppedVideoPackets);
  const lossRatio = totalPackets > 0
    ? Math.max(0, input.droppedVideoPackets) / totalPackets
    : null;

  if (
    input.decodedFrames < MIN_MEASURED_FRAMES
    || fps === null
    || !Number.isFinite(fps)
  ) {
    return {
      tone: "measuring",
      label: "正在测量",
      detail: "正在收集画面帧率和传输稳定性。",
    };
  }

  const stablePackets = lossRatio === null || lossRatio <= 0.02;
  if (fps >= 24 && (input.maxFrameGapMs === null || input.maxFrameGapMs <= 160) && stablePackets) {
    return {
      tone: "good",
      label: "画面流畅",
      detail: "当前画面保持稳定，可以继续使用。",
    };
  }

  const usablePackets = lossRatio === null || lossRatio <= 0.08;
  if (fps >= 15 && (input.maxFrameGapMs === null || input.maxFrameGapMs <= 350) && usablePackets) {
    return {
      tone: "attention",
      label: "基本稳定",
      detail: "检测到偶发卡顿，DeskLink 正在保持连接。",
    };
  }

  return {
    tone: "poor",
    label: "需要关注",
    detail: "画面出现明显卡顿，可以在工具栏切换为“流畅”画质。",
  };
}
