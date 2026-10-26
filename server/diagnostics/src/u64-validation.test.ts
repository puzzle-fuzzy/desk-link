import { describe, expect, test } from "bun:test";
import { createHash, generateKeyPairSync, sign } from "node:crypto";
import { BatchValidationError, installationIdentifier, SIGNATURE_DOMAIN, verifyBatch } from "./validation";

// Synthetic parsing-boundary tests only. These are not product encoder fixtures,
// hardware evidence, or the producer-owned log/signedBatch compatibility chain.
const now = 1_800_000_000_000;
const MAX_U64 = "18446744073709551615";
const { publicKey, privateKey } = generateKeyPairSync("ed25519");
const rawKey = Buffer.from(publicKey.export({ format: "der", type: "spki" })).subarray(-32);
type Event = Record<string, unknown>;
type Source = "host" | "controller";

// Independent reviewed schema inventory: 18 distinct Rust u64 wire keys.
const fields = [
  ["stream_id", "host_connecting"],
  ["send_attempts", "host_cursor_send"], ["send_errors", "host_cursor_send"],
  ["received_video_packets", "controller_video_metrics"],
  ["dropped_video_packets", "controller_video_metrics"],
  ["completed_frames", "controller_video_metrics"],
  ["delivered_video_frames", "controller_video_metrics"],
  ["video_ipc_overflow_drops", "controller_video_metrics"],
  ["video_ipc_keyframe_replacements", "controller_video_metrics"],
  ["input_backpressure_count", "controller_video_metrics"],
  ["received_frames", "controller_render_metrics"],
  ["submitted_frames", "controller_render_metrics"],
  ["displayed_frames", "controller_render_metrics"],
  ["malformed_frames", "controller_render_metrics"],
  ["coalesced_frame_drops", "controller_render_metrics"],
  ["first_frame_ms", "controller_render_metrics"],
  ["max_frame_gap_ms", "controller_render_metrics"],
  ["profile_probe_ms", "controller_render_metrics"],
] as const;

function event(name: string): Event {
  const common = { schema: 1, timestamp_unix_ms: now, level: "info", event: name, stream_id: 1 };
  if (name === "host_cursor_send") return { ...common, level: "warning", send_attempts: MAX_U64,
    send_errors: "0", disabled: true, reason: "capability_or_size" };
  if (name === "host_direct_listener_stopped") return { ...common, level: "warning", retired_active: true, reason: "listener_closed" };
  if (name === "controller_video_metrics") return { ...common, attempt: 1, video_path: "relay" };
  if (name === "controller_render_metrics") return { ...common, video_width: 1920, video_height: 1080,
    h264_profile: "high", profile_probe: "supported" };
  if (name === "host_reconnecting") return { ...common, retry: 1, maximum_retries: 3, delay_ms: 500, reason: "synthetic" };
  return common;
}

function verifySynthetic(value: Event, source: Source = String(value.event).startsWith("host_") ? "host" : "controller",
  rawNumber?: { field: string; literal: string }) {
  let text = JSON.stringify({ schema: 1, app_version: "0.1.0", platform: "windows", source,
    installation_id: installationIdentifier(rawKey), correlation_id: null, events: [value] });
  if (rawNumber) {
    const original = `${JSON.stringify(rawNumber.field)}:${JSON.stringify(value[rawNumber.field])}`;
    if (!text.includes(original)) throw new Error("synthetic raw numeric replacement did not match");
    text = text.replace(original, `${JSON.stringify(rawNumber.field)}:${rawNumber.literal}`);
  }
  const body = Buffer.from(text, "utf8");
  const headers = new Headers({ "content-type": "application/json",
    "x-desklink-public-key": rawKey.toString("hex"),
    "x-desklink-batch-id": createHash("blake2s256").update(body).digest("hex"),
    "x-desklink-signature": sign(null, Buffer.concat([SIGNATURE_DOMAIN, body]), privateKey).toString("hex") });
  return verifyBatch(body, headers, now).batch.events[0]!;
}
function accepts(value: Event, source?: Source) {
  const parsed = verifySynthetic(value, source);
  expect(parsed as unknown).toEqual(value);
  expect(JSON.stringify(parsed)).toBe(JSON.stringify(value));
  return parsed;
}
function rejects(value: Event, source?: Source, rawNumber?: { field: string; literal: string }) {
  try { verifySynthetic(value, source, rawNumber); }
  catch (error) {
    expect(error).toBeInstanceOf(BatchValidationError);
    expect((error as BatchValidationError).status).toBe(400);
    return;
  }
  throw new Error(`Accepted invalid synthetic event: ${JSON.stringify(value)}`);
}

