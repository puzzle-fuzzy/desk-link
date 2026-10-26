import { expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createAccountApp } from "./app";
import { loadAccountConfig } from "./config";
import { AccountStore } from "./db/store";
import { DevMailer } from "./providers/dev-mailer";
import { AccountService } from "./services/account-service";
import { SessionService } from "./services/session-service";

const config = loadAccountConfig({ NODE_ENV: "test" });
const input = (index: number, email = "race@example.com") => ({
  email, password: "synthetic registration password", deviceId: `synthetic-device-${index}`,
  platform: "windows", deviceName: "Synthetic",
});
const count = (store: AccountStore, table: string) =>
  (store.database.query(`SELECT count(*) AS n FROM ${table}`).get() as { n: number }).n;

test("same-email concurrent registration across connections has exactly one set of effects", async () => {
  const directory = mkdtempSync(join(tmpdir(), "desklink-registration-"));
  const stores = [new AccountStore(join(directory, "account.sqlite")), new AccountStore(join(directory, "account.sqlite"))];
  const mailer = new DevMailer();
  try {
    const services = stores.map(store => new AccountService(store, config, mailer, new SessionService(store, config, () => 100), () => 100));
    const results = await Promise.all(Array.from({ length: 16 }, (_, index) => services[index % 2].register(input(index))));
    expect(results).toEqual(Array.from({ length: 16 }, () => ({ verificationRequired: true })));
    for (const table of ["users", "account_devices", "audit_events", "action_tokens", "email_outbox"]) {
      expect(count(stores[0], table)).toBe(1);
    }
    expect(mailer.messages).toHaveLength(0);
    await services[0].flushOutbox();
    expect(mailer.messages).toHaveLength(1);
    const original = stores[0].findUserByEmail("race@example.com")!;
    await services[1].register({ ...input(100), password: "a different synthetic password" });
    expect(stores[0].findUserById(original.id)?.passwordHash).toBe(original.passwordHash);
    expect(count(stores[0], "account_devices")).toBe(1);
    expect(mailer.messages).toHaveLength(1);
  } finally {
    stores.forEach(store => store.close());
    rmSync(directory, { recursive: true, force: true, maxRetries: 5, retryDelay: 20 });
  }
}, 30_000);

test("concurrent HTTP registrations return the same generic 202 body", async () => {
  const store = new AccountStore();
  try {
    const app = createAccountApp({ store, config, mailer: new DevMailer() });
    const results = await Promise.all(Array.from({ length: 4 }, (_, index) => app.fetch(new Request("http://test/v1/account/register", {
      method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(input(index)),
    }))));
    expect(results.map(result => result.status)).toEqual([202, 202, 202, 202]);
    for (const result of results) expect(await result.json()).toEqual({ verificationRequired: true });
    expect(count(store, "users")).toBe(1);
  } finally { store.close(); }
}, 30_000);

test("registration rolls back all effects when queueing fails", async () => {
  const store = new AccountStore();
  try {
    store.database.exec("CREATE TRIGGER reject_email BEFORE INSERT ON email_outbox BEGIN SELECT RAISE(ABORT, 'synthetic failure'); END");
    const service = new AccountService(store, config, new DevMailer(), new SessionService(store, config, () => 100), () => 100);
    await expect(service.register(input(1))).rejects.toThrow("synthetic failure");
    for (const table of ["users", "account_devices", "audit_events", "action_tokens", "email_outbox"]) expect(count(store, table)).toBe(0);
  } finally { store.close(); }
});

test("a different email can enqueue while the worker waits for mail", async () => {
  const store = new AccountStore();
  let release!: () => void;
  let entered!: () => void;
  const gate = new Promise<void>(resolve => { release = resolve; });
  const started = new Promise<void>(resolve => { entered = resolve; });
  const service = new AccountService(store, config, {
    async send(message) { if (message.to === "slow@example.com") { entered(); await gate; } },
  }, new SessionService(store, config, () => 100), () => 100);
  try {
    await service.register(input(1, "slow@example.com"));
    const first = service.flushOutbox();
    await started;
    expect(await service.register(input(2, "independent@example.com"))).toEqual({ verificationRequired: true });
    expect(count(store, "users")).toBe(2);
    release();
    await first;
  } finally { release(); store.close(); }
});
