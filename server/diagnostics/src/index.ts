import { mkdirSync } from "node:fs";
import { dirname } from "node:path";

import { DiagnosticStore } from "./store";
import { BatchValidationError, MAX_BATCH_BYTES, verifyBatch } from "./validation";

class SlidingWindowLimiter {
  private readonly requests = new Map<string, number[]>();

  constructor(
    private readonly maximum: number,
    private readonly windowMs: number,
  ) {}

  accept(key: string, now = Date.now()): boolean {
    const cutoff = now - this.windowMs;
    const recent = (this.requests.get(key) ?? []).filter((timestamp) => timestamp >= cutoff);
    if (recent.length >= this.maximum) {
      this.requests.set(key, recent);
      return false;
    }
    recent.push(now);
    this.requests.set(key, recent);
    if (this.requests.size > 10_000) {
      for (const [candidate, timestamps] of this.requests) {
        if (timestamps.every((timestamp) => timestamp < cutoff)) {
          this.requests.delete(candidate);
        }
      }
    }
    return true;
  }
}

const address = process.env.DESKLINK_DIAGNOSTICS_ADDR ?? "127.0.0.1";
const port = parsePort(process.env.DESKLINK_DIAGNOSTICS_PORT ?? "3411");
const databasePath = process.env.DESKLINK_DIAGNOSTICS_DATABASE ?? "/var/lib/desklink-diagnostics/diagnostics.sqlite";
mkdirSync(dirname(databasePath), { recursive: true });
const store = new DiagnosticStore(databasePath);
const limiter = new SlidingWindowLimiter(12, 60_000);

const server = Bun.serve({
  hostname: address,
  port,
  maxRequestBodySize: MAX_BATCH_BYTES,
  async fetch(request, serverInstance) {
    const url = new URL(request.url);
    if (request.method === "GET" && url.pathname === "/health") {
      return json({ schema: 1, status: "ok", service: "desklink-diagnostics" });
    }
    if (request.method !== "POST" || url.pathname !== "/v1/batches") {
      return json({ error: "not_found" }, 404);
    }
    if (request.headers.get("content-type")?.split(";", 1)[0].trim() !== "application/json") {
      return json({ error: "content_type" }, 415);
    }
    const declaredLength = Number(request.headers.get("content-length") ?? "0");
    if (!Number.isSafeInteger(declaredLength) || declaredLength <= 0 || declaredLength > MAX_BATCH_BYTES) {
      return json({ error: "batch_size" }, 413);
    }
    try {
      const rawBody = new Uint8Array(await request.arrayBuffer());
      const verified = verifyBatch(rawBody, request.headers);
      if (!limiter.accept(verified.batch.installation_id)) {
        return json({ error: "rate_limited" }, 429, { "retry-after": "60" });
      }
      const remoteAddress = serverInstance.requestIP(request)?.address ?? "unknown";
      const stored = store.insert(verified.batchId, verified.batch, remoteAddress);
      return json({
        schema: 1,
        accepted: true,
        duplicate: stored.duplicate,
        accepted_events: stored.acceptedEvents,
        retention_days: 14,
      }, 202);
    } catch (error) {
      if (error instanceof BatchValidationError) {
        return json({ error: "invalid_batch", detail: error.message }, error.status);
      }
      console.error("diagnostic ingestion failed", error);
      return json({ error: "internal" }, 500);
    }
  },
});

console.log(`DeskLink diagnostics listening on http://${server.hostname}:${server.port}`);

function parsePort(value: string): number {
  const parsed = Number(value);
  if (!Number.isInteger(parsed) || parsed < 1 || parsed > 65_535) {
    throw new Error("DESKLINK_DIAGNOSTICS_PORT is invalid");
  }
  return parsed;
}

function json(value: unknown, status = 200, headers: Record<string, string> = {}): Response {
  return Response.json(value, {
    status,
    headers: {
      "cache-control": "no-store",
      "x-content-type-options": "nosniff",
      ...headers,
    },
  });
}
