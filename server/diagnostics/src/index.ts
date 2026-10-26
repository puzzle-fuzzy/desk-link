import { mkdirSync } from "node:fs";
import { dirname } from "node:path";

import { DiagnosticStore } from "./store";
import { DiagnosticMaintenance } from "./maintenance";
import { createDiagnosticsRuntime } from "./runtime";
import { createDiagnosticsApp } from "./app";
import { diagnosticConfiguration } from "./configuration";

const address = process.env.DESKLINK_DIAGNOSTICS_ADDR ?? "127.0.0.1";
const port = parsePort(process.env.DESKLINK_DIAGNOSTICS_PORT ?? "3411");
const databasePath = process.env.DESKLINK_DIAGNOSTICS_DATABASE ?? "/var/lib/desklink-diagnostics/diagnostics.sqlite";
mkdirSync(dirname(databasePath), { recursive: true });
const configuration = diagnosticConfiguration(process.env);
const store = new DiagnosticStore(databasePath, configuration.storageLimits);
const maintenance = new DiagnosticMaintenance(store);
const app = createDiagnosticsApp({ store, retention: () => maintenance.snapshot(),
  trustedProxies: configuration.trustedProxies, admission: configuration.admission });

const runtime = createDiagnosticsRuntime(store, maintenance, app, {
  hostname: address,
  port,
});
const server = runtime.server;

console.log(`DeskLink diagnostics listening on http://${server.hostname}:${server.port}`);

function shutdown() {
  void runtime.shutdown().catch(() => {
    console.error("diagnostics shutdown failed");
    process.exitCode = 1;
  });
}
process.once("SIGTERM", shutdown);
process.once("SIGINT", shutdown);

function parsePort(value: string): number {
  const parsed = Number(value);
  if (!Number.isInteger(parsed) || parsed < 1 || parsed > 65_535) {
    throw new Error("DESKLINK_DIAGNOSTICS_PORT is invalid");
  }
  return parsed;
}
