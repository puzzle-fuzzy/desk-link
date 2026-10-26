import { expect, test } from "bun:test";
import { createHash, generateKeyPairSync, sign } from "node:crypto";
import { readFileSync } from "node:fs";
import { createDiagnosticsApp } from "./app";
import { analyzeDiagnosticRows } from "./analysis";
import { readAnalysisRows } from "./analysis-store";
import { DiagnosticStore } from "./store";
import { installationIdentifier, SIGNATURE_DOMAIN, verifyBatch } from "./validation";

interface ProducerFixture {
  producer_commit: string;
  sources: { path: string; git_blob: string; sha256: string }[];
  rustc_version: string;
  cases: { name: string; line: string }[];
}
const fixture = JSON.parse(readFileSync(new URL("../fixtures/host-media-cursor-producer.json", import.meta.url), "utf8")) as ProducerFixture;
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

test("fixture comes from the pinned real Rust product encoder, including safe and u64 boundary cases", () => {
  expect(fixture.producer_commit).toBe("a599246535b73f57e0425cbbfd18c7156e050bba");
  expect(fixture.rustc_version).toContain("1.97.1");
  expect(fixture.sources.map(source => source.path)).toEqual(expect.arrayContaining([
    "apps/windows/src/diagnostics.rs", "apps/windows/src/runtime/media_diagnostics.rs", "apps/windows/src/runtime/cursor_send.rs",
  ]));
  for (const source of fixture.sources) {
    expect(source.git_blob).toMatch(/^[a-f0-9]{40}$/);
    expect(source.sha256).toMatch(/^[a-f0-9]{64}$/);
  }
  expect(fixture.cases.map(value => value.name)).toEqual(["disabled", "enabled", "max_safe", "u64_overflow"]);
  expect(producerEvent("disabled")).toMatchObject({ event: "host_cursor_send", stream_id: 7,
    send_attempts: 2, send_errors: 1, disabled: true, reason: "capability_or_size" });
  expect(Object.keys(producerEvent("disabled")).sort()).toEqual([
    "disabled", "event", "level", "reason", "schema", "send_attempts", "send_errors", "stream_id", "timestamp_unix_ms",
  ]);
});

test("Rust JSONL to signed mixed batch verifies, stores exactly, deduplicates and yields only a cursor degradation finding", async () => {
  const store = new DiagnosticStore(":memory:");
  try {
    const legacy = { schema: 1, timestamp_unix_ms: now, level: "info", event: "host_connected", stream_id: 7 };
    const events = [legacy, producerEvent("disabled"), producerEvent("enabled"), producerEvent("max_safe")];
    const wire = signed(events);
    const verified = verifyBatch(wire.body, wire.headers, now);
    expect(JSON.stringify(verified.batch.events)).toBe(JSON.stringify(events));
    const app = createDiagnosticsApp({ store, now: () => now });
    const response = await app.fetch(wire.request(), "192.0.2.1");
    expect(response.status).toBe(202);
    expect((await response.json()).accepted_events).toBe(4);
    const usage = store.usage();
    expect(await (await app.fetch(wire.request(), "192.0.2.1")).json()).toMatchObject({ duplicate: true, accepted_events: 0 });
    expect(store.usage()).toEqual(usage);
    const rows = readAnalysisRows(store.database, now - 60_000, 100, "a".repeat(32));
    expect(rows).toHaveLength(4);
    expect(rows.map(row => JSON.parse(row.payload_json))).toEqual(expect.arrayContaining(events));
    const session = analyzeDiagnosticRows(rows, 1, now).sessions[0]!;
    expect(session.findings.map(finding => finding.code)).toEqual(["cursor_lane_disabled"]);
    expect(session.video.received_packets).toBe(0);
    expect(session.video.dropped_packets).toBe(0);
    expect(session.video.loss_percent).toBeNull();
    expect(session.stop_reason_kinds).toEqual([]);
    expect(session.outcome).toBe("warning");
  } finally { store.close(); }
});

test("new cursor fields stay event-scoped, typed and bounded; malformed signed mixed batches store nothing", async () => {
  const original = producerEvent("disabled");
  const missing = Object.keys(original).map(key => { const result = { ...original }; delete result[key]; return result; });
  const invalid = [
    ...missing,
    { ...original, raw_error: "synthetic-private-error" }, { ...original, peer_id: "synthetic-private-identity" },
    { ...original, received_video_packets: 100 }, { ...original, event: "host_connected" },
    { ...original, event: "host_cursor_send_v2" }, { ...original, reason: "listener_closed" },
    { ...original, level: "info" }, { ...original, stream_id: 0 }, { ...original, stream_id: "7" },
    { ...original, send_attempts: -1 }, { ...original, send_attempts: 2.5 }, { ...original, send_errors: true },
    { ...original, send_errors: 3 }, { ...original, disabled: "true" }, { ...original, disabled: 1 },
    { ...original, send_attempts: Number.MAX_SAFE_INTEGER + 1 }, producerEvent("u64_overflow"),
  ];
  const store = new DiagnosticStore(":memory:");
  try {
    const app = createDiagnosticsApp({ store, now: () => now });
    for (let index = 0; index < invalid.length; index++) {
      const wire = signed([original, invalid[index]!]);
      const response = await app.fetch(wire.request(), `192.0.2.${index + 1}`);
      expect(response.status).toBe(400);
      expect((await response.json()).error).toBe("invalid_batch");
      expect(store.usage()).toMatchObject({ events: 0, batches: 0, storedBytes: 0 });
    }
    const controller = signed([original], "controller");
    expect((await app.fetch(controller.request(), "198.51.100.1")).status).toBe(400);
    const stderr = signed([{ kind: "cursor_send", stream_id: 7, send_attempts: 2, send_errors: 1, disabled: true, reason: "capability_or_size" }]);
    expect((await app.fetch(stderr.request(), "198.51.100.2")).status).toBe(400);
  } finally { store.close(); }
});

test("changing producer bytes without a matching signature is rejected before storage", async () => {
  const wire = signed([producerEvent("disabled")]);
  const tampered = Buffer.from(wire.body.toString().replace('"send_errors":1', '"send_errors":0'));
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
