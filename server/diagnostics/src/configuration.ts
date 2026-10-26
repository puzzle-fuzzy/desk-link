import { DEFAULT_ADMISSION, DiagnosticAdmission, trustedProxySet } from "./admission";
import { DEFAULT_STORAGE_LIMITS } from "./storage-quota";

export function diagnosticConfiguration(environment: Record<string, string | undefined>) {
  const integer = (name: string, fallback: number) => {
    const text = environment[`DESKLINK_DIAGNOSTICS_${name}`];
    if (text === undefined) return fallback;
    const value = /^[1-9]\d*$/.test(text) ? Number(text) : NaN;
    if (!Number.isSafeInteger(value) || value <= 0) throw new Error(`invalid diagnostic setting ${name}`);
    return value;
  };
  return {
    trustedProxies: trustedProxySet(environment.DESKLINK_DIAGNOSTICS_TRUSTED_PROXIES),
    admission: new DiagnosticAdmission({ ...DEFAULT_ADMISSION, global: {
      requests: integer("GLOBAL_REQUESTS_PER_MINUTE", DEFAULT_ADMISSION.global.requests),
      bytes: integer("GLOBAL_BYTES_PER_MINUTE", DEFAULT_ADMISSION.global.bytes),
      events: integer("GLOBAL_EVENTS_PER_MINUTE", DEFAULT_ADMISSION.global.events),
    } }),
    storageLimits: {
      events: integer("MAX_STORED_EVENTS", DEFAULT_STORAGE_LIMITS.events),
      batches: integer("MAX_STORED_BATCHES", DEFAULT_STORAGE_LIMITS.batches),
      bytes: integer("MAX_STORED_BYTES", DEFAULT_STORAGE_LIMITS.bytes),
    },
  };
}
