import { createHash, createPublicKey, verify } from "node:crypto";

export const MAX_BATCH_BYTES = 64 * 1_024;
export const SIGNATURE_DOMAIN = Buffer.from("desklink-cloud-diagnostics-v1\0", "utf8");

const ED25519_SPKI_PREFIX = Buffer.from("302a300506032b6570032100", "hex");
const HEX_32 = /^[0-9a-f]{32}$/;
const HEX_64 = /^[0-9a-f]{64}$/;
const HEX_128 = /^[0-9a-f]{128}$/;
const APP_VERSION = /^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$/;
const EVENT_NAME = /^[a-z][a-z0-9_]{1,63}$/;
const LONG_HEX = /(?:^|[^0-9a-fA-F])[0-9a-fA-F]{32,}(?:$|[^0-9a-fA-F])/;
const SECRET_ASSIGNMENT = /DESKLINK_(?:AUTH_KEY|PAIRING_INVITE|SESSION_ID|PEER_VERIFY_KEY|HOST_VERIFY_KEY)\s*=/i;
const ALLOWED_EVENT_FIELDS = new Set([
  "schema",
  "timestamp_unix_ms",
  "level",
  "event",
  "pairing_mode",
  "attempt",
  "stream_id",
  "retry",
  "maximum_retries",
  "delay_ms",
  "reason",
  "received_video_packets",
  "dropped_video_packets",
  "completed_frames",
  "delivered_video_frames",
  "video_ipc_overflow_drops",
  "video_ipc_keyframe_replacements",
  "input_backpressure_count",
  "received_frames",
  "submitted_frames",
  "displayed_frames",
  "malformed_frames",
  "decoder_recoveries",
  "video_pull_failures",
  "first_frame_ms",
]);

export interface DiagnosticEvent {
  schema: 1;
  timestamp_unix_ms: number;
  level: "info" | "warning" | "error";
  event: string;
  [key: string]: string | number | boolean;
}

export interface DiagnosticBatch {
  schema: 1;
  app_version: string;
  platform: "windows";
  source: "host" | "controller";
  installation_id: string;
  correlation_id: string | null;
  events: DiagnosticEvent[];
}

export interface VerifiedBatch {
  batchId: string;
  batch: DiagnosticBatch;
}

export class BatchValidationError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
  }
}

export function verifyBatch(rawBody: Uint8Array, headers: Headers, nowUnixMs = Date.now()): VerifiedBatch {
  if (rawBody.byteLength === 0 || rawBody.byteLength > MAX_BATCH_BYTES) {
    throw new BatchValidationError(413, "diagnostic batch size is invalid");
  }
  const publicKeyHex = normalizedHeader(headers, "x-desklink-public-key", HEX_64);
  const signatureHex = normalizedHeader(headers, "x-desklink-signature", HEX_128);
  const claimedBatchId = normalizedHeader(headers, "x-desklink-batch-id", HEX_64);
  const batchId = createHash("blake2s256").update(rawBody).digest("hex");
  if (batchId !== claimedBatchId) {
    throw new BatchValidationError(400, "diagnostic batch hash does not match");
  }

  const publicKeyBytes = Buffer.from(publicKeyHex, "hex");
  const publicKey = createPublicKey({
    key: Buffer.concat([ED25519_SPKI_PREFIX, publicKeyBytes]),
    format: "der",
    type: "spki",
  });
  const signedPayload = Buffer.concat([SIGNATURE_DOMAIN, Buffer.from(rawBody)]);
  if (!verify(null, signedPayload, publicKey, Buffer.from(signatureHex, "hex"))) {
    throw new BatchValidationError(401, "diagnostic batch signature is invalid");
  }

  let value: unknown;
  try {
    value = JSON.parse(Buffer.from(rawBody).toString("utf8"));
  } catch {
    throw new BatchValidationError(400, "diagnostic batch is not valid JSON");
  }
  const batch = validateBatch(value, publicKeyBytes, nowUnixMs);
  return { batchId, batch };
}

