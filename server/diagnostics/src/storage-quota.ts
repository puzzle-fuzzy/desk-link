import type { Database } from "bun:sqlite";

export interface StorageLimits { events: number; batches: number; bytes: number }
export const DEFAULT_STORAGE_LIMITS: StorageLimits = {
  events: 1_000_000, batches: 250_000, bytes: 256 * 1_024 * 1_024,
};
export class DiagnosticStorageFull extends Error {
  constructor() { super("diagnostic storage quota reached"); }
}

// Bill all variable text, including metadata replicated into each event, in UTF-8
// bytes. Row caps additionally bound SQLite record and index overhead.
const batchColumns = ["batch_id", "installation_id", "correlation_id", "app_version", "source", "remote_address"];
const eventColumns = ["event_id", "batch_id", "installation_id", "correlation_id", "app_version", "source", "level", "event", "payload_json"];
function textBytes(columns: readonly string[], prefix = "") {
  return columns.map((column) => `COALESCE(length(CAST(${prefix}${column} AS BLOB)), 0)`).join(" + ");
}

export function configureStorageQuota(database: Database, limits: StorageLimits): void {
  for (const value of Object.values(limits)) {
    if (!Number.isSafeInteger(value) || value <= 0) throw new RangeError("invalid diagnostic storage limit");
  }
  database.transaction(() => {
    database.exec(`CREATE TABLE IF NOT EXISTS diagnostic_storage_usage (
      singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
      events INTEGER NOT NULL CHECK (events >= 0), batches INTEGER NOT NULL CHECK (batches >= 0),
      stored_bytes INTEGER NOT NULL CHECK (stored_bytes >= 0),
      maximum_events INTEGER NOT NULL, maximum_batches INTEGER NOT NULL, maximum_stored_bytes INTEGER NOT NULL
    );`);
    // One legacy backfill under a writer transaction; later starts touch one row.
    const exists = database.query("SELECT singleton FROM diagnostic_storage_usage WHERE singleton = 1").get();
    if (!exists) {
      database.query(`INSERT INTO diagnostic_storage_usage
        SELECT 1, (SELECT COUNT(*) FROM events), (SELECT COUNT(*) FROM batches),
          COALESCE((SELECT SUM(${textBytes(eventColumns)}) FROM events), 0)
          + COALESCE((SELECT SUM(${textBytes(batchColumns)}) FROM batches), 0), ?, ?, ?`)
        .run(limits.events, limits.batches, limits.bytes);
    } else {
      database.query(`UPDATE diagnostic_storage_usage SET maximum_events = ?, maximum_batches = ?, maximum_stored_bytes = ? WHERE singleton = 1`)
        .run(limits.events, limits.batches, limits.bytes);
    }
    for (const [table, columns, countColumn] of [["batches", batchColumns, "batches"], ["events", eventColumns, "events"]] as const) {
      const assertQuota = `SELECT CASE WHEN (SELECT events > maximum_events OR batches > maximum_batches
        OR stored_bytes > maximum_stored_bytes FROM diagnostic_storage_usage WHERE singleton = 1)
        THEN RAISE(ABORT, 'diagnostic_storage_quota') END;`;
      database.exec(`
        CREATE TRIGGER IF NOT EXISTS diagnostic_${table}_insert_quota AFTER INSERT ON ${table} BEGIN
          UPDATE diagnostic_storage_usage SET ${countColumn} = ${countColumn} + 1,
            stored_bytes = stored_bytes + ${textBytes(columns, "NEW.")} WHERE singleton = 1;
          ${assertQuota}
        END;
        CREATE TRIGGER IF NOT EXISTS diagnostic_${table}_delete_usage AFTER DELETE ON ${table} BEGIN
          UPDATE diagnostic_storage_usage SET ${countColumn} = ${countColumn} - 1,
            stored_bytes = stored_bytes - (${textBytes(columns, "OLD.")}) WHERE singleton = 1;
        END;
        CREATE TRIGGER IF NOT EXISTS diagnostic_${table}_update_quota AFTER UPDATE ON ${table} BEGIN
          UPDATE diagnostic_storage_usage SET stored_bytes = stored_bytes
            - (${textBytes(columns, "OLD.")}) + (${textBytes(columns, "NEW.")}) WHERE singleton = 1;
          ${assertQuota}
        END;
      `);
    }
  }).immediate();
}

export function storageUsage(database: Database) {
  return database.query(`SELECT events, batches, stored_bytes AS storedBytes,
    maximum_events AS maximumEvents, maximum_batches AS maximumBatches,
    maximum_stored_bytes AS maximumStoredBytes FROM diagnostic_storage_usage WHERE singleton = 1`).get() as {
      events: number; batches: number; storedBytes: number;
      maximumEvents: number; maximumBatches: number; maximumStoredBytes: number;
    };
}
