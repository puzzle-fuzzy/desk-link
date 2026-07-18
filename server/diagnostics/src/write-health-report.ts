import { Database } from "bun:sqlite";
import { chmodSync, renameSync, writeFileSync } from "node:fs";

import { analyzeDiagnosticRows } from "./analysis";
import { readAnalysisRows } from "./analysis-store";
import { buildDiagnosticHealthReport } from "./health";

const databasePath = process.env.DESKLINK_DIAGNOSTICS_DATABASE
  ?? "/var/lib/desklink-diagnostics/diagnostics.sqlite";
const outputPath = process.env.DESKLINK_DIAGNOSTICS_HEALTH_REPORT
  ?? "/var/lib/desklink-diagnostics/health-report.json";
const hours = 24;
const database = new Database(databasePath, { readonly: true, strict: true });
const rows = readAnalysisRows(database, Date.now() - hours * 60 * 60 * 1_000, 50_000);
database.close(false);

const analysis = analyzeDiagnosticRows(rows, hours);
const report = buildDiagnosticHealthReport(analysis);
const temporary = `${outputPath}.tmp`;
writeFileSync(temporary, `${JSON.stringify(report, null, 2)}\n`, { encoding: "utf8", mode: 0o600 });
chmodSync(temporary, 0o600);
renameSync(temporary, outputPath);
console.log(JSON.stringify({
  status: report.status,
  requires_attention: report.requires_attention,
  sessions: report.summary.sessions,
  errors: report.summary.error,
  warnings: report.summary.warning,
}));
