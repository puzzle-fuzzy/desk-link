import { Database } from "bun:sqlite";
import { createHash } from "node:crypto";

import type { DiagnosticBatch, DiagnosticEvent } from "./validation";

const RETENTION_SECONDS = 14 * 24 * 60 * 60;

export interface StoredBatchResult {
  duplicate: boolean;
  acceptedEvents: number;
}

export class DiagnosticStore {
  readonly database: Database;

  constructor(path: string) {
    this.database = new Database(path, { create: true, strict: true });
    this.database.exec("PRAGMA journal_mode = WAL");
    this.database.exec("PRAGMA synchronous = NORMAL");
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
    `);
  }

  insert(
    batchId: string,
    batch: DiagnosticBatch,
    remoteAddress: string,
    receivedAtUnixS = Math.floor(Date.now() / 1_000),
  ): StoredBatchResult {
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

    this.purge(receivedAtUnixS - RETENTION_SECONDS);
    return { duplicate: false, acceptedEvents };
  }

  purge(beforeUnixS: number): void {
    this.database.query("DELETE FROM events WHERE received_at_unix_s < ?").run(beforeUnixS);
    this.database.query("DELETE FROM batches WHERE received_at_unix_s < ?").run(beforeUnixS);
  }

  close(): void {
    this.database.close(false);
  }
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
