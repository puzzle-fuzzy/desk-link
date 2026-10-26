import { expect, test } from "bun:test";
import { Database } from "bun:sqlite";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createAccountApp } from "./app";
import { AccountStore } from "./db/store";
import type { EmailOutboxRecord } from "./db/types";
import { AccountWorker } from "./worker";

const register = () => new Request("http://test/v1/account/register", {
  method: "POST", headers: { "content-type": "application/json" },
  body: JSON.stringify({ email: "synthetic@example.com", password: "synthetic password only", deviceId: "synthetic-device", platform: "windows" }),
});
function seed(store: AccountStore, now = 100) {
  const user = store.createUser({ email: "synthetic@example.com", passwordHash: "synthetic", now });
  const token = store.createActionToken({ userId: user.id, purpose: "verify_email", tokenHash: "synthetic", now, expiresAt: now + 1 });
  return store.enqueueEmail({ userId: user.id, recipient: user.email, template: "verify_email", actionTokenId: token.id, now });
}
function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>(done => { resolve = done; });
  return { promise, resolve };
}

test("slow delivery does not block register, resend or forgot and shutdown waits for the current send only", async () => {
  const store = new AccountStore();
  const entered = deferred();
  const gate = deferred();
  let sends = 0;
  const app = createAccountApp({ store, mailer: { async send() { sends++; entered.resolve(); await gate.promise; } } });
  expect((await app.fetch(register())).status).toBe(202);
  expect(sends).toBe(0);
  const flushing = app.flushOutbox();
  await entered.promise;
  for (const path of ["verify-email/resend", "password/forgot"]) {
    expect((await app.fetch(new Request(`http://test/v1/account/${path}`, {
      method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ email: "synthetic@example.com" }),
    }))).status).toBe(202);
  }
  expect(store.database.query("SELECT count(*) AS n FROM email_outbox").get()).toEqual({ n: 3 });
  let closed = false;
  const closing = app.close().then(() => { closed = true; });
  await Promise.resolve();
  expect(closed).toBe(false);
  expect(store.database.query("SELECT 1 AS n").get()).toEqual({ n: 1 });
  gate.resolve();
  await flushing;
  await closing;
  expect(sends).toBe(1);
  expect(await app.flushOutbox()).toBe(0);
  expect(() => store.database.query("SELECT 1").get()).toThrow();
});

test("persistent queue survives restart and concurrent worker connections never claim the same live lease", async () => {
  const directory = mkdtempSync(join(tmpdir(), "desklink-outbox-"));
  const path = join(directory, "local.sqlite");
  let store = new AccountStore(path);
  seed(store);
  store.close();
  store = new AccountStore(path);
  const second = new AccountStore(path);
  const entered = deferred();
  const gate = deferred();
  let sends = 0;
  const mailer = { async send() { sends++; entered.resolve(); await gate.promise; } };
  const firstApp = createAccountApp({ store, mailer, now: () => 100 });
  const secondApp = createAccountApp({ store: second, mailer, now: () => 100 });
  try {
    const flushing = firstApp.flushOutbox();
    await entered.promise;
    expect(await secondApp.flushOutbox()).toBe(0);
    expect(sends).toBe(1);
    gate.resolve();
    expect(await flushing).toBe(1);
  } finally {
    gate.resolve();
    await firstApp.close(); await secondApp.close();
    rmSync(directory, { recursive: true, force: true, maxRetries: 5, retryDelay: 20 });
  }
});

test("expired lease is recovered and stale acknowledgements cannot overwrite its new owner", () => {
  const store = new AccountStore();
  try {
    const email = seed(store);
    const old = store.claimEmail(100)!;
    expect(store.claimEmail(129)).toBeUndefined();
    expect(store.completeClaimedEmail(old, 130)).toBe(false);
    const replacement = store.claimEmail(130)!;
    expect(replacement.id).toBe(email.id);
    expect(replacement.leaseToken).not.toBe(old.leaseToken);
    expect(replacement.attempts).toBe(2);
    expect(store.completeClaimedEmail(old, 130)).toBe(false);
    expect(store.failClaimedEmail(old, 130, "mail_delivery_failed")).toBe(false);
    expect(store.prepareClaimedEmailToken(old, "stale", 130)).toBe(false);
    expect(store.completeClaimedEmail(replacement, 130)).toBe(true);
    expect(() => store.claimEmail(130, 0)).toThrow();
  } finally { store.close(); }
});

