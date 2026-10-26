import { afterEach, describe, expect, test } from "bun:test";
import { createHash, generateKeyPairSync, sign } from "node:crypto";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createDiagnosticsApp } from "./app";
import { DEFAULT_ADMISSION, DiagnosticAdmission, trustedProxySet, type AdmissionOptions } from "./admission";
import { DiagnosticStore } from "./store";
import { DiagnosticStorageFull } from "./storage-quota";
import { diagnosticConfiguration } from "./configuration";
import { installationIdentifier, MAX_BATCH_BYTES, SIGNATURE_DOMAIN, type DiagnosticBatch } from "./validation";

const stores: DiagnosticStore[] = [];
const directories: string[] = [];
const now = 2_000_000_000_000;
afterEach(() => {
  for (const store of stores.splice(0)) store.close();
  for (const directory of directories.splice(0)) rmSync(directory, { recursive: true, force: true });
});
function store(options?: ConstructorParameters<typeof DiagnosticStore>[1]) {
  const result = new DiagnosticStore(":memory:", options); stores.push(result); return result;
}
function limits(overrides: Partial<AdmissionOptions> = {}) {
  return new DiagnosticAdmission({ ...DEFAULT_ADMISSION,
    global: { requests: 100, bytes: 100 * MAX_BATCH_BYTES, events: 10_000 },
    ip: { requests: 100, bytes: 100 * MAX_BATCH_BYTES, events: 10_000 },
    ...overrides });
}
function signed(count = 1, extra: Partial<DiagnosticBatch> = {}) {
  const { publicKey, privateKey } = generateKeyPairSync("ed25519");
  const rawKey = publicKey.export({ format: "der", type: "spki" }).subarray(-32);
  const batch: DiagnosticBatch = { schema: 1, app_version: "0.1.0", platform: "windows", source: "controller",
    installation_id: installationIdentifier(rawKey), correlation_id: null,
    events: Array.from({ length: count }, (_, index) => ({ schema: 1, timestamp_unix_ms: now + index,
      level: "info", event: "synthetic_ingestion", reason: "合成诊断" })), ...extra };
  const body = Buffer.from(JSON.stringify(batch));
  const headers = new Headers({ "content-type": "application/json", "content-length": String(body.byteLength),
    "x-desklink-public-key": rawKey.toString("hex"),
    "x-desklink-signature": sign(null, Buffer.concat([SIGNATURE_DOMAIN, body]), privateKey).toString("hex"),
    "x-desklink-batch-id": createHash("blake2s256").update(body).digest("hex") });
  const request = () => new Request("http://synthetic/v1/batches", { method: "POST", headers, body });
  return { batch, headers, body, request, batchId: headers.get("x-desklink-batch-id")! };
}

