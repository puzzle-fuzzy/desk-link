export type RenderFrameHandle = number;
export type RenderFrameRequest = (callback: () => void) => RenderFrameHandle;
export type RenderFrameCancel = (handle: RenderFrameHandle) => void;

/**
 * Coalesces state-driven renders into one animation-frame commit. Remote video
 * and input stay on their own hot paths; this only limits bursts of UI chrome
 * updates from arriving in the same task.
 */
export class RenderScheduler {
  private frame: RenderFrameHandle | null = null;
  private generation = 0;

  constructor(
    private readonly requestFrame: RenderFrameRequest,
    private readonly cancelFrame: RenderFrameCancel = () => {},
  ) {}

  schedule(render: () => void): void {
    if (this.frame !== null) {
      return;
    }
    const generation = ++this.generation;
    this.frame = this.requestFrame(() => {
      if (generation !== this.generation) {
        return;
      }
      this.frame = null;
      render();
    });
  }

  cancel(): void {
    if (this.frame === null) {
      return;
    }
    this.generation += 1;
    this.cancelFrame(this.frame);
    this.frame = null;
  }

  get pending(): boolean {
    return this.frame !== null;
  }
}
