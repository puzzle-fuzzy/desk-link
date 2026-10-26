import { expect, test } from "bun:test";
import { Database } from "bun:sqlite";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createAccountApp } from "./app";
import { AccountStore } from "./db/store";

const ready = (store: AccountStore) => createAccountApp({ store }).fetch(new Request("http://test/ready"));

test("closed database is not ready while process health remains ok and no details leak", async () => {
  const store = new AccountStore();
  const app = createAccountApp({ store });
  store.close();
  const response = await app.fetch(new Request("http://test/ready"));
  expect(response.status).toBe(503);
  expect(await response.json()).toEqual({ schema: 1, status: "not_ready", service: "desklink-account",
    dependencies: { database: "unavailable", mail: "degraded" } });
  expect((await app.fetch(new Request("http://test/health"))).status).toBe(200);
});

test("incomplete migration and damaged schema are not ready", async () => {
  const store = new AccountStore();
  try {
    store.database.exec("PRAGMA user_version = 1");
    expect((await ready(store)).status).toBe(503);
    store.database.exec("PRAGMA user_version = 3; DROP TABLE audit_events");
    expect((await ready(store)).status).toBe(503);
  } finally { store.close(); }
});

test("query-only connection cannot pass a read-only SELECT as readiness", async () => {
  const store = new AccountStore();
  try {
    store.database.exec("PRAGMA query_only = ON");
    expect((await ready(store)).status).toBe(503);
    store.database.exec("PRAGMA query_only = OFF");
    expect((await ready(store)).status).toBe(200);
  } finally { store.close(); }
});

test("locked and truly read-only file databases fail with bounded lock waiting", async () => {
  const directory = mkdtempSync(join(tmpdir(), "desklink-readiness-"));
  const path = join(directory, "local.sqlite");
  const store = new AccountStore(path);
  const competitor = new Database(path);
  try {
    competitor.exec("BEGIN IMMEDIATE");
    const started = performance.now();
    expect((await ready(store)).status).toBe(503);
    expect(performance.now() - started).toBeLessThan(250);
    competitor.exec("ROLLBACK");
    expect((await ready(store)).status).toBe(200);
    store.close();
    const readonly = new Database(path, { readonly: true });
    Object.defineProperty(store, "database", { value: readonly });
    expect((await ready(store)).status).toBe(503);
  } finally {
    competitor.close(); store.close(); rmSync(directory, { recursive: true, force: true, maxRetries: 5, retryDelay: 20 });
  }
});

test("mail retry failures are visible as recoverable degradation without failing database readiness", async () => {
  const store = new AccountStore();
  try {
    const user = store.createUser({ email: "synthetic@example.com", passwordHash: "synthetic", now: 1 });
    const token = store.createActionToken({ userId: user.id, purpose: "verify_email", tokenHash: "synthetic", now: 1, expiresAt: 100 });
    const email = store.enqueueEmail({ userId: user.id, recipient: user.email, template: "verify_email", actionTokenId: token.id, now: 1 });
    store.failClaimedEmail(store.claimEmail(1)!, 1, "mail_delivery_failed");
    const response = await ready(store);
    expect(response.status).toBe(200);
    expect((await response.json()).dependencies).toEqual({ database: "ready", mail: "degraded" });
    store.completeClaimedEmail(store.claimEmail(11)!, 11);
    expect((await (await ready(store)).json()).dependencies.mail).toBe("ready");
  } finally { store.close(); }
});

test("healthy probes are rollback-only and low-cost", () => {
  const store = new AccountStore();
  try {
    const started = performance.now();
    for (let index = 0; index < 100; index++) expect(store.readiness().database).toBe("ready");
    console.log(`DL2-037: 100 in-memory readiness probes ${(performance.now() - started).toFixed(2)}ms`);
    expect(store.database.inTransaction).toBe(false);
    expect(store.database.query("SELECT count(*) AS n FROM readiness_probe").get()).toEqual({ n: 1 });
  } finally { store.close(); }
});