describe("diagnostic ingress admission", () => {
  test("socket addresses ignore spoofed headers; explicit nginx trust records the overwritten real IP", async () => {
    const database = store();
    const fixture = signed();
    const request = fixture.request(); request.headers.set("x-real-ip", "192.0.2.99"); request.headers.set("x-forwarded-for", "198.51.100.99");
    const direct = createDiagnosticsApp({ store: database, now: () => now });
    expect((await direct.fetch(request, "192.0.2.1")).status).toBe(202);
    expect(database.database.query("SELECT remote_address FROM batches").get()).toEqual({ remote_address: "192.0.2.1" });
    const proxy = createDiagnosticsApp({ store: database, now: () => now, trustedProxies: trustedProxySet("127.0.0.1") });
    const proxied = signed().request(); proxied.headers.set("x-real-ip", "203.0.113.7"); proxied.headers.set("x-forwarded-for", "192.0.2.200, 203.0.113.7");
    expect((await proxy.fetch(proxied, "127.0.0.1")).status).toBe(202);
    expect(database.database.query("SELECT remote_address FROM batches WHERE remote_address = '203.0.113.7'").all()).toHaveLength(1);
    const forged = signed().request(); forged.headers.set("x-real-ip", "192.0.2.1, 192.0.2.2");
    expect((await proxy.fetch(forged, "127.0.0.1")).status).toBe(400);
    expect(forged.bodyUsed).toBe(false);
  });

  test("fresh signing keys share the same pre-body IP budget", async () => {
    const database = store();
    const app = createDiagnosticsApp({ store: database, now: () => now,
      admission: limits({ ip: { requests: 3, bytes: 100 * MAX_BATCH_BYTES, events: 10_000 } }) });
    for (let index = 0; index < 20; index++) {
      const request = signed().request();
      request.headers.set("x-forwarded-for", `198.51.100.${index}`);
      const response = await app.fetch(request, "192.0.2.1");
      expect(response.status).toBe(index < 3 ? 202 : 429);
      expect(request.bodyUsed).toBe(index < 3);
    }
    expect(database.usage()).toMatchObject({ events: 3, batches: 3 });
  });

  test("global request and byte limits reject different IPs before reading bodies", async () => {
    for (const global of [
      { requests: 1, bytes: 100 * MAX_BATCH_BYTES, events: 10_000 },
      { requests: 100, bytes: MAX_BATCH_BYTES, events: 10_000 },
    ]) {
      const database = store();
      const app = createDiagnosticsApp({ store: database, now: () => now, admission: limits({ global }) });
      expect((await app.fetch(signed().request(), "192.0.2.1")).status).toBe(202);
      const rejected = signed().request();
      expect((await app.fetch(rejected, "192.0.2.2")).status).toBe(429);
      expect(rejected.bodyUsed).toBe(false);
      expect(database.usage()).toMatchObject({ events: 1, batches: 1 });
    }
  });

  test("verified event totals combine rotating keys and different IPs", async () => {
    const database = store();
    const app = createDiagnosticsApp({ store: database, now: () => now,
      admission: limits({ global: { requests: 100, bytes: 100 * MAX_BATCH_BYTES, events: 3 } }) });
    expect((await app.fetch(signed(2).request(), "192.0.2.1")).status).toBe(202);
    expect((await app.fetch(signed(2).request(), "192.0.2.2")).status).toBe(429);
    expect(database.usage()).toMatchObject({ events: 2, batches: 1 });
  });

  test("actual loopback HTTP sockets defeat forwarded-header rotation before reading the next body", async () => {
    const database = store();
    const app = createDiagnosticsApp({ store: database, now: () => now,
      admission: limits({ ip: { requests: 1, bytes: 100 * MAX_BATCH_BYTES, events: 10_000 } }) });
    const bodyReads: boolean[] = [];
    const server = Bun.serve({ hostname: "127.0.0.1", port: 0, maxRequestBodySize: MAX_BATCH_BYTES,
      async fetch(request, instance) {
        const response = await app.fetch(request, instance.requestIP(request)?.address);
        bodyReads.push(request.bodyUsed); return response;
      } });
    try {
      for (let index = 0; index < 2; index++) {
        const fixture = signed(); fixture.headers.set("x-real-ip", `192.0.2.${index + 1}`);
        const response = await fetch(`http://127.0.0.1:${server.port}/v1/batches`,
          { method: "POST", headers: fixture.headers, body: fixture.body });
        expect(response.status).toBe(index === 0 ? 202 : 429);
        await response.arrayBuffer();
      }
      expect(bodyReads).toEqual([true, false]);
      expect(database.database.query("SELECT remote_address FROM batches").all()).toEqual([{ remote_address: "127.0.0.1" }]);
    } finally { await server.stop(true); }
  });

  test("200 fresh keys and socket IPs cannot grow beyond the persistent total quota", async () => {
    const database = store({ events: 8, batches: 8, bytes: 1_000_000 });
    const app = createDiagnosticsApp({ store: database, now: () => now,
      admission: limits({ global: { requests: 1_000, bytes: 1_000 * MAX_BATCH_BYTES, events: 10_000 } }) });
    for (let index = 0; index < 200; index++) {
      const response = await app.fetch(signed().request(), `192.0.2.${index + 1}`);
      expect(response.status).toBe(index < 8 ? 202 : 503);
    }
    expect(database.usage()).toMatchObject({ events: 8, batches: 8 });
    expect(database.database.query("SELECT COUNT(*) AS count FROM events").get()).toEqual({ count: 8 });
    expect(database.database.query("SELECT COUNT(*) AS count FROM batches").get()).toEqual({ count: 8 });
  });

  test("timeouts release concurrent admission and length/signature failures never store data", async () => {
    const database = store();
    const admission = limits({ maximumInFlight: 1 });
    const app = createDiagnosticsApp({ store: database, now: () => now, admission, bodyTimeoutMs: 10 });
    const fixture = signed();
    const stalled = new Request("http://synthetic/v1/batches", { method: "POST", headers: fixture.headers,
      body: new ReadableStream<Uint8Array>({ pull: () => new Promise(() => undefined) }) });
    const pending = app.fetch(stalled, "192.0.2.1");
    const excluded = signed().request();
    expect((await app.fetch(excluded, "192.0.2.2")).status).toBe(429);
    expect(excluded.bodyUsed).toBe(false);
    expect((await pending).status).toBe(408);
    expect(admission.snapshot().inFlight).toBe(0);
    const truncated = signed().request(); truncated.headers.set("content-length", "1");
    expect((await app.fetch(truncated, "192.0.2.2")).status).toBe(413);
    const short = signed().request(); short.headers.set("content-length", String(fixture.body.byteLength + 1));
    expect((await app.fetch(short, "192.0.2.2")).status).toBe(413);
    const signature = signed().request(); signature.headers.set("x-desklink-signature", "0".repeat(128));
    expect((await app.fetch(signature, "192.0.2.2")).status).toBe(401);
    expect(database.usage()).toMatchObject({ events: 0, batches: 0, storedBytes: 0 });
    expect(admission.snapshot().inFlight).toBe(0);
  });

  test("storage full still permits authenticated duplicate receipts", async () => {
    const database = store({ events: 1, batches: 10, bytes: 10_000_000 });
    const app = createDiagnosticsApp({ store: database, now: () => now });
    const fixture = signed();
    expect((await app.fetch(fixture.request(), "192.0.2.1")).status).toBe(202);
    const used = database.usage();
    const duplicate = await app.fetch(fixture.request(), "192.0.2.1");
    expect(duplicate.status).toBe(202);
    expect(await duplicate.json()).toMatchObject({ duplicate: true, accepted_events: 0 });
    const full = await app.fetch(signed().request(), "192.0.2.1");
    expect(full.status).toBe(503);
    expect(await full.json()).toEqual({ error: "storage_quota" });
    expect(database.usage()).toEqual(used);
  });

  test("replicated app metadata is bounded", async () => {
    const database = store();
    const app = createDiagnosticsApp({ store: database, now: () => now });
    expect((await app.fetch(signed(1, { app_version: `0.1.0-${"x".repeat(1000)}` }).request(), "192.0.2.1")).status).toBe(400);
    expect(database.usage()).toMatchObject({ events: 0, batches: 0, storedBytes: 0 });
  });

  test("deployment configuration requires explicit proxy trust and validates quota overrides", () => {
    const defaultConfig = diagnosticConfiguration({});
    expect(defaultConfig.trustedProxies.size).toBe(0);
    expect(defaultConfig.storageLimits).toEqual({ events: 1_000_000, batches: 250_000, bytes: 268_435_456 });
    const configured = diagnosticConfiguration({ DESKLINK_DIAGNOSTICS_TRUSTED_PROXIES: "127.0.0.1,::1",
      DESKLINK_DIAGNOSTICS_MAX_STORED_EVENTS: "8", DESKLINK_DIAGNOSTICS_GLOBAL_REQUESTS_PER_MINUTE: "9" });
    expect(configured.trustedProxies.has("127.0.0.1")).toBe(true);
    expect(configured.storageLimits.events).toBe(8);
    expect(configured.admission.options.global.requests).toBe(9);
    for (const text of ["0", "-1", "1.5", "Infinity", "9007199254740992"]) {
      expect(() => diagnosticConfiguration({ DESKLINK_DIAGNOSTICS_MAX_STORED_BYTES: text })).toThrow("invalid diagnostic setting");
    }
    expect(() => diagnosticConfiguration({ DESKLINK_DIAGNOSTICS_TRUSTED_PROXIES: "*" })).toThrow("invalid diagnostic trusted proxy address");
  });
});

