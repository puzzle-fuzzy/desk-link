import { describe, expect, test } from "bun:test";

import {
  DEFAULT_ADMISSION,
  DiagnosticAdmission,
  clientIp,
  normalizeIp,
  trustedProxySet,
  type AdmissionOptions,
} from "./admission";
import { MAX_BATCH_BYTES } from "./validation";

function options(overrides: Partial<AdmissionOptions> = {}): AdmissionOptions {
  const generous = { requests: 100_000, bytes: MAX_BATCH_BYTES * 100_000, events: 100_000 };
  return {
    ...DEFAULT_ADMISSION,
    windowMs: 1_000,
    maximumIpKeys: 16,
    maximumInstallationKeys: 16,
    maximumInFlight: 100,
    maximumIpInFlight: 100,
    global: { ...generous },
    ip: { ...generous },
    installation: { ...generous },
    ...overrides,
  };
}

describe("diagnostic admission budgets", () => {
  test("rotating signing identities cannot reset a socket IP request budget", () => {
    const admission = new DiagnosticAdmission(options({
      ip: { requests: 2, bytes: MAX_BATCH_BYTES * 10, events: 100 },
    }));
    for (const identity of ["new-key-a", "new-key-b"]) {
      const lease = admission.begin("192.0.2.1", 0)!;
      lease.completeBody(100);
      expect(lease.acceptEvents(identity, 100, 1)).toBe(true);
      lease.release();
    }
    expect(admission.begin("192.0.2.1", 0)).toBeUndefined();
    expect(admission.snapshot()).toEqual({ ipKeys: 1, installationKeys: 2, inFlight: 0 });
  });

  test("global request budget combines different IPs before body processing", () => {
    const admission = new DiagnosticAdmission(options({
      global: { requests: 2, bytes: MAX_BATCH_BYTES * 10, events: 100 },
    }));
    for (const ip of ["192.0.2.1", "192.0.2.2"]) {
      const lease = admission.begin(ip, 0)!;
      lease.completeBody(100);
      lease.release();
    }
    expect(admission.begin("192.0.2.3", 0)).toBeUndefined();
    expect(admission.snapshot().ipKeys).toBe(2);
  });

  test("full maximum-body reservations enforce global and IP byte budgets", () => {
    const local = new DiagnosticAdmission(options({
      ip: { requests: 100, bytes: MAX_BATCH_BYTES, events: 100 },
    }));
    const interrupted = local.begin("192.0.2.1", 0)!;
    interrupted.release();
    // An interrupted body retains its reservation; release is not a refund.
    expect(local.begin("192.0.2.1", 0)).toBeUndefined();
    const otherIp = local.begin("192.0.2.2", 0)!;
    expect(otherIp).toBeDefined();
    otherIp.release();

    const global = new DiagnosticAdmission(options({
      global: { requests: 100, bytes: MAX_BATCH_BYTES * 2, events: 100 },
    }));
    const first = global.begin("192.0.2.1", 0)!;
    const second = global.begin("192.0.2.2", 0)!;
    expect(global.begin("192.0.2.3", 0)).toBeUndefined();
    first.completeBody(8);
    first.completeBody(0); // Cannot refund the same body twice.
    expect(global.begin("192.0.2.3", 0)).toBeUndefined();
    second.completeBody(8);
    const third = global.begin("192.0.2.3", 0)!;
    expect(third).toBeDefined();
    first.release(); second.release(); third.release();
  });

  test("global and per-IP in-flight limits release exactly once", () => {
    const admission = new DiagnosticAdmission(options({ maximumInFlight: 2, maximumIpInFlight: 1 }));
    const first = admission.begin("192.0.2.1", 0)!;
    expect(admission.begin("192.0.2.1", 0)).toBeUndefined();
    const second = admission.begin("192.0.2.2", 0)!;
    expect(admission.begin("192.0.2.3", 0)).toBeUndefined();
    first.release(); first.release();
    expect(admission.snapshot().inFlight).toBe(1);
    const third = admission.begin("192.0.2.3", 0)!;
    expect(third).toBeDefined();
    second.release(); third.release();
    expect(admission.snapshot().inFlight).toBe(0);
  });

  test("event budgets combine global, IP and installation usage", () => {
    const admission = new DiagnosticAdmission(options({
      global: { requests: 100, bytes: MAX_BATCH_BYTES * 100, events: 3 },
      ip: { requests: 100, bytes: MAX_BATCH_BYTES * 100, events: 2 },
      installation: { requests: 100, bytes: MAX_BATCH_BYTES * 100, events: 2 },
    }));
    const accept = (ip: string, identity: string, events: number) => {
      const lease = admission.begin(ip, 0)!;
      lease.completeBody(100);
      const result = lease.acceptEvents(identity, 100, events);
      lease.release();
      return result;
    };
    expect(accept("192.0.2.1", "a", 2)).toBe(true);
    expect(accept("192.0.2.1", "b", 1)).toBe(false); // IP exhausted.
    expect(accept("192.0.2.2", "a", 1)).toBe(false); // Same installation, new IP.
    expect(accept("192.0.2.2", "b", 1)).toBe(true);
    expect(accept("192.0.2.3", "c", 1)).toBe(false); // Global exhausted.
  });

  test("installation request and actual-byte budgets survive IP changes", () => {
    const admission = new DiagnosticAdmission(options({
      installation: { requests: 1, bytes: 10, events: 100 },
    }));
    const first = admission.begin("192.0.2.1", 0)!;
    first.completeBody(10);
    expect(first.acceptEvents("same-key", 10, 1)).toBe(true);
    first.release();
    const second = admission.begin("192.0.2.2", 0)!;
    second.completeBody(1);
    expect(second.acceptEvents("same-key", 1, 1)).toBe(false);
    expect(second.acceptEvents("different-key", 11, 1)).toBe(false);
    second.release();
  });

  test("IP key floods fail closed without evicting live budgets", () => {
    const admission = new DiagnosticAdmission(options({
      maximumIpKeys: 2,
      ip: { requests: 1, bytes: MAX_BATCH_BYTES, events: 100 },
    }));
    for (const ip of ["192.0.2.1", "192.0.2.2"]) admission.begin(ip, 0)!.release();
    let denied = 0;
    for (let i = 0; i < 10_000; i++) if (!admission.begin(`untrusted-key-${i}`, 0)) denied++;
    expect(denied).toBe(10_000);
    expect(admission.snapshot().ipKeys).toBe(2);
    expect(admission.begin("192.0.2.1", 0)).toBeUndefined();
    const replacement = admission.begin("192.0.2.3", 1_000)!;
    expect(replacement).toBeDefined();
    replacement.release();
    expect(admission.snapshot().ipKeys).toBe(2);
  });

  test("installation key floods stay bounded and reclaim expired entries", () => {
    const admission = new DiagnosticAdmission(options({
      maximumInstallationKeys: 2,
      installation: { requests: 1, bytes: MAX_BATCH_BYTES, events: 100 },
    }));
    const accept = (identity: string, now: number) => {
      const lease = admission.begin("192.0.2.1", now)!;
      lease.completeBody(1);
      const result = lease.acceptEvents(identity, 1, 1);
      lease.release();
      return result;
    };
    expect(accept("key-a", 0)).toBe(true);
    expect(accept("key-b", 0)).toBe(true);
    let denied = 0;
    for (let i = 0; i < 10_000; i++) if (!accept(`new-key-${i}`, 0)) denied++;
    expect(denied).toBe(10_000);
    expect(admission.snapshot().installationKeys).toBe(2);
    expect(accept("key-a", 0)).toBe(false);
    expect(accept("new-epoch-key", 1_000)).toBe(true);
    expect(admission.snapshot().installationKeys).toBe(2);
  });

  test("an old lease cannot refund or accept events in a later window", () => {
    const admission = new DiagnosticAdmission(options({
      global: { requests: 100, bytes: MAX_BATCH_BYTES, events: 100 },
      ip: { requests: 100, bytes: MAX_BATCH_BYTES, events: 100 },
    }));
    const old = admission.begin("192.0.2.1", 0)!;
    const current = admission.begin("192.0.2.1", 1_000)!;
    expect(admission.snapshot().inFlight).toBe(2);
    old.completeBody(0);
    expect(old.acceptEvents("old", 1, 1)).toBe(false);
    old.release();
    expect(admission.snapshot().inFlight).toBe(1);
    expect(admission.begin("192.0.2.1", 1_000)).toBeUndefined();
    current.completeBody(0);
    const afterRefund = admission.begin("192.0.2.1", 1_000)!;
    expect(afterRefund).toBeDefined();
    current.release(); afterRefund.release();
    expect(admission.snapshot().inFlight).toBe(0);
  });

  test("expired in-flight keys stay pinned and backward clocks cannot reset usage", () => {
    const admission = new DiagnosticAdmission(options({ maximumIpKeys: 1, maximumIpInFlight: 1 }));
    const old = admission.begin("192.0.2.1", 0)!;
    expect(admission.begin("192.0.2.2", 1_000)).toBeUndefined();
    expect(admission.begin("192.0.2.1", 1_000)).toBeUndefined();
    old.release();
    const newWindow = admission.begin("192.0.2.1", 1_000)!;
    expect(newWindow).toBeDefined();
    newWindow.release();

    const monotonic = new DiagnosticAdmission(options({
      global: { requests: 1, bytes: MAX_BATCH_BYTES * 10, events: 100 },
    }));
    monotonic.begin("192.0.2.1", 1_000)!.release();
    expect(monotonic.begin("192.0.2.2", 0)).toBeUndefined();
  });
});

