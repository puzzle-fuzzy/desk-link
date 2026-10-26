import { expect, test } from "bun:test";
import { createHash, generateKeyPairSync, sign } from "node:crypto";
import { readFileSync } from "node:fs";
import { createDiagnosticsApp } from "./app";
import { analyzeDiagnosticRows } from "./analysis";
import { readAnalysisRows } from "./analysis-store";
import { DiagnosticStore } from "./store";
import { installationIdentifier, SIGNATURE_DOMAIN, verifyBatch } from "./validation";

interface Fixture {
  producer_commit: string;
  sources: { path: string; git_blob: string; sha256: string }[];
  rustc_version: string;
  cases: { name: string; line: string }[];
}
const fixture = JSON.parse(readFileSync(new URL("../fixtures/host-media-listener-producer.json", import.meta.url), "utf8")) as Fixture;
const producerEvent = (name: string): Record<string, unknown> => JSON.parse(fixture.cases.find(value => value.name === name)!.line);
const now = Math.max(...fixture.cases.map(value => Number(JSON.parse(value.line).timestamp_unix_ms)));
const { publicKey, privateKey } = generateKeyPairSync("ed25519");
const rawKey = Buffer.from(publicKey.export({ format: "der", type: "spki" })).subarray(-32);

function signed(events: Record<string, unknown>[], source = "host") {
  const body = Buffer.from(JSON.stringify({ schema: 1, platform: "windows", app_version: "0.1.93", source,
    installation_id: installationIdentifier(rawKey), correlation_id: "a".repeat(32), events }));
  const headers = new Headers({ "content-type": "application/json", "content-length": String(body.byteLength),
    "x-desklink-public-key": rawKey.toString("hex"), "x-desklink-batch-id": createHash("blake2s256").update(body).digest("hex"),
    "x-desklink-signature": sign(null, Buffer.concat([SIGNATURE_DOMAIN, body]), privateKey).toString("hex"),
  });
  return { body, headers, request: () => new Request("http://test/v1/batches", { method: "POST", body, headers }) };
}

test("listener fixture is pinned to the actual terminal-listener Rust producer and its exact seven-field encoding", () => {
  expect(fixture.producer_commit).toBe("6dbe53d64c60bbdd50af4b592ebef7471b800fcc");
  expect(fixture.rustc_version).toContain("1.97.1");
  expect(fixture.sources.map(source => source.path)).toEqual(expect.arrayContaining([
    "apps/windows/src/diagnostics.rs", "apps/windows/src/runtime/media_diagnostics.rs", "apps/windows/src/runtime/cursor_send.rs",
  ]));
  for (const source of fixture.sources) {
    expect(source.git_blob).toMatch(/^[a-f0-9]{40}$/);
    expect(source.sha256).toMatch(/^[a-f0-9]{64}$/);
  }
  expect(fixture.cases.map(value => value.name)).toEqual(["retired_true", "retired_false", "max_safe", "u64_overflow"]);
  expect(producerEvent("retired_true")).toMatchObject({ event: "host_direct_listener_stopped", stream_id: 7,
    retired_active: true, level: "warning", reason: "listener_closed" });
  expect(producerEvent("retired_false").retired_active).toBe(false);
  expect(Object.keys(producerEvent("retired_true")).sort()).toEqual([
    "event", "level", "reason", "retired_active", "schema", "stream_id", "timestamp_unix_ms",
  ]);
});