describe("persistent diagnostic storage quota", () => {
  test("an over-budget multi-event batch rolls back its marker, every event and counters", () => {
    const database = store({ events: 2, batches: 10, bytes: 10_000_000 });
    const first = signed(); database.insert(first.batchId, first.batch, "192.0.2.1", 1);
    const before = database.usage();
    const oversized = signed(2);
    expect(() => database.insert(oversized.batchId, oversized.batch, "192.0.2.1", 1)).toThrow(DiagnosticStorageFull);
    expect(database.usage()).toEqual(before);
    expect(database.database.query("SELECT COUNT(*) AS count FROM events").get()).toEqual({ count: 1 });
    expect(database.database.query("SELECT COUNT(*) AS count FROM batches").get()).toEqual({ count: 1 });
  });

  test("UTF-8 quota bills all row text and rolls back before a byte overrun", () => {
    const fixture = signed();
    const reference = store(); reference.insert(fixture.batchId, fixture.batch, "192.0.2.1", 1);
    const bytes = reference.usage().storedBytes;
    expect(bytes).toBeGreaterThan(Buffer.byteLength(JSON.stringify(fixture.batch.events[0])));
    const database = store({ events: 100, batches: 100, bytes: bytes - 1 });
    expect(() => database.insert(fixture.batchId, fixture.batch, "192.0.2.1", 1)).toThrow(DiagnosticStorageFull);
    expect(database.usage()).toMatchObject({ events: 0, batches: 0, storedBytes: 0 });
    reference.database.query("UPDATE diagnostic_storage_usage SET maximum_stored_bytes = ?").run(bytes + 1);
    expect(() => reference.database.query("UPDATE events SET payload_json = ?").run("汉".repeat(1_000)))
      .toThrow("diagnostic_storage_quota");
    expect(reference.usage().storedBytes).toBe(bytes);
  });

  test("zero-new-event envelopes cannot bypass the independent batch cap", () => {
    const database = store({ events: 100, batches: 2, bytes: 10_000_000 });
    const fixture = signed();
    expect(database.insert("batch-a", fixture.batch, "192.0.2.1", 1).acceptedEvents).toBe(1);
    expect(database.insert("batch-b", { ...fixture.batch, app_version: "0.1.1" }, "192.0.2.1", 1).acceptedEvents).toBe(0);
    const before = database.usage();
    expect(() => database.insert("batch-c", fixture.batch, "192.0.2.1", 1)).toThrow(DiagnosticStorageFull);
    expect(database.usage()).toEqual(before);
    expect(before).toMatchObject({ events: 1, batches: 2 });
  });

  test("two file connections and restarts share counters; retention returns capacity", () => {
    const directory = mkdtempSync(join(tmpdir(), "desklink-quota-")); directories.push(directory);
    const path = join(directory, "events.sqlite");
    const quota = { events: 2, batches: 2, bytes: 10_000_000 };
    let first = new DiagnosticStore(path, quota);
    const second = new DiagnosticStore(path, quota);
    try {
      const a = signed(), b = signed(), c = signed();
      first.insert(a.batchId, a.batch, "192.0.2.1", 1);
      second.insert(b.batchId, b.batch, "192.0.2.2", 1);
      expect(() => first.insert(c.batchId, c.batch, "192.0.2.3", 1)).toThrow(DiagnosticStorageFull);
      const before = first.usage(); expect(second.usage()).toEqual(before);
      first.close(); first = new DiagnosticStore(path, quota);
      expect(first.usage()).toEqual(before);
      expect(first.purge(2)).toEqual({ events: 2, batches: 2, more: false });
      expect(second.usage()).toMatchObject({ events: 0, batches: 0, storedBytes: 0 });
      expect(first.insert(c.batchId, c.batch, "192.0.2.3", 2)).toEqual({ duplicate: false, acceptedEvents: 1 });
    } finally { first.close(); second.close(); }
  });

  test("legacy databases backfill exact usage once and may drain when already above a new cap", () => {
    const directory = mkdtempSync(join(tmpdir(), "desklink-quota-upgrade-")); directories.push(directory);
    const path = join(directory, "events.sqlite");
    let database = new DiagnosticStore(path);
    const fixture = signed(2); database.insert(fixture.batchId, fixture.batch, "192.0.2.1", 1);
    const used = database.usage();
    for (const table of ["events", "batches"]) {
      for (const action of ["insert_quota", "delete_usage", "update_quota"]) {
        database.database.exec(`DROP TRIGGER diagnostic_${table}_${action}`);
      }
    }
    database.database.exec("DROP TABLE diagnostic_storage_usage");
    database.close();
    database = new DiagnosticStore(path, { events: 1, batches: 1, bytes: used.storedBytes - 1 });
    try {
      expect(database.usage()).toMatchObject({ events: used.events, batches: used.batches, storedBytes: used.storedBytes });
      const next = signed();
      expect(() => database.insert(next.batchId, next.batch, "192.0.2.1", 2)).toThrow(DiagnosticStorageFull);
      expect(database.insert(fixture.batchId, fixture.batch, "192.0.2.1", 2).duplicate).toBe(true);
      expect(database.purge(2)).toEqual({ events: 2, batches: 1, more: false });
      expect(database.usage()).toMatchObject({ events: 0, batches: 0, storedBytes: 0 });
    } finally { database.close(); }
  });
});
