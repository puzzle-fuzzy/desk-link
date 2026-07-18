export type ActiveTransferState = "sending" | "receiving";

export interface TransferProgressSample {
  identity: string;
  state: ActiveTransferState;
  transferred: number;
  total: number;
}

export interface TransferMetrics {
  identity: string;
  lastTransferred: number;
  lastSampleAtMs: number;
  bytesPerSecond: number | null;
}

const MIN_SAMPLE_INTERVAL_MS = 200;
const SPEED_SMOOTHING = 0.3;

export function sampleTransferMetrics(
  previous: TransferMetrics | null,
  sample: TransferProgressSample,
  nowMs: number,
): TransferMetrics {
  const transferred = boundedBytes(sample.transferred, sample.total);
  if (
    !previous
    || previous.identity !== sample.identity
    || transferred < previous.lastTransferred
    || !Number.isFinite(nowMs)
    || nowMs < previous.lastSampleAtMs
  ) {
    return {
      identity: sample.identity,
      lastTransferred: transferred,
      lastSampleAtMs: Number.isFinite(nowMs) ? nowMs : 0,
      bytesPerSecond: null,
    };
  }

  const elapsedMs = nowMs - previous.lastSampleAtMs;
  if (elapsedMs < MIN_SAMPLE_INTERVAL_MS || transferred === previous.lastTransferred) {
    return previous;
  }

  const instantaneous = (transferred - previous.lastTransferred) / (elapsedMs / 1_000);
  const bytesPerSecond = previous.bytesPerSecond === null
    ? instantaneous
    : previous.bytesPerSecond * (1 - SPEED_SMOOTHING) + instantaneous * SPEED_SMOOTHING;
  return {
    identity: sample.identity,
    lastTransferred: transferred,
    lastSampleAtMs: nowMs,
    bytesPerSecond: Number.isFinite(bytesPerSecond) && bytesPerSecond > 0
      ? bytesPerSecond
      : previous.bytesPerSecond,
  };
}

export function transferMetricsLabel(
  metrics: TransferMetrics | null,
  transferred: number,
  total: number,
): string {
  if (!metrics?.bytesPerSecond || total <= 0 || transferred >= total) {
    return "";
  }
  const remainingSeconds = (total - boundedBytes(transferred, total)) / metrics.bytesPerSecond;
  return `${formatTransferRate(metrics.bytesPerSecond)} · ${formatRemainingTime(remainingSeconds)}`;
}

export function queuedFilesSummary(files: Array<{ size: number }>): string {
  if (files.length === 0) return "";
  const total = files.reduce((sum, file) => {
    const size = Number.isFinite(file.size) && file.size > 0 ? file.size : 0;
    return sum + size;
  }, 0);
  return `${files.length} 个文件${total > 0 ? `，共 ${formatBytes(total)}` : ""}`;
}

export function formatTransferRate(bytesPerSecond: number): string {
  return `${formatBytes(Math.max(0, bytesPerSecond))}/秒`;
}

export function formatRemainingTime(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return "";
  if (seconds < 1) return "预计不到 1 秒";
  if (seconds < 60) return `预计 ${Math.ceil(seconds)} 秒`;
  if (seconds < 3_600) return `预计 ${Math.ceil(seconds / 60)} 分钟`;
  if (seconds < 86_400) return `预计 ${Math.ceil(seconds / 3_600)} 小时`;
  return "预计超过 1 天";
}

function boundedBytes(value: number, total: number): number {
  if (!Number.isFinite(value) || value <= 0) return 0;
  if (!Number.isFinite(total) || total <= 0) return value;
  return Math.min(value, total);
}

function formatBytes(bytes: number): string {
  if (bytes < 1_024) return `${Math.round(bytes)} B`;
  if (bytes < 1_024 * 1_024) return `${(bytes / 1_024).toFixed(1)} KB`;
  if (bytes < 1_024 * 1_024 * 1_024) return `${(bytes / (1_024 * 1_024)).toFixed(1)} MB`;
  return `${(bytes / (1_024 * 1_024 * 1_024)).toFixed(1)} GB`;
}
