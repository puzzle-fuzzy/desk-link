import { Database } from "bun:sqlite";

import { analyzeDiagnosticRows } from "./analysis";
import { readAnalysisRows } from "./analysis-store";

const databasePath = process.env.DESKLINK_DIAGNOSTICS_DATABASE
  ?? "/var/lib/desklink-diagnostics/diagnostics.sqlite";
const hours = boundedInteger(argument("--hours") ?? "24", 1, 24 * 14);
const limit = boundedInteger(argument("--limit") ?? "10000", 1, 50_000);
const correlation = argument("--correlation")?.trim().toLowerCase() ?? null;
if (correlation !== null && !/^[0-9a-f]{32}$/.test(correlation)) {
  throw new Error("--correlation must be a 32-character lowercase hexadecimal identifier");
}

const database = new Database(databasePath, { readonly: true, strict: true });
const sinceUnixMs = Date.now() - hours * 60 * 60 * 1_000;
const rows = readAnalysisRows(database, sinceUnixMs, limit, correlation);
const report = analyzeDiagnosticRows(rows, hours);
console.log(JSON.stringify(report, null, 2));
database.close(false);

const failOnAlert = process.argv.includes("--fail-on-alert");
if (failOnAlert && (report.summary.error > 0 || report.summary.warning > 0)) {
  process.exitCode = 2;
}

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
