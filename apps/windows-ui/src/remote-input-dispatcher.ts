import type { ControllerInput, RemoteInputContext } from "./types";
import { MAX_WHEEL_DELTA } from "./input-contract.generated";
import { validInputContext } from "./remote-input-context";

type InputSender = (input: ControllerInput, context: RemoteInputContext) => Promise<void>;
type TextSender = (text: string, context: RemoteInputContext) => Promise<void>;

interface QueuedInput extends RemoteInputContext {
  input?: ControllerInput;
  text?: string;
  shortcut?: ControllerInput;
  shortcutGeneration?: number;
  resolve?: () => void;
  reject?: (error: unknown) => void;
}

function sameDirection(left = 0, right = 0): boolean {
  return left === 0 || right === 0 || Math.sign(left) === Math.sign(right);
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
  private readonly pendingMouseMove = { x: 0, y: 0, streamId: "", inputEpoch: "" };
  private hasPendingMouseMove = false;
  private generation = 0;
  private shortcutGeneration = 0;

  constructor(
    private readonly sender: InputSender,
    private readonly onError: () => void = () => {},
    private readonly textSender?: TextSender,
  ) {}

  enqueue(input: ControllerInput, context: RemoteInputContext): void {
    if (!validInputContext(context)) { this.onError(); return; }
    const { streamId, inputEpoch } = context;
    if (input.kind === "wheel" && (!Number.isSafeInteger(input.deltaX ?? 0) || !Number.isSafeInteger(input.deltaY ?? 0))) {
      this.onError();
      return;
    }
    this.flushPendingMouseMove();
    const last = this.queue.length > this.queueHead
      ? this.queue[this.queue.length - 1]
      : undefined;
    const queued = { input, streamId, inputEpoch };
    if (
      input.kind === "wheel"
      && last?.input?.kind === "wheel"
      && last.streamId === streamId && last.inputEpoch === inputEpoch
      && sameDirection(last.input.deltaX, input.deltaX)
      && sameDirection(last.input.deltaY, input.deltaY)
      && Number.isSafeInteger((last.input.deltaX ?? 0) + (input.deltaX ?? 0))
      && Number.isSafeInteger((last.input.deltaY ?? 0) + (input.deltaY ?? 0))
    ) {
      last.input.deltaX = (last.input.deltaX ?? 0) + (input.deltaX ?? 0);
      last.input.deltaY = (last.input.deltaY ?? 0) + (input.deltaY ?? 0);
    } else if (
      input.kind === "mouseMove"
      && last?.input?.kind === "mouseMove"
      && last.streamId === streamId && last.inputEpoch === inputEpoch
    ) {
      this.queue[this.queue.length - 1] = queued;
    } else {
      this.queue.push(queued);
    }
    this.startPump();
  }

  enqueueText(text: string, context: RemoteInputContext): Promise<void> {
    if (!validInputContext(context)) return Promise.reject(new Error("Invalid remote input context"));
    const { streamId, inputEpoch } = context;
    this.flushPendingMouseMove();
    // The existing native text command accepts at most 256 Unicode scalars.
    const characters = Array.from(text);
    const pending: Promise<void>[] = [];
    for (let offset = 0; offset < characters.length; offset += 256) {
      pending.push(new Promise<void>((resolve, reject) => {
        this.queue.push({ text: characters.slice(offset, offset + 256).join(""), streamId, inputEpoch, resolve, reject });
      }));
    }
    this.startPump();
    return Promise.all(pending).then(() => {});
  }

  enqueueShortcut(input: ControllerInput, context: RemoteInputContext): Promise<void> {
    if (!validInputContext(context)) return Promise.reject(new Error("Invalid remote input context"));
    const { streamId, inputEpoch } = context;
    if (input.kind !== "key" || !input.key || input.pressed !== true) {
      return Promise.reject(new Error("Invalid remote shortcut"));
    }
    this.flushPendingMouseMove();
    const pending = new Promise<void>((resolve, reject) => {
      this.queue.push({ shortcut: { ...input }, shortcutGeneration: this.shortcutGeneration, streamId, inputEpoch, resolve, reject });
    });
    this.startPump();
    return pending;
  }

  cancelShortcuts(): void {
    this.shortcutGeneration += 1;
  }

  /**
   * Queues a pointer move without allocating a ControllerInput for every
   * hardware event while an IPC call is still in flight. Discrete input is
   * flushed ahead of button/key events so their original order is preserved.
   */
  enqueueMouseMove(x: number, y: number, context: RemoteInputContext): void {
    if (!validInputContext(context)) { this.onError(); return; }
    const { streamId, inputEpoch } = context;
    const last = this.queue.length > this.queueHead
      ? this.queue[this.queue.length - 1]
      : undefined;
    if (last?.input?.kind === "mouseMove" && last.streamId === streamId && last.inputEpoch === inputEpoch) {
      last.input.x = x;
      last.input.y = y;
    } else if (last) {
      this.queue.push({ input: { kind: "mouseMove", x, y }, streamId, inputEpoch });
    } else if (this.hasPendingMouseMove && (this.pendingMouseMove.streamId !== streamId || this.pendingMouseMove.inputEpoch !== inputEpoch)) {
      this.flushPendingMouseMove();
      this.queue.push({ input: { kind: "mouseMove", x, y }, streamId, inputEpoch });
    } else {
      this.pendingMouseMove.x = x;
      this.pendingMouseMove.y = y;
      this.pendingMouseMove.streamId = streamId;
      this.pendingMouseMove.inputEpoch = inputEpoch;
      this.hasPendingMouseMove = true;
    }
    this.startPump();
  }

  discardPendingMoves(): void {
    this.hasPendingMouseMove = false;
    for (let index = this.queue.length - 1; index >= this.queueHead; index -= 1) {
      if (this.queue[index]?.input?.kind === "mouseMove") {
        this.queue.splice(index, 1);
      }
    }
    this.compactQueue();
  }

  discardAll(): void {
    this.cancelShortcuts();
    this.generation += 1;
    for (let index = this.queueHead; index < this.queue.length; index += 1) {
      this.queue[index]?.reject?.(new Error("Remote input was cleared"));
    }
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
        const context = { streamId: queued.streamId, inputEpoch: queued.inputEpoch };
        try {
          if (queued.shortcut) {
            if (queued.shortcutGeneration !== this.shortcutGeneration) throw new Error("Remote shortcut was cancelled");
            // A single key plus its modifier mask is a complete native chord.
            // Key-up uses the same mask, releasing the main key before modifiers.
            // Always attempt it after an uncertain IPC result or cancellation.
            try {
              await this.sender(queued.shortcut, context);
              if (queued.shortcutGeneration !== this.shortcutGeneration) throw new Error("Remote shortcut was cancelled");
            } finally {
              await this.sender({ ...queued.shortcut, pressed: false }, context);
            }
          } else if (queued.text !== undefined) {
            if (!this.textSender) throw new Error("Remote text sender is unavailable");
            await this.textSender(queued.text, context);
          } else if (queued.input) {
            if (queued.input.kind === "wheel") {
              const generation = this.generation;
              let x = queued.input.deltaX ?? 0;
              let y = queued.input.deltaY ?? 0;
              while ((x !== 0 || y !== 0) && generation === this.generation) {
                const deltaX = Math.max(-MAX_WHEEL_DELTA, Math.min(MAX_WHEEL_DELTA, x));
                const deltaY = Math.max(-MAX_WHEEL_DELTA, Math.min(MAX_WHEEL_DELTA, y));
                await this.sender({ kind: "wheel", deltaX, deltaY }, context);
                x -= deltaX;
                y -= deltaY;
              }
            } else {
              await this.sender(queued.input, context);
            }
          }
          queued.resolve?.();
        } catch (error) {
          queued.reject?.(error);
          this.onError();
        }
      }
      this.compactQueue();
      if (!this.hasPendingMouseMove) {
        return;
      }
      const { x, y, streamId, inputEpoch } = this.pendingMouseMove;
      this.hasPendingMouseMove = false;
      try {
        await this.sender({ kind: "mouseMove", x, y }, { streamId, inputEpoch });
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
      inputEpoch: this.pendingMouseMove.inputEpoch,
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