describe("diagnostic trusted client IP", () => {
  test("direct connections ignore all forwarded address headers", () => {
    const headers = new Headers({ "x-real-ip": "198.51.100.1", "x-forwarded-for": "203.0.113.1, 203.0.113.2" });
    expect(clientIp("192.0.2.1", headers, trustedProxySet())).toBe("192.0.2.1");
    expect(clientIp(undefined, headers, trustedProxySet())).toBeUndefined();
    expect(clientIp("not-a-socket-ip", headers, trustedProxySet())).toBeUndefined();
  });

  test("only configured proxies may supply a single valid X-Real-IP", () => {
    const trusted = trustedProxySet("127.0.0.1, ::1");
    expect(clientIp("::ffff:127.0.0.1", new Headers({ "x-real-ip": " 198.51.100.2 ", "x-forwarded-for": "203.0.113.99" }), trusted))
      .toBe("198.51.100.2");
    expect(clientIp("::1", new Headers({ "x-real-ip": "2001:0DB8:0000::1" }), trusted)).toBe("2001:db8::1");
    for (const real of ["", "198.51.100.1, 198.51.100.2", "198.51.100.1:3411", "garbage"]) {
      expect(clientIp("127.0.0.1", new Headers({ "x-real-ip": real, "x-forwarded-for": "198.51.100.3" }), trusted))
        .toBeUndefined();
    }
    expect(clientIp("127.0.0.1", new Headers({ "x-forwarded-for": "198.51.100.3" }), trusted)).toBeUndefined();
    expect(() => trustedProxySet("127.0.0.1, 0.0.0.0/0")).toThrow("invalid diagnostic trusted proxy address");
  });

  test("canonical IP forms share keys and scoped IPv6 cannot throw on request input", () => {
    expect(normalizeIp("2001:0DB8:0000:0000:0000:0000:0000:0001")).toBe("2001:db8::1");
    expect(normalizeIp("::ffff:192.0.2.1")).toBe("192.0.2.1");
    expect(normalizeIp("0:0:0:0:0:ffff:c000:201")).toBe("192.0.2.1");
    expect(normalizeIp("192.0.2.1")).toBe("192.0.2.1");
    expect(normalizeIp("fe80::1%eth0")).toBeUndefined();
  });
});
