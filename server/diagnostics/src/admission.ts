import { isIP } from "node:net";
import { MAX_BATCH_BYTES } from "./validation";

export interface BudgetLimits { requests: number; bytes: number; events: number }
export interface AdmissionOptions {
  windowMs: number;
  maximumIpKeys: number;
  maximumInstallationKeys: number;
  global: BudgetLimits;
  ip: BudgetLimits;
  installation: BudgetLimits;
  maximumInFlight: number;
  maximumIpInFlight: number;
}
export const DEFAULT_ADMISSION: AdmissionOptions = {
  windowMs: 60_000, maximumIpKeys: 2_048, maximumInstallationKeys: 4_096,
  global: { requests: 120, bytes: 4 * 1_024 * 1_024, events: 6_000 },
  ip: { requests: 12, bytes: 768 * 1_024, events: 1_200 },
  installation: { requests: 12, bytes: 768 * 1_024, events: 1_200 },
  maximumInFlight: 16, maximumIpInFlight: 4,
};
interface Bucket { epoch: number; requests: number; bytes: number; events: number; inFlight: number }
const bucket = (epoch: number): Bucket => ({ epoch, requests: 0, bytes: 0, events: 0, inFlight: 0 });

class BoundedBuckets {
  private readonly entries = new Map<string, Bucket>();
  constructor(private readonly maximum: number) {}
  get(key: string, epoch: number): Bucket | undefined {
    let entry = this.entries.get(key);
    if (entry) {
      if (entry.epoch !== epoch) {
        entry.epoch = epoch; entry.requests = 0; entry.bytes = 0; entry.events = 0;
        this.entries.delete(key); this.entries.set(key, entry);
      }
      return entry;
    }
    if (this.entries.size >= this.maximum) {
      // One O(1) candidate, never a scan. Fail closed when full: rotating keys
      // must not evict a live bucket and reset an existing identity's budget.
      const oldest = this.entries.entries().next().value;
      if (!oldest || oldest[1].epoch === epoch || oldest[1].inFlight > 0) return undefined;
      this.entries.delete(oldest[0]);
    }
    entry = bucket(epoch); this.entries.set(key, entry);
    return entry;
  }
  get size() { return this.entries.size; }
}

export interface AdmissionLease {
  completeBody(actualBytes: number): void;
  acceptEvents(installation: string, actualBytes: number, events: number): boolean;
  release(): void;
}

export class DiagnosticAdmission {
  private global = bucket(-1);
  private readonly ips: BoundedBuckets;
  private readonly installations: BoundedBuckets;
  private lastNow = 0;
  constructor(readonly options: AdmissionOptions = DEFAULT_ADMISSION) {
    for (const value of [options.windowMs, options.maximumIpKeys, options.maximumInstallationKeys,
      options.maximumInFlight, options.maximumIpInFlight, ...Object.values(options.global),
      ...Object.values(options.ip), ...Object.values(options.installation)]) {
      if (!Number.isSafeInteger(value) || value <= 0) throw new RangeError("invalid diagnostic admission limit");
    }
    this.ips = new BoundedBuckets(options.maximumIpKeys);
    this.installations = new BoundedBuckets(options.maximumInstallationKeys);
  }
  private epoch(now: number) {
    this.lastNow = Math.max(this.lastNow, now);
    return Math.floor(this.lastNow / this.options.windowMs);
  }
  begin(ip: string, now: number): AdmissionLease | undefined {
    const epoch = this.epoch(now);
    if (this.global.epoch !== epoch) Object.assign(this.global, { epoch, requests: 0, bytes: 0, events: 0 });
    const global = this.global;
    if (global.inFlight >= this.options.maximumInFlight || !fits(global, this.options.global, 1, MAX_BATCH_BYTES, 0)) return undefined;
    const local = this.ips.get(ip, epoch);
    if (!local || local.inFlight >= this.options.maximumIpInFlight || !fits(local, this.options.ip, 1, MAX_BATCH_BYTES, 0)) return undefined;
    for (const target of [global, local]) { target.requests++; target.bytes += MAX_BATCH_BYTES; target.inFlight++; }
    let completed = false;
    let released = false;
    return {
      completeBody: (actualBytes) => {
        if (completed || released) return;
        completed = true;
        // Reserve the entire allowed body before reading. Refund unused bytes
        // after a bounded read, including invalid signatures; no header trust.
        const refund = MAX_BATCH_BYTES - Math.min(MAX_BATCH_BYTES, Math.max(0, actualBytes));
        // A body spanning a window must not refund credits from a newer window.
        if (global.epoch === epoch) global.bytes -= refund;
        if (local.epoch === epoch) local.bytes -= refund;
      },
      acceptEvents: (installation, actualBytes, events) => {
        if (released || !completed) return false;
        // Count against the admission window. A lease never migrates counters.
        if (global.epoch !== epoch || local.epoch !== epoch) return false;
        const identity = this.installations.get(installation, epoch);
        if (!identity || !fits(global, this.options.global, 0, 0, events)
          || !fits(local, this.options.ip, 0, 0, events)
          || !fits(identity, this.options.installation, 1, actualBytes, events)) return false;
        global.events += events; local.events += events;
        identity.requests++; identity.bytes += actualBytes; identity.events += events;
        return true;
      },
      release: () => {
        if (released) return;
        released = true; global.inFlight--; local.inFlight--;
        // Interrupted bodies retain their full reservation until the window ends.
      },
    };
  }
  snapshot() { return { ipKeys: this.ips.size, installationKeys: this.installations.size, inFlight: this.global.inFlight }; }
}
function fits(value: Bucket, limit: BudgetLimits, requests: number, bytes: number, events: number) {
  return value.requests + requests <= limit.requests && value.bytes + bytes <= limit.bytes && value.events + events <= limit.events;
}

export function normalizeIp(value: string): string | undefined {
  if (value.includes("%") || !isIP(value)) return undefined;
  if (isIP(value) === 4) return value;
  const normalized = new URL(`http://[${value}]/`).hostname.slice(1, -1);
  const mapped = /^::ffff:([a-f0-9]{1,4}):([a-f0-9]{1,4})$/.exec(normalized);
  if (!mapped) return normalized;
  const high = parseInt(mapped[1]!, 16), low = parseInt(mapped[2]!, 16);
  return `${high >> 8}.${high & 255}.${low >> 8}.${low & 255}`;
}
export function trustedProxySet(value = ""): ReadonlySet<string> {
  const values = value.split(",").map((part) => part.trim()).filter(Boolean);
  return new Set(values.map((part) => {
    const ip = normalizeIp(part);
    if (!ip) throw new Error("invalid diagnostic trusted proxy address");
    return ip;
  }));
}
export function clientIp(socketIp: string | undefined, headers: Headers, trusted: ReadonlySet<string>): string | undefined {
  const socket = socketIp ? normalizeIp(socketIp) : undefined;
  if (!socket) return undefined;
  if (!trusted.has(socket)) return socket;
  const real = headers.get("x-real-ip");
  return real ? normalizeIp(real.trim()) : undefined;
}
