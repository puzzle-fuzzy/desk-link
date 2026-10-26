import { clientIp, DiagnosticAdmission } from "./admission";
import { DiagnosticStorageFull } from "./storage-quota";
import type { DiagnosticStore } from "./store";
import { BatchValidationError, MAX_BATCH_BYTES, verifyBatch } from "./validation";

export function createDiagnosticsApp(options: {
  store: DiagnosticStore;
  trustedProxies?: ReadonlySet<string>;
  admission?: DiagnosticAdmission;
  now?: () => number;
  bodyTimeoutMs?: number;
  retention?: () => unknown;
}) {
  const admission = options.admission ?? new DiagnosticAdmission();
  const trusted = options.trustedProxies ?? new Set<string>();
  const now = options.now ?? Date.now;
  const bodyTimeoutMs = options.bodyTimeoutMs ?? 5_000;
  if (!Number.isSafeInteger(bodyTimeoutMs) || bodyTimeoutMs <= 0) throw new RangeError("invalid diagnostic body timeout");
  return {
    async fetch(request: Request, socketIp: string | undefined): Promise<Response> {
      const url = new URL(request.url);
      if (request.method === "GET" && url.pathname === "/health") {
        return json({ schema: 1, status: "ok", service: "desklink-diagnostics",
          retention: options.retention?.(), admission: admission.snapshot(), storage: options.store.usage() });
      }
      if (request.method !== "POST" || url.pathname !== "/v1/batches") return json({ error: "not_found" }, 404);
      const ip = clientIp(socketIp, request.headers, trusted);
      if (!ip) return json({ error: "client_address" }, 400);
      // Admission deliberately precedes reading any body or doing signature work.
      const lease = admission.begin(ip, now());
      if (!lease) return limited();
      try {
        if (request.headers.get("content-type")?.split(";", 1)[0].trim() !== "application/json") return json({ error: "content_type" }, 415);
        const lengthText = request.headers.get("content-length") ?? "";
        const declaredLength = /^\d+$/.test(lengthText) ? Number(lengthText) : 0;
        if (!Number.isSafeInteger(declaredLength) || declaredLength <= 0 || declaredLength > MAX_BATCH_BYTES) return json({ error: "batch_size" }, 413);
        const rawBody = await readBody(request, declaredLength, bodyTimeoutMs);
        lease.completeBody(rawBody.byteLength);
        const verified = verifyBatch(rawBody, request.headers, now());
        if (!lease.acceptEvents(verified.batch.installation_id, rawBody.byteLength, verified.batch.events.length)) return limited();
        const stored = options.store.insert(verified.batchId, verified.batch, ip, Math.floor(now() / 1_000));
        return json({ schema: 1, accepted: true, duplicate: stored.duplicate,
          accepted_events: stored.acceptedEvents, retention_days: 14 }, 202);
      } catch (error) {
        if (error instanceof BatchValidationError) return json({ error: "invalid_batch", detail: error.message }, error.status);
        if (error instanceof DiagnosticStorageFull) return json({ error: "storage_quota" }, 503, { "retry-after": "60" });
        if (typeof error === "object" && error !== null && "code" in error && error.code === "SQLITE_BUSY") {
          return json({ error: "storage_busy" }, 503, { "retry-after": "1" });
        }
        console.error("diagnostic ingestion failed");
        return json({ error: "internal" }, 500);
      } finally { lease.release(); }
    },
  };
}

async function readBody(request: Request, length: number, timeoutMs: number): Promise<Uint8Array> {
  if (!request.body) throw new BatchValidationError(413, "diagnostic batch size is invalid");
  const reader = request.body.getReader();
  let timer: ReturnType<typeof setTimeout> | undefined;
  const timeout = new Promise<never>((_, reject) => {
    timer = setTimeout(() => reject(new BatchValidationError(408, "diagnostic body timeout")), timeoutMs);
  });
  try {
    const bytes = new Uint8Array(length);
    let received = 0;
    for (;;) {
      const chunk = await Promise.race([reader.read(), timeout]);
      if (chunk.done) break;
      received += chunk.value.byteLength;
      if (received > length) throw new BatchValidationError(413, "diagnostic batch size is invalid");
      bytes.set(chunk.value, received - chunk.value.byteLength);
    }
    if (received !== length) throw new BatchValidationError(413, "diagnostic batch size is invalid");
    return bytes;
  } catch (error) {
    void reader.cancel().catch(() => undefined);
    throw error;
  } finally {
    clearTimeout(timer);
    reader.releaseLock();
  }
}
function limited() { return json({ error: "rate_limited" }, 429, { "retry-after": "60" }); }
function json(value: unknown, status = 200, headers: Record<string, string> = {}): Response {
  return Response.json(value, { status, headers: { "cache-control": "no-store", "x-content-type-options": "nosniff", ...headers } });
}
