export interface VideoPullKey {
  streamId: number;
  configVersion: number;
}

type PullFrame<T> = (key: VideoPullKey) => Promise<T>;
type DeliverFrame<T> = (frame: T) => void;
type DeliveryErrorHandler = (error: unknown) => void;
type PullErrorHandler = (
  error: unknown,
  consecutiveFailures: number,
  retryDelayMs: number,
) => void;
type WaitForRetry = (delayMs: number) => Promise<void>;

const VIDEO_PULL_RETRY_DELAYS_MS = [100, 250, 500, 1_000, 2_000] as const;
const MAX_VIDEO_PULL_FAILURES = 1_000_000;

export function nextVideoPullFailureCount(current: number): number {
  return Math.min(MAX_VIDEO_PULL_FAILURES, Math.max(0, Math.floor(current)) + 1);
}

export function videoPullRetryDelay(consecutiveFailures: number): number {
  const index = Math.min(
    VIDEO_PULL_RETRY_DELAYS_MS.length - 1,
    Math.max(0, Math.floor(consecutiveFailures) - 1),
  );
  return VIDEO_PULL_RETRY_DELAYS_MS[index]!;
}

export class SerialVideoPull<T> {
  private generation = 0;
  private activeKey: string | null = null;

  constructor(
    private readonly waitForRetry: WaitForRetry = (delayMs) => new Promise((resolve) => {
      setTimeout(resolve, delayMs);
    }),
  ) {}

  start(
    key: VideoPullKey,
    pullFrame: PullFrame<T>,
    deliverFrame: DeliverFrame<T>,
    onDeliveryError?: DeliveryErrorHandler,
    onPullError?: PullErrorHandler,
  ): void {
    const keyId = videoPullKey(key);
    if (this.activeKey === keyId) {
      return;
    }

    const generation = ++this.generation;
    this.activeKey = keyId;
    void this.run(
      generation,
      keyId,
      { ...key },
      pullFrame,
      deliverFrame,
      onDeliveryError,
      onPullError,
    );
  }

  stop(): void {
    this.generation += 1;
    this.activeKey = null;
  }

  private async run(
    generation: number,
    keyId: string,
    key: VideoPullKey,
    pullFrame: PullFrame<T>,
    deliverFrame: DeliverFrame<T>,
    onDeliveryError?: DeliveryErrorHandler,
    onPullError?: PullErrorHandler,
  ): Promise<void> {
    let consecutiveFailures = 0;
    while (this.isCurrent(generation, keyId)) {
      let frame: T;
      try {
        frame = await pullFrame(key);
        consecutiveFailures = 0;
      } catch (error) {
        if (!this.isCurrent(generation, keyId)) {
          return;
        }
        consecutiveFailures = Math.min(Number.MAX_SAFE_INTEGER, consecutiveFailures + 1);
        const retryDelayMs = videoPullRetryDelay(consecutiveFailures);
        try {
          onPullError?.(error, consecutiveFailures, retryDelayMs);
        } catch {
          // Diagnostics are best-effort and must not stop video recovery.
        }
        try {
          await this.waitForRetry(retryDelayMs);
        } catch {
          return;
        }
        continue;
      }

      if (!this.isCurrent(generation, keyId)) {
        return;
      }

      try {
        deliverFrame(frame);
      } catch (error) {
        try {
          onDeliveryError?.(error);
        } catch {
          // Error reporting is best-effort and must not stop the bounded pull loop.
        }
      }
    }
  }

  private isCurrent(generation: number, keyId: string): boolean {
    return generation === this.generation && this.activeKey === keyId;
  }
}

function videoPullKey(key: VideoPullKey): string {
  return `${key.streamId}:${key.configVersion}`;
}
