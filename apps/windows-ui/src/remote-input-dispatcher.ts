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
  private pumping: Promise<void> | null = null;

  constructor(
    private readonly sender: InputSender,
    private readonly onError: () => void = () => {},
  ) {}

  enqueue(input: ControllerInput, streamId: number): void {
    const last = this.queue.at(-1);
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

  discardPendingMoves(): void {
    for (let index = this.queue.length - 1; index >= 0; index -= 1) {
      if (this.queue[index]?.input.kind === "mouseMove") {
        this.queue.splice(index, 1);
      }
    }
  }

  discardAll(): void {
    this.queue.length = 0;
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
      if (this.queue.length > 0) {
        this.startPump();
      }
    });
  }

  private async pump(): Promise<void> {
    while (this.queue.length > 0) {
      const queued = this.queue.shift();
      if (!queued) {
        continue;
      }
      try {
        await this.sender(queued.input, queued.streamId);
      } catch {
        this.onError();
      }
    }
  }
}