export function installationIdentifier(publicKey: Uint8Array): string {
  return createHash("blake2s256")
    .update("desklink-diagnostic-installation-v1\0")
    .update(publicKey)
    .digest("hex")
    .slice(0, 32);
}

function normalizedHeader(headers: Headers, name: string, pattern: RegExp): string {
  const value = headers.get(name)?.trim().toLowerCase() ?? "";
  if (!pattern.test(value)) {
    throw new BatchValidationError(400, `${name} is invalid`);
  }
  return value;
}

function validateBatch(value: unknown, publicKey: Uint8Array, nowUnixMs: number): DiagnosticBatch {
  if (!isObject(value)) {
    throw new BatchValidationError(400, "diagnostic batch must be an object");
  }
  const keys = Object.keys(value);
  if (keys.some((key) => !["schema", "app_version", "platform", "source", "installation_id", "correlation_id", "events"].includes(key))) {
    throw new BatchValidationError(400, "diagnostic batch contains unsupported fields");
  }
  if (value.schema !== 1 || value.platform !== "windows") {
    throw new BatchValidationError(400, "diagnostic batch schema or platform is unsupported");
  }
  if (typeof value.app_version !== "string" || !APP_VERSION.test(value.app_version)) {
    throw new BatchValidationError(400, "diagnostic app version is invalid");
  }
  if (value.source !== "host" && value.source !== "controller") {
    throw new BatchValidationError(400, "diagnostic source is invalid");
  }
  const expectedInstallation = installationIdentifier(publicKey);
  if (value.installation_id !== expectedInstallation) {
    throw new BatchValidationError(401, "diagnostic installation identity does not match signature");
  }
  if (value.correlation_id !== null && (typeof value.correlation_id !== "string" || !HEX_32.test(value.correlation_id))) {
    throw new BatchValidationError(400, "diagnostic correlation identifier is invalid");
  }
  if (!Array.isArray(value.events) || value.events.length === 0 || value.events.length > 100) {
    throw new BatchValidationError(400, "diagnostic event count is invalid");
  }
  const events = value.events.map((event) => validateEvent(event, nowUnixMs));
  return {
    schema: 1,
    app_version: value.app_version,
    platform: "windows",
    source: value.source,
    installation_id: expectedInstallation,
    correlation_id: value.correlation_id,
    events,
  };
}

function validateEvent(value: unknown, nowUnixMs: number): DiagnosticEvent {
  if (!isObject(value) || Object.keys(value).some((key) => !ALLOWED_EVENT_FIELDS.has(key))) {
    throw new BatchValidationError(400, "diagnostic event contains unsupported fields");
  }
  if (value.schema !== 1 || !Number.isSafeInteger(value.timestamp_unix_ms)) {
    throw new BatchValidationError(400, "diagnostic event schema or timestamp is invalid");
  }
  const timestamp = Number(value.timestamp_unix_ms);
  if (timestamp < nowUnixMs - 31 * 24 * 60 * 60 * 1_000 || timestamp > nowUnixMs + 10 * 60 * 1_000) {
    throw new BatchValidationError(400, "diagnostic event timestamp is outside the accepted window");
  }
  if (value.level !== "info" && value.level !== "warning" && value.level !== "error") {
    throw new BatchValidationError(400, "diagnostic event level is invalid");
  }
  if (typeof value.event !== "string" || !EVENT_NAME.test(value.event)) {
    throw new BatchValidationError(400, "diagnostic event name is invalid");
  }
  for (const [key, field] of Object.entries(value)) {
    if (typeof field === "number" && (!Number.isSafeInteger(field) || field < 0)) {
      throw new BatchValidationError(400, `diagnostic numeric field ${key} is invalid`);
    }
    if (typeof field === "string") {
      if (field.length > (key === "reason" ? 512 : 96) || SECRET_ASSIGNMENT.test(field) || LONG_HEX.test(` ${field} `)) {
        throw new BatchValidationError(400, `diagnostic text field ${key} is unsafe`);
      }
    }
    if (!["string", "number", "boolean"].includes(typeof field)) {
      throw new BatchValidationError(400, `diagnostic field ${key} has an invalid type`);
    }
  }
  return value as DiagnosticEvent;
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
