import type { ControllerInput } from "./types";

type InputSender = (input: ControllerInput) => Promise<void>;

/**
 * Keeps discrete input ordered while collapsing consecutive pointer moves.
 * At most one Tauri IPC call is in flight, so a high-polling-rate mouse cannot
 * build an unbounded Promise/command backlog on the WebView main thread.
 */
export class RemoteInputDispatcher {
  private readonly queue: ControllerInput[] = [];
  private pumping: Promise<void> | null = null;

  constructor(
    private readonly sender: InputSender,
    private readonly onError: () => void = () => {},
  ) {}

  enqueue(input: ControllerInput): void {
    const last = this.queue.at(-1);
    if (input.kind === "mouseMove" && last?.kind === "mouseMove") {
      this.queue[this.queue.length - 1] = input;
    } else {
      this.queue.push(input);
    }
    this.startPump();
  }

  discardPendingMoves(): void {
    for (let index = this.queue.length - 1; index >= 0; index -= 1) {
      if (this.queue[index]?.kind === "mouseMove") {
        this.queue.splice(index, 1);
      }
    }
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
      const input = this.queue.shift();
      if (!input) {
        continue;
      }
      try {
        await this.sender(input);
      } catch {
        this.onError();
      }
    }
  }
}
