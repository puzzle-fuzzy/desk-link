import { DiagnosticStore, RETENTION_SECONDS, type PurgeResult } from "./store";

// A bounded slice each second while catching up, then one inexpensive indexed
// probe per minute. Drain 1000 events/second, then 1000 batch markers/second.
export class DiagnosticMaintenance {
  private timer?: ReturnType<typeof setTimeout>;
  private stopped = false;
  private status: {
    state: "starting" | "ok" | "degraded";
    failures: number;
    durationMs: number;
    events: number;
    batches: number;
    more: boolean;
    databaseBytes: number;
    reusableBytes: number;
    walBytes: number;
  } = {
    state: "starting", failures: 0, durationMs: 0, events: 0, batches: 0,
    more: false, databaseBytes: 0, reusableBytes: 0, walBytes: 0,
  };

  constructor(
    private readonly store: DiagnosticStore,
    private readonly now: () => number = () => Math.floor(Date.now() / 1_000),
  ) {}

  snapshot() { return { ...this.status }; }

  runOnce(): PurgeResult | undefined {
    if (this.stopped) return undefined;
    const started = performance.now();
    try {
      const result = this.store.purge(this.now() - RETENTION_SECONDS);
      this.store.checkpoint();
      this.status = {
        ...this.status, ...result, ...this.store.storageMetrics(),
        durationMs: performance.now() - started, state: "ok",
      };
      return result;
    } catch {
      // Health exposes fixed categories only, never a database path or SQL error.
      this.status = { ...this.status, state: "degraded", failures: this.status.failures + 1,
        durationMs: performance.now() - started };
      return undefined;
    }
  }

  start(): void {
    if (this.stopped || this.timer) return;
    const tick = () => {
      this.timer = undefined;
      if (this.stopped) return;
      const result = this.runOnce();
      this.timer = setTimeout(tick, result?.more ? 1_000 : 60_000);
      this.timer.unref();
    };
    this.timer = setTimeout(tick, 0);
    this.timer.unref();
  }

  stop(): void {
    this.stopped = true;
    if (this.timer) clearTimeout(this.timer);
    this.timer = undefined;
  }
}
