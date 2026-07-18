import { createHash, generateKeyPairSync, sign } from "node:crypto";

import { SIGNATURE_DOMAIN, installationIdentifier } from "./validation";

const endpoint = process.env.DESKLINK_DIAGNOSTICS_ENDPOINT
  ?? "https://p2p.yxswy.com/desklink-diagnostics/v1/batches";
const now = Date.now();
const { publicKey, privateKey } = generateKeyPairSync("ed25519");
const spki = publicKey.export({ format: "der", type: "spki" });
const rawPublicKey = spki.subarray(spki.length - 32);
const batch = {
  schema: 1,
  app_version: "0.0.0",
  platform: "windows",
  source: "controller",
  installation_id: installationIdentifier(rawPublicKey),
  correlation_id: null,
  events: [{
    schema: 1,
    timestamp_unix_ms: now,
    level: "info",
    event: "managed_diagnostics_probe",
    attempt: 1,
  }],
};
const body = Buffer.from(JSON.stringify(batch), "utf8");
const signature = sign(null, Buffer.concat([SIGNATURE_DOMAIN, body]), privateKey);
const response = await fetch(endpoint, {
  method: "POST",
  headers: {
    "content-type": "application/json",
    "x-desklink-public-key": rawPublicKey.toString("hex"),
    "x-desklink-signature": signature.toString("hex"),
    "x-desklink-batch-id": createHash("blake2s256").update(body).digest("hex"),
  },
  body,
});
const result = await response.json() as Record<string, unknown>;
if (response.status !== 202 || result.accepted !== true) {
  throw new Error(`managed diagnostics probe failed with HTTP ${response.status}: ${JSON.stringify(result)}`);
}
console.log(`DeskLink managed diagnostics signed ingestion passed: ${endpoint}`);
