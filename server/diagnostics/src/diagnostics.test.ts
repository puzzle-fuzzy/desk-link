import { afterEach, describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createHash, generateKeyPairSync, sign } from "node:crypto";
import { Database } from "bun:sqlite";

import { DiagnosticStore, RETENTION_SECONDS } from "./store";
import { DiagnosticMaintenance } from "./maintenance";
import {
  BatchValidationError,
  SIGNATURE_DOMAIN,
  installationIdentifier,
  verifyBatch,
} from "./validation";

const directories: string[] = [];

afterEach(() => {
  while (directories.length > 0) {
    rmSync(directories.pop()!, { recursive: true, force: true });
  }
});

describe("signed diagnostic ingestion", () => {
  test("accepts a bounded signed batch and stores every event once", () => {
    const fixture = signedFixture();
    const verified = verifyBatch(fixture.body, fixture.headers, fixture.now);
    expect(verified.batch.installation_id).toBe(fixture.installationId);

    const directory = mkdtempSync(join(tmpdir(), "desklink-diagnostics-"));
    directories.push(directory);
    const store = new DiagnosticStore(join(directory, "events.sqlite"));
    const first = store.insert(verified.batchId, verified.batch, "127.0.0.1", Math.floor(fixture.now / 1_000));
    const second = store.insert(verified.batchId, verified.batch, "127.0.0.1", Math.floor(fixture.now / 1_000));
    expect(first).toEqual({ duplicate: false, acceptedEvents: 1 });
    expect(second).toEqual({ duplicate: true, acceptedEvents: 0 });
    expect((store.database.query("SELECT COUNT(*) AS count FROM events").get() as { count: number }).count).toBe(1);
    store.close();
  });

  test("rolls back a partially written batch so every event can be retried", () => {
    const fixture = signedFixture({
      events: [
        eventFixture({ event: "controller_connected" }),
        eventFixture({ event: "controller_stopped" }),
      ],
    });
    const verified = verifyBatch(fixture.body, fixture.headers, fixture.now);
    const store = new DiagnosticStore(":memory:");
    try {
      store.database.exec(`
        CREATE TRIGGER synthetic_event_failure BEFORE INSERT ON events
        WHEN NEW.event = 'controller_stopped'
        BEGIN SELECT RAISE(ABORT, 'synthetic write failure'); END;
      `);
      const insert = () => store.insert(
        verified.batchId,
        verified.batch,
        "127.0.0.1",
        Math.floor(fixture.now / 1_000),
      );

      expect(insert).toThrow("synthetic write failure");
      for (const table of ["batches", "events"]) {
        const row = store.database.query(`SELECT COUNT(*) AS count FROM ${table}`).get() as { count: number };
        expect(row.count).toBe(0);
      }

      store.database.exec("DROP TRIGGER synthetic_event_failure");
      expect(insert()).toEqual({ duplicate: false, acceptedEvents: 2 });
      expect(insert()).toEqual({ duplicate: true, acceptedEvents: 0 });
      const row = store.database.query("SELECT COUNT(*) AS count FROM events").get() as { count: number };
      expect(row.count).toBe(2);
    } finally {
      store.close();
    }
  });

  test("retention uses indexes, stays bounded and preserves the exact 14 day boundary", () => {
    const now = 2_000_000_000;
    const cutoff = now - RETENTION_SECONDS;
    const store = new DiagnosticStore(":memory:");
    try {
      const fixture = signedFixture();
      const { batch } = verifyBatch(fixture.body, fixture.headers, fixture.now);
      // Unique events are needed because deduplication intentionally spans batches.
      for (let i = 0; i < 8; i++) {
        store.insert(`expired-${i}`, { ...batch, events: [{ ...batch.events[0]!, timestamp_unix_ms: i }] }, "synthetic", cutoff - 1);
      }
      store.insert("boundary", { ...batch, events: [{ ...batch.events[0]!, timestamp_unix_ms: 10 }] }, "synthetic", cutoff);
      store.insert("current", { ...batch, events: [{ ...batch.events[0]!, timestamp_unix_ms: 11 }] }, "synthetic", now);
      for (const table of ["events", "batches"]) {
        const plan = store.database.query(`EXPLAIN QUERY PLAN SELECT rowid FROM ${table}
          WHERE received_at_unix_s < ? ORDER BY received_at_unix_s LIMIT ?`).all(cutoff, 3);
        expect(JSON.stringify(plan)).toContain(`${table}_by_received_at`);
        expect(JSON.stringify(plan)).not.toContain(`SCAN ${table}`);
      }
      const assertNoOrphans = () => expect(store.database.query(`SELECT COUNT(*) AS count FROM events e
        LEFT JOIN batches b ON b.batch_id = e.batch_id WHERE b.batch_id IS NULL`).get()).toEqual({ count: 0 });
      expect(store.purge(cutoff, 3)).toEqual({ events: 3, batches: 0, more: true });
      assertNoOrphans();
      expect(store.purge(cutoff, 3)).toEqual({ events: 3, batches: 0, more: true });
      assertNoOrphans();
      expect(store.purge(cutoff, 3)).toEqual({ events: 2, batches: 3, more: true });
      assertNoOrphans();
      expect(store.purge(cutoff, 3)).toEqual({ events: 0, batches: 3, more: true });
      expect(store.purge(cutoff, 3)).toEqual({ events: 0, batches: 2, more: false });
      expect(store.database.query("SELECT batch_id FROM batches ORDER BY batch_id").all()).toEqual([
        { batch_id: "boundary" }, { batch_id: "current" },
      ]);
      expect(store.database.query("SELECT COUNT(*) AS count FROM events").get()).toEqual({ count: 2 });
      expect(() => store.purge(cutoff, 0)).toThrow(RangeError);
    } finally { store.close(); }
  });

  test("ingestion does not delete history or depend on maintenance succeeding", () => {
    const store = new DiagnosticStore(":memory:");
    const maintenance = new DiagnosticMaintenance(store, () => 2_000_000_000);
    try {
      const fixture = signedFixture();
      const { batch } = verifyBatch(fixture.body, fixture.headers, fixture.now);
      store.insert("expired", batch, "synthetic", 1);
      store.database.exec(`CREATE TRIGGER synthetic_purge_failure BEFORE DELETE ON events
        BEGIN SELECT RAISE(ABORT, 'synthetic purge failure'); END;`);
      expect(store.insert("new", { ...batch, events: [{ ...batch.events[0]!, timestamp_unix_ms: 1 }] }, "synthetic", 2_000_000_000))
        .toEqual({ duplicate: false, acceptedEvents: 1 });
      expect(maintenance.runOnce()).toBeUndefined();
      expect(maintenance.snapshot()).toMatchObject({ state: "degraded", failures: 1 });
      expect(JSON.stringify(maintenance.snapshot())).not.toContain("synthetic purge failure");
      expect(store.database.query("SELECT COUNT(*) AS count FROM batches").get()).toEqual({ count: 2 });
      store.database.exec("DROP TRIGGER synthetic_purge_failure");
      expect(maintenance.runOnce()).toEqual({ events: 1, batches: 1, more: false });
      expect(maintenance.snapshot()).toMatchObject({ state: "ok", failures: 1 });
      maintenance.start();
      maintenance.stop();
      expect(maintenance.runOnce()).toBeUndefined();
    } finally { maintenance.stop(); store.close(); }
  });

  test("file maintenance recovers from another writer and releases metric/checkpoint reads", () => {
    const directory = mkdtempSync(join(tmpdir(), "desklink-retention-"));
    directories.push(directory);
    const path = join(directory, "events.sqlite");
    let store = new DiagnosticStore(path);
    // Exercise the startup upgrade path from an existing schema without indexes.
    store.database.exec("DROP INDEX events_by_received_at; DROP INDEX batches_by_received_at");
    store.close();
    store = new DiagnosticStore(path);
    const writer = new Database(path);
    const maintenance = new DiagnosticMaintenance(store);
    try {
      expect(store.database.query("SELECT name FROM sqlite_master WHERE type = 'index' AND name LIKE '%_by_received_at'").all())
        .toHaveLength(2);
      writer.exec("BEGIN IMMEDIATE");
      expect(maintenance.runOnce()).toBeUndefined();
      expect(maintenance.snapshot()).toMatchObject({ state: "degraded", failures: 1 });
      writer.exec("ROLLBACK");
      expect(maintenance.runOnce()).toEqual({ events: 0, batches: 0, more: false });
      expect(maintenance.runOnce()).toEqual({ events: 0, batches: 0, more: false });
      expect(maintenance.snapshot()).toMatchObject({ state: "ok", failures: 1 });
      expect(maintenance.snapshot().databaseBytes).toBeGreaterThan(0);
    } finally { maintenance.stop(); writer.close(); store.close(); }
  });

  test("rejects tampering, identities that do not match and secret-shaped text", () => {
    const fixture = signedFixture();
    const tampered = Uint8Array.from(fixture.body);
    tampered[tampered.length - 2] ^= 1;
    expect(() => verifyBatch(tampered, fixture.headers, fixture.now)).toThrow(BatchValidationError);

    const mismatched = signedFixture({ installation_id: "0".repeat(32) });
    expect(() => verifyBatch(mismatched.body, mismatched.headers, mismatched.now)).toThrow(
      "diagnostic installation identity does not match signature",
    );

    const secret = signedFixture({
      events: [eventFixture({ reason: `DESKLINK_SESSION_ID=${"a".repeat(32)}` })],
    });
    expect(() => verifyBatch(secret.body, secret.headers, secret.now)).toThrow("diagnostic text field reason is unsafe");
  });

  test("accepts bounded renderer and input queue metrics", () => {
    const now = Date.now();
    const fixture = signedFixture({
      events: [
        eventFixture({
          event: "controller_render_metrics",
          level: "info",
          stream_id: 4,
          received_frames: 90,
          submitted_frames: 88,
          displayed_frames: 86,
          malformed_frames: 1,
          decoder_recoveries: 1,
          video_pull_failures: 2,
          first_frame_ms: 620,
          video_width: 2560,
          video_height: 1440,
          video_path: "relay",
          displayed_fps_x100: 3000,
          max_frame_gap_ms: 42,
          coalesced_frame_drops: 3,
          h264_profile: "high",
          profile_probe: "supported",
          profile_probe_ms: 18,
          profile_fallback_reason: "decoderStall",
        }, now),
        eventFixture({
          event: "controller_video_metrics",
          level: "info",
          video_path: "directLan",
          video_path_rtt_ms: 4,
          video_path_loss_basis_points: 12,
          received_video_packets: 220,
          dropped_video_packets: 2,
          completed_frames: 90,
          delivered_video_frames: 88,
          video_ipc_overflow_drops: 2,
          video_ipc_keyframe_replacements: 1,
          input_backpressure_count: 0,
        }, now + 1),
      ],
    });
    const verified = verifyBatch(fixture.body, fixture.headers, fixture.now);
    expect(verified.batch.events[0]).toMatchObject({
      event: "controller_render_metrics",
      displayed_frames: 86,
      video_pull_failures: 2,
      first_frame_ms: 620,
    });
    expect(verified.batch.events[1]).toMatchObject({
      event: "controller_video_metrics",
      delivered_video_frames: 88,
      video_path: "directLan",
      video_path_rtt_ms: 4,
      video_path_loss_basis_points: 12,
      video_ipc_overflow_drops: 2,
      video_ipc_keyframe_replacements: 1,
      input_backpressure_count: 0,
    });
  });
});

