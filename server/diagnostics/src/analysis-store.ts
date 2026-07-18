import type { Database } from "bun:sqlite";

import type { AnalysisEventRow } from "./analysis";

export function readAnalysisRows(
  database: Database,
  sinceUnixMs: number,
  limit: number,
  correlation: string | null = null,
): AnalysisEventRow[] {
  const query = correlation
    ? `SELECT timestamp_unix_ms, correlation_id, app_version, source, level, event, payload_json
         FROM events
         WHERE timestamp_unix_ms >= ? AND correlation_id = ?
         ORDER BY timestamp_unix_ms ASC LIMIT ?`
    : `SELECT timestamp_unix_ms, correlation_id, app_version, source, level, event, payload_json
         FROM events
         WHERE timestamp_unix_ms >= ?
         ORDER BY timestamp_unix_ms ASC LIMIT ?`;
  const parameters = correlation ? [sinceUnixMs, correlation, limit] : [sinceUnixMs, limit];
  return database.query(query).all(...parameters) as unknown as AnalysisEventRow[];
}
