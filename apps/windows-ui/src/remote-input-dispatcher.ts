import type { ControllerInput } from "./types";

type InputSender = (input: ControllerInput, streamId: number) => Promise<void>;

interface QueuedInput {
  input: ControllerInput;
  streamId: number;
}

/**
 * Keeps discrete input ordered while collapsing consecutive pointer moves.
 * At most one Tauri IPC call is in flight, so a high-polling-rate mouse cannot
 * build an unbounded Promise/command backlog on the WebView main thread.
 */
export class RemoteInputDispatcher {
  private readonly queue: QueuedInput[] = [];
  private queueHead = 0;
  private pumping: Promise<void> | null = null;
  private readonly pendingMouseMove = { x: 0, y: 0, streamId: 0 };
  private hasPendingMouseMove = false;

  constructor(
    private readonly sender: InputSender,
    private readonly onError: () => void = () => {},
  ) {}

  enqueue(input: ControllerInput, streamId: number): void {
    this.flushPendingMouseMove();
    const last = this.queue.length > this.queueHead
      ? this.queue[this.queue.length - 1]
      : undefined;
    const queued = { input, streamId };
    if (
      input.kind === "mouseMove"
      && last?.input.kind === "mouseMove"
      && last.streamId === streamId
    ) {
      this.queue[this.queue.length - 1] = queued;
    } else {
      this.queue.push(queued);
    }
    this.startPump();
  }

  /**
   * Queues a pointer move without allocating a ControllerInput for every
   * hardware event while an IPC call is still in flight. Discrete input is
   * flushed ahead of button/key events so their original order is preserved.
   */
  enqueueMouseMove(x: number, y: number, streamId: number): void {
    const last = this.queue.length > this.queueHead
      ? this.queue[this.queue.length - 1]
      : undefined;
    if (last?.input.kind === "mouseMove" && last.streamId === streamId) {
      last.input.x = x;
      last.input.y = y;
    } else if (last) {
      this.queue.push({ input: { kind: "mouseMove", x, y }, streamId });
    } else if (this.hasPendingMouseMove && this.pendingMouseMove.streamId !== streamId) {
      this.flushPendingMouseMove();
      this.queue.push({ input: { kind: "mouseMove", x, y }, streamId });
    } else {
      this.pendingMouseMove.x = x;
      this.pendingMouseMove.y = y;
      this.pendingMouseMove.streamId = streamId;
      this.hasPendingMouseMove = true;
    }
    this.startPump();
  }

  discardPendingMoves(): void {
    this.hasPendingMouseMove = false;
    for (let index = this.queue.length - 1; index >= this.queueHead; index -= 1) {
      if (this.queue[index]?.input.kind === "mouseMove") {
        this.queue.splice(index, 1);
      }
    }
    this.compactQueue();
  }

  discardAll(): void {
    this.queue.length = 0;
    this.queueHead = 0;
    this.hasPendingMouseMove = false;
  }

  async drain(): Promise<void> {
    while (this.pumping) {
      await this.pumping;
    }
  }

  private startPump(): void {
    if (this.pumping) {
      return;
    }
    this.pumping = this.pump().finally(() => {
      this.pumping = null;
      if (this.queue.length > 0 || this.hasPendingMouseMove) {
        this.startPump();
      }
    });
  }

  private async pump(): Promise<void> {
    while (true) {
      while (this.queueHead < this.queue.length) {
        const queued = this.queue[this.queueHead];
        this.queueHead += 1;
        if (!queued) {
          continue;
        }
        try {
          await this.sender(queued.input, queued.streamId);
        } catch {
          this.onError();
        }
      }
      this.compactQueue();
      if (!this.hasPendingMouseMove) {
        return;
      }
      const { x, y, streamId } = this.pendingMouseMove;
      this.hasPendingMouseMove = false;
      try {
        await this.sender({ kind: "mouseMove", x, y }, streamId);
      } catch {
        this.onError();
      }
    }
  }

  private flushPendingMouseMove(): void {
    if (!this.hasPendingMouseMove) {
      return;
    }
    this.queue.push({
      input: {
        kind: "mouseMove",
        x: this.pendingMouseMove.x,
        y: this.pendingMouseMove.y,
      },
      streamId: this.pendingMouseMove.streamId,
    });
    this.hasPendingMouseMove = false;
  }

  private compactQueue(): void {
    if (this.queueHead === 0) {
      return;
    }
    if (this.queueHead >= this.queue.length) {
      this.queue.length = 0;
      this.queueHead = 0;
      return;
    }
    if (this.queueHead >= 32 && this.queueHead * 2 >= this.queue.length) {
      this.queue.splice(0, this.queueHead);
      this.queueHead = 0;
    }
  }
}