test("actual Rust listener lines verify and atomically store alongside cursor and legacy events without loss inference", async () => {
  const store = new DiagnosticStore(":memory:");
  try {
    const legacy = { schema: 1, timestamp_unix_ms: now, level: "info", event: "host_available", stream_id: 7 };
    const cursor = { schema: 1, timestamp_unix_ms: now, level: "warning", event: "host_cursor_send", stream_id: 7,
      send_attempts: 2, send_errors: 1, disabled: true, reason: "capability_or_size" };
    const events = [legacy, cursor, producerEvent("retired_true"), producerEvent("retired_false"), producerEvent("max_safe")];
    const wire = signed(events);
    expect(JSON.stringify(verifyBatch(wire.body, wire.headers, now).batch.events)).toBe(JSON.stringify(events));
    const app = createDiagnosticsApp({ store, now: () => now });
    const response = await app.fetch(wire.request(), "192.0.2.1");
    expect(response.status).toBe(202);
    expect((await response.json()).accepted_events).toBe(5);
    const usage = store.usage();
    expect(await (await app.fetch(wire.request(), "192.0.2.1")).json()).toMatchObject({ duplicate: true, accepted_events: 0 });
    expect(store.usage()).toEqual(usage);
    const rows = readAnalysisRows(store.database, now - 60_000, 100, "a".repeat(32));
    expect(rows).toHaveLength(5);
    expect(rows.map(row => JSON.parse(row.payload_json))).toEqual(expect.arrayContaining(events));
    const session = analyzeDiagnosticRows(rows, 1, now).sessions[0]!;
    expect(session.findings.map(finding => finding.code)).toEqual(["cursor_lane_disabled", "direct_listener_stopped"]);
    expect(session.video.received_packets).toBe(0);
    expect(session.video.dropped_packets).toBe(0);
    expect(session.video.loss_percent).toBeNull();
    expect(session.stop_reason_kinds).toEqual([]);
    expect(session.outcome).toBe("warning");
    expect(session.findings.find(finding => finding.code === "direct_listener_stopped")?.detail).toContain("后续媒体事件");
  } finally { store.close(); }
});

test("retired_active remains listener-specific and malformed signed mixed batches have no stored side effects", async () => {
  const original = producerEvent("retired_true");
  const missing = Object.keys(original).map(key => { const result = { ...original }; delete result[key]; return result; });
  const invalid = [
    ...missing,
    { ...original, raw_error: "synthetic-private-error" }, { ...original, endpoint: "synthetic-private-address" },
    { ...original, send_errors: 1 }, { ...original, event: "host_cursor_send" },
    { ...original, event: "host_direct_listener_stopped_v2" }, { ...original, reason: "capability_or_size" },
    { ...original, reason: "listener_closed: raw error" }, { ...original, level: "info" },
    { ...original, stream_id: 0 }, { ...original, stream_id: -1 }, { ...original, stream_id: "07" },
    { ...original, stream_id: 1.5 }, { ...original, stream_id: Number.MAX_SAFE_INTEGER + 1 },
    { ...original, retired_active: "true" }, { ...original, retired_active: 1 }, { ...original, retired_active: null },
    producerEvent("u64_overflow"),
  ];
  const store = new DiagnosticStore(":memory:");
  try {
    const app = createDiagnosticsApp({ store, now: () => now });
    for (let index = 0; index < invalid.length; index++) {
      const response = await app.fetch(signed([original, invalid[index]!]).request(), `192.0.2.${index + 1}`);
      expect(response.status).toBe(400);
      expect((await response.json()).error).toBe("invalid_batch");
      expect(store.usage()).toMatchObject({ events: 0, batches: 0, storedBytes: 0 });
    }
    expect((await app.fetch(signed([original], "controller").request(), "198.51.100.1")).status).toBe(400);
    const stderr = { kind: "direct_listener_stopped", stream_id: 7, retired_active: true, reason: "listener_closed" };
    expect((await app.fetch(signed([stderr]).request(), "198.51.100.2")).status).toBe(400);
  } finally { store.close(); }
});

test("listener bytes changed under a valid batch hash still require the matching producer signature", async () => {
  const wire = signed([producerEvent("retired_true")]);
  const tampered = Buffer.from(wire.body.toString().replace('"retired_active":true', '"retired_active":false'));
  const headers = new Headers(wire.headers);
  headers.set("x-desklink-batch-id", createHash("blake2s256").update(tampered).digest("hex"));
  headers.set("content-length", String(tampered.byteLength));
  const store = new DiagnosticStore(":memory:");
  try {
    const app = createDiagnosticsApp({ store, now: () => now });
    const response = await app.fetch(new Request("http://test/v1/batches", { method: "POST", body: tampered, headers }), "192.0.2.1");
    expect(response.status).toBe(401);
    expect(store.usage()).toMatchObject({ events: 0, batches: 0 });
  } finally { store.close(); }
});
