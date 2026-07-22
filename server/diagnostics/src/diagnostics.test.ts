import { afterEach, describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createHash, generateKeyPairSync, sign } from "node:crypto";

import { DiagnosticStore } from "./store";
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
