export const VIDEO_QUEUE_OVERLOAD_THRESHOLD = 5;
export const VIDEO_QUEUE_OVERLOAD_SAMPLES = 3;
export const VIDEO_FRESHNESS_COOLDOWN_MS = 5_000;
export const MAX_REPORTED_DECODE_QUEUE_SIZE = 64;
export const MAX_REPORTED_FRESHNESS_RECOVERIES = 16;

export type VideoPlaybackPressureSample = {
  peakDecodeQueueSize: number;
  freshnessRecoveries: number;
};

export class VideoPlaybackPressure {
  private peakDecodeQueueSize = 0;
  private severeSamples = 0;
  private freshnessRecoveries = 0;
  private lastRecoveryAtMs: number | null = null;

  observe(queueSize: number, nowMs: number): "submit" | "recover" {
    const boundedQueue = Math.min(
      MAX_REPORTED_DECODE_QUEUE_SIZE,
      Math.max(0, Number.isFinite(queueSize) ? Math.trunc(queueSize) : 0),
    );
    this.peakDecodeQueueSize = Math.max(this.peakDecodeQueueSize, boundedQueue);
    if (boundedQueue < VIDEO_QUEUE_OVERLOAD_THRESHOLD) {
      this.severeSamples = 0;
      return "submit";
    }

    this.severeSamples = Math.min(VIDEO_QUEUE_OVERLOAD_SAMPLES, this.severeSamples + 1);
    if (this.severeSamples < VIDEO_QUEUE_OVERLOAD_SAMPLES) {
      return "submit";
    }
    this.severeSamples = 0;
    if (
      this.lastRecoveryAtMs !== null
      && nowMs - this.lastRecoveryAtMs < VIDEO_FRESHNESS_COOLDOWN_MS
    ) {
      return "submit";
    }

    this.lastRecoveryAtMs = nowMs;
    this.freshnessRecoveries = Math.min(
      MAX_REPORTED_FRESHNESS_RECOVERIES,
      this.freshnessRecoveries + 1,
    );
    return "recover";
  }

  takeSample(): VideoPlaybackPressureSample {
    const sample = {
      peakDecodeQueueSize: this.peakDecodeQueueSize,
      freshnessRecoveries: this.freshnessRecoveries,
    };
    this.peakDecodeQueueSize = 0;
    this.freshnessRecoveries = 0;
    return sample;
  }

  reset(): void {
    this.peakDecodeQueueSize = 0;
    this.severeSamples = 0;
    this.freshnessRecoveries = 0;
    this.lastRecoveryAtMs = null;
  }
}
