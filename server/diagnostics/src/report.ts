import { Database } from "bun:sqlite";

const databasePath = process.env.DESKLINK_DIAGNOSTICS_DATABASE ?? "/var/lib/desklink-diagnostics/diagnostics.sqlite";
const hours = boundedInteger(argument("--hours") ?? "24", 1, 24 * 14);
const limit = boundedInteger(argument("--limit") ?? "200", 1, 1_000);
const correlation = argument("--correlation")?.trim().toLowerCase() ?? null;
if (correlation !== null && !/^[0-9a-f]{32}$/.test(correlation)) {
  throw new Error("--correlation must be a 32-character lowercase hexadecimal identifier");
}

const database = new Database(databasePath, { readonly: true, strict: true });
const sinceUnixMs = Date.now() - hours * 60 * 60 * 1_000;
const rows = correlation
  ? database.query(`
      SELECT timestamp_unix_ms, received_at_unix_s, installation_id, correlation_id,
             app_version, source, level, event, payload_json
      FROM events
      WHERE timestamp_unix_ms >= ? AND correlation_id = ?
      ORDER BY timestamp_unix_ms DESC
      LIMIT ?
    `).all(sinceUnixMs, correlation, limit)
  : database.query(`
      SELECT timestamp_unix_ms, received_at_unix_s, installation_id, correlation_id,
             app_version, source, level, event, payload_json
      FROM events
      WHERE timestamp_unix_ms >= ? AND level IN ('warning', 'error')
      ORDER BY timestamp_unix_ms DESC
      LIMIT ?
    `).all(sinceUnixMs, limit);

console.log(JSON.stringify({
  schema: 1,
  generated_at: new Date().toISOString(),
  hours,
  correlation,
  count: rows.length,
  events: rows.map((row) => {
    const value = row as Record<string, string | number | null>;
    return {
      ...value,
      payload: JSON.parse(String(value.payload_json)),
      payload_json: undefined,
    };
  }),
}, null, 2));
database.close(false);

function argument(name: string): string | undefined {
  const index = process.argv.indexOf(name);
  return index >= 0 ? process.argv[index + 1] : undefined;
}

function boundedInteger(value: string, minimum: number, maximum: number): number {
  const parsed = Number(value);
  if (!Number.isInteger(parsed) || parsed < minimum || parsed > maximum) {
    throw new Error(`integer must be between ${minimum} and ${maximum}`);
  }
  return parsed;
}
