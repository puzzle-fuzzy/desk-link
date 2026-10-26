import { expect, spyOn, test } from "bun:test";
import { createHash, generateKeyPairSync, sign } from "node:crypto";
import { mkdtempSync, rmSync } from "node:fs";
import { createConnection } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createDiagnosticsApp } from "./app";
import { DiagnosticMaintenance } from "./maintenance";
import { createDiagnosticsRuntime } from "./runtime";
import { DiagnosticStore } from "./store";
import { installationIdentifier, SIGNATURE_DOMAIN } from "./validation";

const connectable = (port: number) => new Promise<boolean>(resolve => {
  const socket = createConnection({ host: "127.0.0.1", port });
  socket.once("connect", () => { socket.destroy(); resolve(true); });
  socket.once("error", () => { socket.destroy(); resolve(false); });
});

test("real signed HTTP receipt commits while shutdown drains, then timers and every SQLite statement are released", async () => {
  const directory = mkdtempSync(join(tmpdir(), "desklink-diagnostic-shutdown-"));
  const store = new DiagnosticStore(join(directory, "local.sqlite"));
  const maintenance = new DiagnosticMaintenance(store);
  const close = spyOn(store, "close");
  let release!: () => void, entered!: () => void;
  const gate = new Promise<void>(resolve => { release = resolve; });
  const started = new Promise<void>(resolve => { entered = resolve; });
  const base = createDiagnosticsApp({ store });
  const runtime = createDiagnosticsRuntime(store, maintenance, {
    async fetch(request, ip) { entered(); await gate; return base.fetch(request, ip); },
  }, { hostname: "127.0.0.1", port: 0 });
  const port = runtime.server.port!;
  const { publicKey, privateKey } = generateKeyPairSync("ed25519");
  const key = Buffer.from(publicKey.export({ type: "spki", format: "der" })).subarray(-32);
  const body = Buffer.from(JSON.stringify({ schema: 1, platform: "windows", source: "controller", app_version: "0.1.0",
    installation_id: installationIdentifier(key), correlation_id: null,
    events: [{ schema: 1, timestamp_unix_ms: Date.now(), level: "info", event: "controller_connected" }],
  }));
  const response = fetch(`http://127.0.0.1:${port}/v1/batches`, { method: "POST", body, headers: {
    "content-type": "application/json", "content-length": String(body.byteLength), "x-desklink-public-key": key.toString("hex"),
    "x-desklink-batch-id": createHash("blake2s256").update(body).digest("hex"),
    "x-desklink-signature": sign(null, Buffer.concat([SIGNATURE_DOMAIN, body]), privateKey).toString("hex"),
  } });
  try {
    await started;
    const closing = runtime.shutdown();
    expect(runtime.shutdown()).toBe(closing);
    expect(await connectable(port)).toBe(false);
    expect(close).not.toHaveBeenCalled();
    release();
    const receipt = await response;
    expect(receipt.status).toBe(202);
    expect((await receipt.json()).accepted_events).toBe(1);
    await closing;
    expect(close).toHaveBeenCalledTimes(1);
    expect(maintenance.runOnce()).toBeUndefined();
    expect(() => store.usage()).toThrow();
    // No GC/deletion retries: all cached native statements and the file closed.
    rmSync(directory, { recursive: true });
  } finally { release(); await runtime.shutdown(); close.mockRestore(); }
});

test("cache-sized fixed query workloads and failed initialization release real files without waiting for GC", () => {
  for (const fails of [false, true]) {
    const directory = mkdtempSync(join(tmpdir(), "desklink-diagnostic-close-"));
    const path = join(directory, "local.sqlite");
    if (fails) expect(() => new DiagnosticStore(path, { events: 0, batches: 1, bytes: 1 })).toThrow();
    else {
      const store = new DiagnosticStore(path);
      const first = store.database.query("SELECT 1 AS n");
      for (let pass = 0; pass < 3; pass++) for (let index = 0; index < 200; index++) store.database.query(`SELECT ${index} AS n`).get();
      expect(store.database.query("SELECT 1 AS n")).toBe(first);
      store.close();
    }
    rmSync(directory, { recursive: true });
  }
});