describe("synthetic signed u64 parsing boundaries", () => {
  test("the independent field inventory contains exactly 18 distinct u64 keys", () => {
    expect(fields).toHaveLength(18);
    expect(new Set(fields.map(([key]) => key)).size).toBe(18);
  });

  for (const [field, name] of fields) {
    test(`${field} preserves canonical decimal strings and legacy safe numbers exactly`, () => {
      const decimals = ["0", "1", "9007199254740990", "9007199254740991", "9007199254740992",
        "9007199254740993", "18446744073709551614", MAX_U64];
      const numbers = [0, 1, Number.MAX_SAFE_INTEGER - 1, Number.MAX_SAFE_INTEGER];
      for (const value of [...decimals, ...numbers]) {
        if (field === "stream_id" && (value === "0" || value === 0)) continue;
        const parsed = accepts({ ...event(name), [field]: value });
        expect(typeof parsed[field]).toBe(typeof value);
        expect(parsed[field]).toBe(value);
      }
    });

    test(`${field} rejects noncanonical, unsafe and wrongly typed signed values`, () => {
      const invalid: unknown[] = [
        "", " ", "\t", "\n", " 1", "1 ", "1\n", "1\r", "1\r\n", "1\u2028", "1\u2029", "\u00a01",
        "+0", "+1", "-0", "-1", "−1",
        "00", "01", "0001", "0.0", "1.0", ".1", "1.", "1e3", "1E3", "1e+3",
        "0x10", "0XFF", "0o7", "0b1", "1_000", "NaN", "Infinity", "-Infinity",
        "１", "١", "१", "²", "0\u200b", "1\u0000",
        "18446744073709551616", "99999999999999999999", "100000000000000000000", "1".repeat(4_096),
        -1, 0.5, Number.MAX_SAFE_INTEGER + 1, Number.MAX_SAFE_INTEGER + 2, 2 ** 64, Number.MAX_VALUE,
        true, false, null, {}, [], [0],
      ];
      for (const value of invalid) rejects({ ...event(name), [field]: value });
      // JSON.stringify(-0) erases its sign, so sign the literal JSON number -0.
      rejects({ ...event(name), [field]: 0 }, undefined, { field, literal: "-0" });
      rejects({ ...event(name), [field]: 0 }, undefined, { field, literal: "1e999" });
    });
  }

  test("stream IDs remain nonzero in every reviewed host/controller u64 event", () => {
    const names = ["host_connecting", "host_available", "host_connected", "host_cursor_send",
      "host_direct_listener_stopped", "controller_video_metrics", "controller_render_metrics"];
    for (const name of names) {
      for (const value of ["1", "9007199254740993", MAX_U64, 1, Number.MAX_SAFE_INTEGER]) accepts({ ...event(name), stream_id: value });
      for (const value of [0, "0"]) rejects({ ...event(name), stream_id: value });
    }
  });

  test("cursor errors cannot exceed attempts even where Number would round both values together", () => {
    const valid = [
      ["9007199254740992", "9007199254740992"], ["9007199254740993", "9007199254740992"],
      ["9007199254740993", "9007199254740993"], [MAX_U64, "18446744073709551614"], [MAX_U64, MAX_U64],
      ["9007199254740992", Number.MAX_SAFE_INTEGER], [Number.MAX_SAFE_INTEGER, "9007199254740991"],
    ];
    for (const [attempts, errors] of valid) accepts({ ...event("host_cursor_send"), send_attempts: attempts, send_errors: errors });
    const invalid = [
      ["9007199254740992", "9007199254740993"], ["9007199254740995", "9007199254740996"],
      ["18446744073709551614", MAX_U64], [Number.MAX_SAFE_INTEGER, "9007199254740992"],
    ];
    for (const [attempts, errors] of invalid) rejects({ ...event("host_cursor_send"), send_attempts: attempts, send_errors: errors });
    expect(Number("9007199254740992")).toBe(Number("9007199254740993"));
    expect(Number("18446744073709551614")).toBe(Number(MAX_U64));
  });

  test("decimal strings do not broaden timestamp, u32, u16, schema or Duration fields", () => {
    const numericOnly = [
      ["timestamp_unix_ms", "controller_video_metrics", now], ["schema", "controller_video_metrics", 1],
      ["attempt", "controller_video_metrics", 1], ["retry", "host_reconnecting", 1],
      ["maximum_retries", "host_reconnecting", 3], ["delay_ms", "host_reconnecting", 500],
      ["video_path_rtt_ms", "controller_video_metrics", 4],
      ["video_path_loss_basis_points", "controller_video_metrics", 12],
      ["video_width", "controller_render_metrics", 1920], ["video_height", "controller_render_metrics", 1080],
      ["decoder_recoveries", "controller_render_metrics", 1], ["video_pull_failures", "controller_render_metrics", 1],
      ["displayed_fps_x100", "controller_render_metrics", 3000],
    ] as const;
    for (const [field, name, value] of numericOnly) {
      accepts({ ...event(name), [field]: value });
      for (const invalid of [String(value), MAX_U64, true, null]) rejects({ ...event(name), [field]: invalid });
    }
  });

  test("u64 strings cannot escape their reviewed event map or introduce unknown keys", () => {
    for (const [field] of fields) {
      rejects({ schema: 1, timestamp_unix_ms: now, level: "info", event: "synthetic_unknown_metric", [field]: "1" });
    }
    rejects({ ...event("controller_video_metrics"), received_frames: "1" });
    rejects({ ...event("controller_render_metrics"), received_video_packets: "1" });
    rejects({ ...event("controller_video_metrics"), future_u64_counter: MAX_U64 });
    rejects({ ...event("controller_render_metrics"), unknown_metric: "0" });
    rejects({ ...event("host_cursor_send"), event: "host_cursor_send_v2" });
    rejects({ ...event("host_direct_listener_stopped"), event: "host_direct_listener_stopped_v2" });
  });

  test("HostMedia source, fixed reason, complete whitelist and typed booleans stay strict", () => {
    for (const name of ["host_cursor_send", "host_direct_listener_stopped"]) {
      const value = { ...event(name), stream_id: MAX_U64 };
      accepts(value, "host");
      rejects(value, "controller");
      for (const reason of ["synthetic_raw_error", "listener_closed_extra", "capability_or_size_extra", 1, true, null]) rejects({ ...value, reason });
      for (const level of ["info", "error"]) rejects({ ...value, level });
      for (const key of ["endpoint", "raw_error", "received_video_packets", "received_frames", "unknown_u64"]) rejects({ ...value, [key]: "1" });
      for (const key of Object.keys(value)) {
        const missing: Event = { ...value }; delete missing[key]; rejects(missing);
      }
      const boolean = name === "host_cursor_send" ? "disabled" : "retired_active";
      for (const flag of [true, false]) accepts({ ...value, [boolean]: flag });
      for (const flag of ["true", "false", "0", 0, 1, null]) rejects({ ...value, [boolean]: flag });
    }
    rejects({ ...event("host_cursor_send"), reason: "listener_closed" });
    rejects({ ...event("host_direct_listener_stopped"), reason: "capability_or_size" });
  });
});
