import { mkdirSync } from "node:fs";
import { dirname } from "node:path";

import { DiagnosticStore } from "./store";
import { DiagnosticMaintenance } from "./maintenance";
import { MAX_BATCH_BYTES } from "./validation";
import { createDiagnosticsApp } from "./app";
import { diagnosticConfiguration } from "./configuration";

const address = process.env.DESKLINK_DIAGNOSTICS_ADDR ?? "127.0.0.1";
const port = parsePort(process.env.DESKLINK_DIAGNOSTICS_PORT ?? "3411");
const databasePath = process.env.DESKLINK_DIAGNOSTICS_DATABASE ?? "/var/lib/desklink-diagnostics/diagnostics.sqlite";
mkdirSync(dirname(databasePath), { recursive: true });
const configuration = diagnosticConfiguration(process.env);
const store = new DiagnosticStore(databasePath, configuration.storageLimits);
const maintenance = new DiagnosticMaintenance(store);
maintenance.start();
const app = createDiagnosticsApp({ store, retention: () => maintenance.snapshot(),
  trustedProxies: configuration.trustedProxies, admission: configuration.admission });

const server = Bun.serve({
  hostname: address,
  port,
  maxRequestBodySize: MAX_BATCH_BYTES,
  fetch(request, serverInstance) {
    return app.fetch(request, serverInstance.requestIP(request)?.address);
  },
});

console.log(`DeskLink diagnostics listening on http://${server.hostname}:${server.port}`);

let stopping = false;
async function shutdown() {
  if (stopping) return;
  stopping = true;
  maintenance.stop();
  await server.stop();
  store.close();
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
