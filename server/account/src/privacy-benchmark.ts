// Local synthetic service timing only; no socket, provider or deployed database.
import { AccountStore } from "./db/store";
import { loadAccountConfig } from "./config";
import { DUMMY_PASSWORD_HASH } from "./domain/password";
import { AccountService } from "./services/account-service";
import { SessionService } from "./services/session-service";

const store = new AccountStore();
const config = loadAccountConfig({ NODE_ENV: "test" });
store.createUser({ email: "known@example.com", passwordHash: DUMMY_PASSWORD_HASH, now: 1 });
const service = new AccountService(store, config, { async send() { throw new Error("provider must not run"); } }, new SessionService(store, config, () => 100), () => 100);
const samples = 100;
const input = (email: string, index: number) => ({ email, password: "synthetic-wrong-password", deviceId: `synthetic-device-${index}`, platform: "windows", deviceName: "Synthetic" });
const metrics = (values: number[]) => {
  const ordered = [...values].sort((a, b) => a - b);
  return { p50: ordered[Math.ceil(ordered.length / 2) - 1], p95: ordered[Math.ceil(ordered.length * .95) - 1], min: ordered[0], max: ordered.at(-1) };
};
const output: unknown[] = [];
try {
  for (const route of ["login", "register", "forgot"] as const) {
    const known: number[] = [], absent: number[] = [];
    for (let sample = -10; sample < samples; sample++) {
      for (const present of sample % 2 === 0 ? [true, false] : [false, true]) {
        const email = present ? "known@example.com" : `absent-${route}-${sample}@example.com`;
        const started = performance.now();
        if (route === "login") { try { await service.login(input(email, sample)); } catch (error) { if ((error as { status?: number }).status !== 401) throw error; } }
        else if (route === "register") await service.register(input(email, sample));
        else await service.requestPasswordReset(email);
        if (sample >= 0) (present ? known : absent).push(performance.now() - started);
      }
    }
    const auc = known.flatMap(left => absent.map(right => left > right ? 1 : left === right ? .5 : 0)).reduce<number>((a, b) => a + b, 0) / (samples * samples);
    output.push({ route, samplesPerBranch: samples, knownMs: metrics(known), absentMs: metrics(absent), knownGreaterAuc: auc,
      orientationIndependentAuc: Math.max(auc, 1 - auc) });
  }
  console.log(JSON.stringify({ date: new Date().toISOString(), bun: Bun.version, platform: process.platform,
    note: "Synthetic in-process service measurements; excludes HTTP/network and production load. Finite distributions cannot prove timing indistinguishability.", output }, null, 2));
} finally { store.close(); }
