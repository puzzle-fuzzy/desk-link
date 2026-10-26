import { expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { mkdtempSync, readFileSync, rmdirSync, unlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createDiagnosticsApp } from "./app";
import { analyzeDiagnosticRows } from "./analysis";
import { readAnalysisRows } from "./analysis-store";
import { DiagnosticStore } from "./store";
import { BatchValidationError, MAX_BATCH_BYTES, verifyBatch, type DiagnosticBatch } from "./validation";

interface RustRequest {
  body_utf8: string;
  public_key: string;
  signature: string;
  batch_id: string;
  event_count: number;
}
interface ProducerCase {
  name: string;
  configured_stream_id: string;
  log_lines: string[];
  request: RustRequest;
  legacy_request?: RustRequest;
}
interface ProducerFixture {
  schema: number;
  u64_fields_by_event: Record<string, string[]>;
  provenance: {
    producer_base_commit: string;
    rustc_version: string;
    sources: { path: string; git_blob: string; sha256: string }[];
  };
  cases: ProducerCase[];
  historical_cases: ProducerCase[];
}

// Exact Git blob from producer 28c11ee05a08183f7636ede7946e93029c7bbb16.
// These requests already carry Rust signatures. Never reconstruct or re-sign them.
const fixtureBytes = readFileSync(new URL("../../../apps/windows/tests/fixtures/diagnostic-u64-producer.json", import.meta.url));
const fixture = JSON.parse(fixtureBytes.toString("utf8")) as ProducerFixture;
const allCases = [...fixture.cases, ...fixture.historical_cases];
const now = Math.max(...allCases.flatMap(value => value.log_lines.map(line => Number(JSON.parse(line).timestamp_unix_ms))));
const names = ["safe", "two_pow_53", "two_pow_53_plus_one", "u64_max", "all_u64_max", "zero_counters"];
const expectedStreams = ["9007199254740991", "9007199254740992", "9007199254740993", "18446744073709551615", "7", "7"];

function headers(request: RustRequest): Headers {
  return new Headers({
    "content-type": "application/json", "content-length": String(Buffer.byteLength(request.body_utf8, "utf8")),
    "x-desklink-public-key": request.public_key, "x-desklink-signature": request.signature,
    "x-desklink-batch-id": request.batch_id,
  });
}

test("the sole complete-u64 fixture has the pinned producer bytes and reviewed field scope", () => {
  expect(fixtureBytes.byteLength).toBe(72_875);
  expect(createHash("sha256").update(fixtureBytes).digest("hex")).toBe("8d7d3124436b52906c63905340d7fa7a968017de40a639db37efe18489bb19bf");
  expect(createHash("sha1").update(`blob ${fixtureBytes.byteLength}\0`).update(fixtureBytes).digest("hex"))
    .toBe("58f386235e9dda4c5045b21af9a1a37a53cc39da");
  expect(fixture.schema).toBe(2);
  expect(fixture.provenance.producer_base_commit).toBe("5069158584dfa1f729defe2fadb5b9933acdfcff");
  expect(fixture.provenance.rustc_version).toContain("1.97.1");
  expect(fixture.provenance.sources).toHaveLength(8);
  expect(Object.keys(fixture.u64_fields_by_event).sort()).toEqual([
    "controller_render_metrics", "controller_video_metrics", "host_available", "host_connected", "host_connecting",
    "host_cursor_send", "host_direct_listener_stopped",
  ]);
  expect(new Set(Object.values(fixture.u64_fields_by_event).flat()).size).toBe(18);
  for (const group of [fixture.cases, fixture.historical_cases]) {
    expect(group.map(value => value.name)).toEqual(names);
    expect(group.map(value => value.configured_stream_id)).toEqual(expectedStreams);
    for (const entry of group) {
      const batch = JSON.parse(entry.request.body_utf8) as DiagnosticBatch;
      expect(entry.log_lines).toHaveLength(7);
      expect(batch.events).toHaveLength(entry.request.event_count);
      expect(batch.correlation_id).toBeNull();
      for (let index = 0; index < batch.events.length; index++) {
        const event = batch.events[index]!;
        const rawLog = entry.log_lines[index]!;
        const oldEvent = JSON.parse(rawLog) as Record<string, string | number | boolean>;
        expect(Object.keys(event).sort()).toEqual(Object.keys(oldEvent).sort());
        expect(event.stream_id).toBe(entry.configured_stream_id);
        const fields = fixture.u64_fields_by_event[event.event]!;
        for (const [key, value] of Object.entries(event)) {
          if (fields.includes(key)) {
            expect(typeof value).toBe("string");
            if (entry.legacy_request) {
              // Read old integer digits directly; JSON.parse would round them.
              const rawDigits = new RegExp(`"${key}":([0-9]+)(?=[,}])`).exec(rawLog)?.[1];
              expect(value).toBe(rawDigits!);
            } else expect(value).toBe(oldEvent[key]);
          } else expect(value).toEqual(oldEvent[key]);
        }
      }
    }
  }
});

for (const [kind, cases] of [["current", fixture.cases], ["historical-normalized", fixture.historical_cases]] as const) {
  for (const entry of cases) {
    test(`actual Rust-signed ${kind} ${entry.name} traverses HTTP, persistent SQLite and duplicate receipts`, async () => {
      const directory = mkdtempSync(join(tmpdir(), "desklink-u64-"));
      const path = join(directory, "diagnostics.sqlite");
      let store = new DiagnosticStore(path);
      const app = createDiagnosticsApp({ store, now: () => now });
      const server = Bun.serve({ hostname: "127.0.0.1", port: 0, maxRequestBodySize: MAX_BATCH_BYTES,
        fetch: (request, server) => app.fetch(request, server.requestIP(request)?.address) });
      const post = () => fetch(`http://127.0.0.1:${server.port}/v1/batches`, {
        method: "POST", body: Buffer.from(entry.request.body_utf8, "utf8"), headers: headers(entry.request),
      });
      try {
        const bytes = Buffer.from(entry.request.body_utf8, "utf8");
        const verified = verifyBatch(bytes, headers(entry.request), now);
        expect(verified.batchId).toBe(entry.request.batch_id);
        const response = await post();
        expect(response.status).toBe(202);
        expect(await response.json()).toMatchObject({ accepted: true, duplicate: false, accepted_events: 7 });
        const usage = store.usage();
        expect(usage).toMatchObject({ batches: 1, events: 7 });
        expect(await (await post()).json()).toMatchObject({ duplicate: true, accepted_events: 0 });
        expect(store.usage()).toEqual(usage);
        const rows = readAnalysisRows(store.database, 0, 100);
        expect(rows.map(row => JSON.parse(row.payload_json))).toEqual(verified.batch.events);
        expect(rows.every(row => row.correlation_id === null)).toBe(true);
        expect(store.database.query("SELECT remote_address FROM batches").all()).toEqual([{ remote_address: "127.0.0.1" }]);
        const report = analyzeDiagnosticRows(rows, 1, now);
        expect(report).toMatchObject({ total_rows: 7, uncorrelated_rows: 7, sessions: [] });
        expect(() => JSON.stringify(report)).not.toThrow();
        expect((await fetch(`http://127.0.0.1:${server.port}/health`)).status).toBe(200);
        store.close();
        store = new DiagnosticStore(path);
        expect(readAnalysisRows(store.database, 0, 100)).toEqual(rows);
        expect(store.usage()).toEqual(usage);
      } finally {
        await server.stop(true);
        store.close();
        // Remove only the explicitly created test files; no recursive deletion.
        for (const file of [path, `${path}-wal`, `${path}-shm`]) {
          try { unlinkSync(file); } catch (error) { if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error; }
        }
        rmdirSync(directory);
      }
    });
  }
}

test("unmodified historical Rust signatures accept safe numbers and reject unsafe numeric batches atomically", async () => {
  const store = new DiagnosticStore(":memory:");
  const app = createDiagnosticsApp({ store, now: () => now });
  const server = Bun.serve({ hostname: "127.0.0.1", port: 0, maxRequestBodySize: MAX_BATCH_BYTES,
    fetch: (request, server) => app.fetch(request, server.requestIP(request)?.address) });
  try {
    for (const entry of fixture.historical_cases) {
      const request = entry.legacy_request!;
      const safe = ["safe", "zero_counters"].includes(entry.name);
      const before = store.usage();
      const response = await fetch(`http://127.0.0.1:${server.port}/v1/batches`, {
        method: "POST", body: Buffer.from(request.body_utf8, "utf8"), headers: headers(request),
      });
      expect(response.status).toBe(safe ? 202 : 400);
      if (safe) {
        expect((await response.json()).accepted_events).toBe(7);
      } else {
        expect((await response.json()).error).toBe("invalid_batch");
        expect(store.usage()).toEqual(before);
        try {
          verifyBatch(Buffer.from(request.body_utf8, "utf8"), headers(request), now);
          throw new Error("unsafe historical numeric batch was accepted");
        } catch (error) {
          expect(error).toBeInstanceOf(BatchValidationError);
          expect((error as BatchValidationError).status).toBe(400);
        }
      }
    }
  } finally { await server.stop(true); store.close(); }
});

test("changed complete-u64 body bytes with a fresh hash still fail the original Rust signature", async () => {
  const original = fixture.cases.find(value => value.name === "two_pow_53_plus_one")!.request;
  const body = original.body_utf8.replaceAll('"9007199254740993"', '"9007199254740992"');
  expect(body).not.toBe(original.body_utf8);
  const request = { ...original, body_utf8: body, batch_id: createHash("blake2s256").update(body).digest("hex") };
  const store = new DiagnosticStore(":memory:");
  try {
    const app = createDiagnosticsApp({ store, now: () => now });
    const response = await app.fetch(new Request("http://test/v1/batches", {
      method: "POST", body: Buffer.from(body, "utf8"), headers: headers(request),
    }), "192.0.2.1");
    expect(response.status).toBe(401);
    expect(store.usage()).toMatchObject({ batches: 0, events: 0 });
  } finally { store.close(); }
});

test("analysis of actual stored producer values preserves adjacent IDs and aggregates beyond one u64", async () => {
  const store = new DiagnosticStore(":memory:");
  try {
    const app = createDiagnosticsApp({ store, now: () => now });
    for (const name of ["two_pow_53", "two_pow_53_plus_one", "all_u64_max"]) {
      const request = fixture.cases.find(value => value.name === name)!.request;
      const response = await app.fetch(new Request("http://test/v1/batches", {
        method: "POST", body: Buffer.from(request.body_utf8, "utf8"), headers: headers(request),
      }), "192.0.2.1");
      expect(response.status).toBe(202);
    }
    const rows = readAnalysisRows(store.database, 0, 100);
    expect(rows).toHaveLength(21);
    expect(rows.every(row => row.correlation_id === null)).toBe(true);
    // Deliberately synthetic analysis group only; do not alter the signed
    // requests, persisted privacy metadata, or claim a real correlated session.
    const report = analyzeDiagnosticRows(rows.map(row => ({ ...row, correlation_id: "f".repeat(32) })), 1, now);
    const session = report.sessions[0]!;
    expect(session.render.received_frames).toBe("18446744073709551621");
    expect(session.render.displayed_frames).toBe("18446744073709551621");
    expect(session.render.first_frame_ms).toBe("18446744073709551615");
    expect(session.video.received_packets).toBe("18446744073709551615");
    expect(session.video.dropped_packets).toBe("18446744073709551615");
    expect(session.video.loss_percent).toBe(50);
    expect(session.findings.filter(finding => finding.code === "cursor_lane_disabled")).toHaveLength(1);
    expect(session.findings.filter(finding => finding.code === "direct_listener_stopped")).toHaveLength(1);
    expect(session.stop_reason_kinds).toEqual([]);
    expect(JSON.parse(JSON.stringify(report))).toEqual(report);
    expect(readAnalysisRows(store.database, 0, 100)).toEqual(rows);
  } finally { store.close(); }
});
