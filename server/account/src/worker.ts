import type { AccountApp } from "./app";

export class AccountWorker {
  private timer?: ReturnType<typeof setTimeout>;
  private stopped = false;
  private inFlight?: Promise<void>;
  private lastCleanup = -Infinity;

  constructor(private readonly app: AccountApp, private readonly now: () => number = Date.now) {}

  start(): void {
    if (this.stopped || this.timer || this.inFlight) return;
    const tick = () => {
      this.timer = undefined;
      if (this.stopped) return;
      this.inFlight = (async () => {
        try {
          await this.app.flushOutbox();
          if (!this.stopped && this.now() - this.lastCleanup >= 60_000) {
            this.app.cleanupHistory();
            this.lastCleanup = this.now();
          }
        } catch {
          console.error("account maintenance failed");
        } finally {
          this.inFlight = undefined;
          if (!this.stopped) {
            this.timer = setTimeout(tick, 1_000);
            this.timer.unref();
          }
        }
      })();
    };
    this.timer = setTimeout(tick, 0);
    this.timer.unref();
  }

  async stop(): Promise<void> {
    this.stopped = true;
    if (this.timer) clearTimeout(this.timer);
    this.timer = undefined;
    // close() stops new claims before waiting for the single in-flight send.
    await this.app.close();
    await this.inFlight;
  }
}
