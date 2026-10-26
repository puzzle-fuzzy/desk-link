import { Database } from "bun:sqlite";
import { createHash } from "node:crypto";
import { statSync } from "node:fs";
import { DiagnosticDatabase } from "./database";

import type { DiagnosticBatch, DiagnosticEvent } from "./validation";
import { configureStorageQuota, DEFAULT_STORAGE_LIMITS, DiagnosticStorageFull, storageUsage, type StorageLimits } from "./storage-quota";

export const RETENTION_SECONDS = 14 * 24 * 60 * 60;
export const PURGE_BATCH_SIZE = 1_000;

export interface PurgeResult {
  events: number;
  batches: number;
  more: boolean;
}

export interface StoredBatchResult {
  duplicate: boolean;
  acceptedEvents: number;
}

export class DiagnosticStore {
  readonly database: Database;

  constructor(private readonly path: string, limits: StorageLimits = DEFAULT_STORAGE_LIMITS) {
    this.database = new DiagnosticDatabase(path, { create: true, strict: true });
    try {
      this.database.exec("PRAGMA journal_mode = WAL");
      this.database.exec("PRAGMA synchronous = NORMAL");
      // Never wait on another writer from the ingestion event loop.
      this.database.exec("PRAGMA busy_timeout = 0");
      this.database.exec(`
        CREATE TABLE IF NOT EXISTS batches (
          batch_id TEXT PRIMARY KEY,
          received_at_unix_s INTEGER NOT NULL,
          installation_id TEXT NOT NULL,
          correlation_id TEXT,
          app_version TEXT NOT NULL,
          source TEXT NOT NULL,
          event_count INTEGER NOT NULL,
          remote_address TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS events (
          event_id TEXT PRIMARY KEY,
          batch_id TEXT NOT NULL,
          received_at_unix_s INTEGER NOT NULL,
          installation_id TEXT NOT NULL,
          correlation_id TEXT,
          app_version TEXT NOT NULL,
          source TEXT NOT NULL,
          timestamp_unix_ms INTEGER NOT NULL,
          level TEXT NOT NULL,
          event TEXT NOT NULL,
          payload_json TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS events_by_correlation
          ON events(correlation_id, timestamp_unix_ms);
        CREATE INDEX IF NOT EXISTS events_by_installation
          ON events(installation_id, timestamp_unix_ms);
        CREATE INDEX IF NOT EXISTS events_by_received_at
          ON events(received_at_unix_s);
        CREATE INDEX IF NOT EXISTS batches_by_received_at
          ON batches(received_at_unix_s);
      `);
      configureStorageQuota(this.database, limits);
    } catch (error) {
      this.database.close(true);
      throw error;
    }
  }

  insert(
    batchId: string,
    batch: DiagnosticBatch,
    remoteAddress: string,
    receivedAtUnixS = Math.floor(Date.now() / 1_000),
  ): StoredBatchResult {
    try { return this.database.transaction((): StoredBatchResult => {
      const insertBatch = this.database.query(`
        INSERT OR IGNORE INTO batches (
          batch_id, received_at_unix_s, installation_id, correlation_id,
          app_version, source, event_count, remote_address
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
      `);
      const result = insertBatch.run(
        batchId,
        receivedAtUnixS,
        batch.installation_id,
        batch.correlation_id,
        batch.app_version,
        batch.source,
        batch.events.length,
        remoteAddress.slice(0, 96),
      );
      if (result.changes === 0) {
        return { duplicate: true, acceptedEvents: 0 };
      }

      const insertEvent = this.database.query(`
        INSERT OR IGNORE INTO events (
          event_id, batch_id, received_at_unix_s, installation_id, correlation_id,
          app_version, source, timestamp_unix_ms, level, event, payload_json
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
      `);
      let acceptedEvents = 0;
      for (const event of batch.events) {
        const payload = JSON.stringify(event);
        const eventId = eventIdentifier(batch, event, payload);
        const inserted = insertEvent.run(
          eventId,
          batchId,
          receivedAtUnixS,
          batch.installation_id,
          batch.correlation_id,
          batch.app_version,
          batch.source,
          event.timestamp_unix_ms,
          event.level,
          event.event,
          payload,
        );
        acceptedEvents += Number(inserted.changes > 0);
      }

      return { duplicate: false, acceptedEvents };
    })(); } catch (error) {
      if (error instanceof Error && error.message.includes("diagnostic_storage_quota")) throw new DiagnosticStorageFull();
      throw error;
    }
  }

  usage() { return storageUsage(this.database); }

  // Each call holds the writer lock for at most two bounded deletes. Backlog is
  // drained by independent maintenance ticks, never by an ingestion transaction.
  purge(beforeUnixS: number, limit = PURGE_BATCH_SIZE): PurgeResult {
    if (!Number.isSafeInteger(limit) || limit < 1 || limit > 5_000) {
      throw new RangeError("diagnostic purge limit must be between 1 and 5000");
    }
    return this.database.transaction(() => {
      this.database.query(`DELETE FROM events WHERE rowid IN (
        SELECT rowid FROM events WHERE received_at_unix_s < ?
        ORDER BY received_at_unix_s LIMIT ?
      )`).run(beforeUnixS, limit);
      // Bun's run().changes includes trigger work. changes() is the top-level
      // deleted-row count used to decide whether this bounded slice drained.
      const events = (this.database.query("SELECT changes() AS count").get() as { count: number }).count;
      // All events of a batch have its receipt timestamp. Keep deduplication
      // markers until expired events have drained, so a bounded slice cannot
      // leave events pointing at an already-deleted batch. This also avoids a
      // correlated scan of every not-yet-drained batch on each maintenance tick.
      let batches = 0;
      if (events < limit) {
        this.database.query(`DELETE FROM batches WHERE rowid IN (
          SELECT rowid FROM batches WHERE received_at_unix_s < ?
          ORDER BY received_at_unix_s LIMIT ?
        )`).run(beforeUnixS, limit);
        batches = (this.database.query("SELECT changes() AS count").get() as { count: number }).count;
      }
      return { events, batches, more: events === limit || batches === limit };
    })();
  }

  storageMetrics() {
    // Read all rows to finish PRAGMA statements before a later checkpoint.
    const scalar = (pragma: string) => {
      const statement = this.database.prepare(pragma);
      try { return Object.values(statement.all()[0]!)[0] as number; }
      finally { statement.finalize(); }
    };
    const pageSize = scalar("PRAGMA page_size");
    return {
      databaseBytes: scalar("PRAGMA page_count") * pageSize,
      reusableBytes: scalar("PRAGMA freelist_count") * pageSize,
      walBytes: this.path === ":memory:" ? 0 : fileSize(`${this.path}-wal`),
    };
  }

  checkpoint(): void {
    // PASSIVE never waits for readers or writers. Free database pages are reused;
    // shrinking the main file requires a separately scheduled offline VACUUM.
    const statement = this.database.prepare("PRAGMA wal_checkpoint(PASSIVE)");
    try { statement.all(); } finally { statement.finalize(); }
  }

  close(): void {
    this.database.close(true);
  }
}

function fileSize(path: string): number {
  try { return statSync(path).size; } catch { return 0; }
}

function eventIdentifier(batch: DiagnosticBatch, event: DiagnosticEvent, payload: string): string {
  return createHash("blake2s256")
    .update("desklink-diagnostic-event-v1\0")
    .update(batch.installation_id)
    .update("\0")
    .update(batch.source)
    .update("\0")
    .update(batch.correlation_id ?? "")
    .update("\0")
    .update(String(event.timestamp_unix_ms))
    .update("\0")
    .update(payload)
    .digest("hex");
}