function signedFixture(overrides: Record<string, unknown> = {}) {
  const now = Date.now();
  const { publicKey, privateKey } = generateKeyPairSync("ed25519");
  const spki = publicKey.export({ format: "der", type: "spki" });
  const rawPublicKey = spki.subarray(spki.length - 32);
  const installationId = installationIdentifier(rawPublicKey);
  const batch = {
    schema: 1,
    app_version: "0.1.24",
    platform: "windows",
    source: "controller",
    installation_id: installationId,
    correlation_id: "1".repeat(32),
    events: [eventFixture({}, now)],
    ...overrides,
  };
  const body = Buffer.from(JSON.stringify(batch), "utf8");
  const signature = sign(null, Buffer.concat([SIGNATURE_DOMAIN, body]), privateKey);
  const headers = new Headers({
    "content-type": "application/json",
    "x-desklink-public-key": rawPublicKey.toString("hex"),
    "x-desklink-signature": signature.toString("hex"),
    "x-desklink-batch-id": createHash("blake2s256").update(body).digest("hex"),
  });
  return { now, body, headers, installationId };
}

function eventFixture(overrides: Record<string, unknown> = {}, now = Date.now()) {
  return {
    schema: 1,
    timestamp_unix_ms: now,
    level: "warning",
    event: "controller_retry_scheduled",
    attempt: 2,
    retry: 1,
    delay_ms: 500,
    reason: "中继连接暂时中断",
    ...overrides,
  };
}
