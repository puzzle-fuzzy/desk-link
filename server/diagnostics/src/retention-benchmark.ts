// Local synthetic SQLite benchmark; never opens a configured service database.
import { cpus, tmpdir } from "node:os";
import { mkdirSync, mkdtempSync, rmSync, rmdirSync } from "node:fs";
import { join, resolve } from "node:path";
import { DiagnosticStore, PURGE_BATCH_SIZE, RETENTION_SECONDS } from "./store";
import type { DiagnosticBatch } from "./validation";

const root = resolve(process.env.DESKLINK_DIAGNOSTICS_BENCH_DIR ?? tmpdir());
mkdirSync(root, { recursive: true });
const directory = mkdtempSync(join(root, "desklink-retention-bench-"));
const events = Number(process.env.DESKLINK_DIAGNOSTICS_BENCH_EVENTS ?? "250000");
if (!Number.isSafeInteger(events) || events < 1_000 || events % 5 !== 0) {
  throw new RangeError("benchmark events must be a positive multiple of 5, at least 1000");
}
const now = 2_000_000_000;
const cutoff = now - RETENTION_SECONDS;
const payload = JSON.stringify({ schema: 1, timestamp_unix_ms: now * 1_000,
  level: "info", event: "synthetic_retention_benchmark", reason: "synthetic".repeat(32) });
const databases: string[] = [];

function timed<T>(operation: () => T): { ms: number; value: T } {
  const start = performance.now();
  const value = operation();
  return { ms: performance.now() - start, value };
}
function distribution(values: number[]) {
  const sorted = [...values].sort((a, b) => a - b);
  const percentile = (p: number) => sorted[Math.min(sorted.length - 1, Math.ceil(sorted.length * p) - 1)]!;
  return { samples: values.length, p50: percentile(0.5), p95: percentile(0.95), max: sorted.at(-1)! };
}
function seed(store: DiagnosticStore, prefix: string, count: number, received: number) {
  const batch = store.database.query(`INSERT INTO batches VALUES (?, ?, 'synthetic', NULL, '0.1.0', 'controller', 5, 'synthetic')`);
  const event = store.database.query(`INSERT INTO events VALUES (?, ?, ?, 'synthetic', NULL, '0.1.0', 'controller', ?, 'info', 'synthetic_retention_benchmark', ?)`);
  const insert = store.database.transaction((start: number, end: number) => {
    for (let index = start; index < end; index++) {
      const batchId = `${prefix}-b-${Math.floor(index / 5)}`;
      if (index % 5 === 0) batch.run(batchId, received);
      event.run(`${prefix}-e-${index}`, batchId, received, index, payload);
    }
  });
  for (let offset = 0; offset < count; offset += 5_000) insert(offset, Math.min(count, offset + 5_000));
  store.checkpoint();
}
function ingest(store: DiagnosticStore, index: number) {
  const batch: DiagnosticBatch = {
    schema: 1, app_version: "0.1.0", platform: "windows", source: "controller",
    installation_id: "1".repeat(32), correlation_id: null,
    events: Array.from({ length: 5 }, (_, offset) => ({ schema: 1,
      timestamp_unix_ms: index * 5 + offset, level: "info", event: "synthetic_ingest" })),
  };
  return store.insert(`ingest-${index}`, batch, "synthetic", now);
}
function run(baseline: boolean) {
  const path = join(directory, `${baseline ? "baseline" : "bounded"}.sqlite`);
  databases.push(path);
  const store = new DiagnosticStore(path);
  try {
    if (baseline) store.database.exec("DROP INDEX events_by_received_at; DROP INDEX batches_by_received_at;");
    seed(store, "retained", events, cutoff);
    const plans = ["events", "batches"].map((table) => {
      const statement = store.database.prepare(`EXPLAIN QUERY PLAN SELECT rowid FROM ${table}
        WHERE received_at_unix_s < ? ORDER BY received_at_unix_s LIMIT ?`);
      try { return { table, plan: statement.all(cutoff, PURGE_BATCH_SIZE) }; }
      finally { statement.finalize(); }
    });
    const ingestTimes: number[] = [];
    const receive = (index: number) => baseline
      ? store.database.transaction(() => {
        ingest(store, index);
        store.database.query("DELETE FROM events WHERE received_at_unix_s < ?").run(cutoff);
        store.database.query("DELETE FROM batches WHERE received_at_unix_s < ?").run(cutoff);
      })()
      : ingest(store, index);
    for (let i = 0; i < 200; i++) {
      const measured = timed(() => receive(i));
      if (i >= 20) ingestTimes.push(measured.ms);
    }
    const storage: unknown[] = [{ phase: "retained", ...store.storageMetrics() }];
    const deletion: unknown[] = [];
    for (let cycle = 0; cycle < 2; cycle++) {
      seed(store, `expired-${cycle}`, events, cutoff - 1);
      storage.push({ phase: `seed-${cycle}`, ...store.storageMetrics() });
      const times: number[] = [];
      let deletedEvents = 0;
      let deletedBatches = 0;
      if (baseline) {
        const result = timed(() => store.database.transaction(() => ({
          events: store.database.query("DELETE FROM events WHERE received_at_unix_s < ?").run(cutoff).changes,
          batches: store.database.query("DELETE FROM batches WHERE received_at_unix_s < ?").run(cutoff).changes,
        }))());
        times.push(result.ms); deletedEvents = result.value.events; deletedBatches = result.value.batches;
      } else {
        for (;;) {
          const result = timed(() => store.purge(cutoff));
          times.push(result.ms); deletedEvents += result.value.events; deletedBatches += result.value.batches;
          if (!result.value.more) break;
          // The production scheduler releases the event loop/lock for 1s here.
          // No sleep in this microbenchmark; each call is a separate transaction.
        }
      }
      store.checkpoint();
      deletion.push({ cycle, deletedEvents, deletedBatches, transactionCallMs: distribution(times) });
      storage.push({ phase: `purged-${cycle}`, ...store.storageMetrics() });
    }
    return { kind: baseline ? "prior-unindexed-full-delete" : "indexed-bounded-maintenance",
      plans, ingestCallMs: distribution(ingestTimes), deletion, storage,
      finalEvents: store.database.query("SELECT COUNT(*) AS count FROM events").get(),
      finalBatches: store.database.query("SELECT COUNT(*) AS count FROM batches").get() };
  } finally { store.close(); }
}

try {
  console.log(JSON.stringify({ schema: 1, date: new Date().toISOString(), bun: Bun.version,
    platform: process.platform, cpu: cpus()[0]?.model, seedEventsPerPhase: events,
    seedBatchesPerPhase: events / 5, payloadBytes: Buffer.byteLength(payload),
    retentionDays: 14, purgeBatchSize: PURGE_BATCH_SIZE,
    note: "Local synthetic SQLite file test. Purge call wall time bounds writer-lock time, includes commit. Ingest timing excludes HTTP/signature verification. Fixed workload observed high-water is not an arbitrary-input disk cap. Main-file free pages are reused; no VACUUM was run.",
    runs: [run(true), run(false)] }, null, 2));
} finally {
  for (const path of databases) for (const suffix of ["", "-wal", "-shm"]) rmSync(path + suffix, { force: true });
  rmdirSync(directory);
}