test("ten failures become terminal and only explicit audited requeue allows another attempt", async () => {
  const store = new AccountStore();
  const email = seed(store);
  let now = 100;
  let sends = 0;
  const app = createAccountApp({ store, now: () => now, mailer: { async send() { sends++; throw new Error("synthetic-secret-do-not-persist"); } } });
  try {
    for (let attempt = 1; attempt <= 10; attempt++) {
      expect(await app.flushOutbox()).toBe(0);
      const record = store.findEmailOutboxById(email.id)!;
      expect(record.attempts).toBe(attempt);
      expect(record.status).toBe(attempt < 10 ? "pending" : "failed");
      expect(record.lastError).toBe("mail_delivery_failed");
      now = record.availableAt;
    }
    expect(sends).toBe(10);
    expect(await app.flushOutbox()).toBe(0);
    expect(store.requeueFailedEmail(email.id, now)).toBe(true);
    expect(store.requeueFailedEmail(email.id, now)).toBe(false);
    expect(store.database.query("SELECT count(*) AS n FROM audit_events WHERE action = 'account.email_requeued'").get()).toEqual({ n: 1 });
    await app.flushOutbox();
    expect(sends).toBe(11);
    expect(store.findEmailOutboxById(email.id)?.attempts).toBe(1);
  } finally { await app.close(); }
});

test("the tenth in-flight crash becomes failed after its lease expires", () => {
  const store = new AccountStore();
  try {
    const email = seed(store);
    let record: EmailOutboxRecord | undefined;
    for (let index = 0; index < 10; index++) record = store.claimEmail(100 + index * 30);
    expect(record?.attempts).toBe(10);
    expect(store.claimEmail(400)).toBeUndefined();
    expect(store.findEmailOutboxById(email.id)?.status).toBe("failed");
    expect(store.findEmailOutboxById(email.id)?.lastError).toBe("delivery_lease_expired");
  } finally { store.close(); }
});

test("legacy file migrates retry and sent records once without replaying ALTER TABLE on restart", () => {
  const directory = mkdtempSync(join(tmpdir(), "desklink-migration-"));
  const path = join(directory, "local.sqlite");
  const legacy = new Database(path, { create: true });
  legacy.exec(readFileSync(new URL("./db/migrations/001_account.sql", import.meta.url), "utf8"));
  legacy.exec(`INSERT INTO users VALUES ('u','synthetic@example.com','synthetic',NULL,'active',1,1,1);
    INSERT INTO action_tokens VALUES ('t','u','verify_email','synthetic',1,2,NULL);
    INSERT INTO email_outbox VALUES ('failed','u','synthetic@example.com','verify_email','t',1,2,10,NULL,'old raw error');
    INSERT INTO email_outbox VALUES ('sent','u','synthetic@example.com','verify_email','t',1,2,1,3,NULL);`);
  legacy.close();
  try {
    for (let index = 0; index < 2; index++) {
      const store = new AccountStore(path);
      expect(store.readiness().database).toBe("ready");
      expect(store.findEmailOutboxById("failed")?.status).toBe("failed");
      expect(store.findEmailOutboxById("failed")?.lastError).toBe("mail_delivery_failed");
      expect(store.findEmailOutboxById("sent")?.status).toBe("delivered");
      store.close();
    }
  } finally { rmSync(directory, { recursive: true, force: true, maxRetries: 5, retryDelay: 20 }); }
});

test("stopping worker cancels scheduled timer and never runs a new claim", async () => {
  const store = new AccountStore();
  seed(store);
  let sends = 0;
  const app = createAccountApp({ store, mailer: { async send() { sends++; } } });
  const worker = new AccountWorker(app);
  worker.start();
  await worker.stop();
  await Bun.sleep(20);
  expect(sends).toBe(0);
  worker.start();
  expect(await app.flushOutbox()).toBe(0);
});
