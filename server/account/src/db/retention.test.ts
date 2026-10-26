import { afterEach, expect, test } from "bun:test";
import { randomUUID } from "node:crypto";

import { loadAccountConfig } from "../config";
import { hashOpaqueToken } from "../domain/tokens";
import { SessionService } from "../services/session-service";
import { cleanupAccountHistory } from "./retention";
import { AccountStore } from "./store";

const DAY = 86_400;
const NOW = 200 * DAY;
const stores: AccountStore[] = [];
afterEach(() => { for (const store of stores.splice(0)) store.close(); });

function fixture() {
  const store = new AccountStore();
  stores.push(store);
  const user = store.createUser({ email: `synthetic-${randomUUID()}@example.com`, passwordHash: "synthetic-hash", now: 1 });
  const device = store.upsertDevice({ id: `synthetic-${randomUUID()}`, userId: user.id, platform: "windows", name: "Synthetic", now: 1 });
  return { store, user, device };
}

function token(store: AccountStore, userId: string, expiresAt: number) {
  return store.createActionToken({ userId, purpose: "verify_email", tokenHash: randomUUID(), now: 1, expiresAt });
}

function outbox(store: AccountStore, status: string, finishedAt: number | null) {
  // Separate users keep this fixture compatible with pending-per-purpose dedup.
  const user = store.createUser({ email: `mail-${randomUUID()}@example.com`, passwordHash: "synthetic-hash", now: 1 });
  const actionToken = token(store, user.id, 1);
  const id = randomUUID();
  store.database.query(`INSERT INTO email_outbox (
    id, user_id, recipient, template, action_token_id, created_at,
    available_at, attempts, sent_at, last_error, status, finished_at, lease_until
  ) VALUES (?, ?, ?, 'verify_email', ?, 1, 1, 1, ?, NULL, ?, ?, ?)`)
    .run(id, user.id, user.email, actionToken.id, status === "delivered" ? finishedAt : null, status, finishedAt, status === "delivering" ? NOW + 60 : null);
  return { id, actionToken };
}

test("collects only terminal mail older than 30 days and preserves pending and leased token references", () => {
  const { store } = fixture();
  const expiredDelivered = outbox(store, "delivered", NOW - 30 * DAY - 1);
  const expiredFailed = outbox(store, "failed", NOW - 30 * DAY - 1);
  const boundary = outbox(store, "failed", NOW - 30 * DAY);
  const pending = outbox(store, "pending", null);
  const leased = outbox(store, "delivering", null);

  const result = cleanupAccountHistory(store.database, NOW);
  expect(result.email_outbox).toBe(2);
  expect(result.action_tokens).toBe(2);
  for (const record of [expiredDelivered, expiredFailed]) {
    expect(store.findEmailOutboxById(record.id)).toBeUndefined();
    expect(store.findActionTokenById(record.actionToken.id)).toBeUndefined();
  }
  for (const record of [boundary, pending, leased]) {
    expect(store.findEmailOutboxById(record.id)).toBeDefined();
    expect(store.findActionTokenById(record.actionToken.id)).toBeDefined();
  }
});

test("keeps valid and exact-boundary action tokens, even when a consumed token is still within retention", () => {
  const { store, user } = fixture();
  const obsolete = token(store, user.id, NOW - 7 * DAY - 1);
  const boundary = token(store, user.id, NOW - 7 * DAY);
  const valid = token(store, user.id, NOW + DAY);
  const consumed = token(store, user.id, NOW - 1);
  store.invalidateActionToken(consumed.id, NOW - DAY);

  expect(cleanupAccountHistory(store.database, NOW).action_tokens).toBe(1);
  expect(store.findActionTokenById(obsolete.id)).toBeUndefined();
  for (const record of [boundary, valid, consumed]) expect(store.findActionTokenById(record.id)).toBeDefined();
});

test("preserves the entire family through its latest expiry plus 30 days, across bounded cleanup passes", () => {
  const { store, user, device } = fixture();
  function session(familyId: string, expiresAt: number) {
    return store.createSession({ userId: user.id, deviceId: device.id, familyId,
      accessTokenHash: randomUUID(), accessExpiresAt: 2, refreshTokenHash: randomUUID(), now: 1, expiresAt });
  }
  const history = session("protected-family", 2);
  const boundary = session("protected-family", NOW - 30 * DAY);
  const obsolete = [session("expired-family", 2), session("expired-family", 3)];

  expect(cleanupAccountHistory(store.database, NOW, 1).account_sessions).toBe(1);
  expect(cleanupAccountHistory(store.database, NOW, 1).account_sessions).toBe(1);
  expect(cleanupAccountHistory(store.database, NOW, 1).account_sessions).toBe(0);
  expect(store.findSessionById(history.id)).toBeDefined();
  expect(store.findSessionById(boundary.id)).toBeDefined();
  for (const record of obsolete) expect(store.findSessionById(record.id)).toBeUndefined();
  expect(cleanupAccountHistory(store.database, NOW + 1, 1).account_sessions).toBe(1);
  expect(cleanupAccountHistory(store.database, NOW + 1, 1).account_sessions).toBe(1);
});

test("cleanup retains an old rotated refresh token so its replay still revokes a live descendant", () => {
  const { store, user, device } = fixture();
  const config = loadAccountConfig({ NODE_ENV: "test" });
  let now = 1;
  const sessions = new SessionService(store, config, () => now);
  let current = sessions.create(user, device);
  const originalRefresh = current.refreshToken;
  for (let rotation = 0; rotation < 4; rotation++) {
    now += 20 * DAY;
    current = sessions.refresh(current.refreshToken);
  }
  expect(now).toBeGreaterThan(60 * DAY);
  expect(cleanupAccountHistory(store.database, now).account_sessions).toBe(0);
  expect(store.findSessionByRefreshHash(hashOpaqueToken(originalRefresh))).toBeDefined();
  expect(sessions.authenticate(new Request("http://synthetic.test/me", {
    headers: { authorization: `Bearer ${current.accessToken}` },
  })).user.id).toBe(user.id);
  expect(() => sessions.refresh(originalRefresh)).toThrow("重复使用");
  expect(() => sessions.authenticate(new Request("http://synthetic.test/me", {
    headers: { authorization: `Bearer ${current.accessToken}` },
  }))).toThrow();
});

test("limits every table separately and applies the exact 90-day audit boundary", () => {
  const { store, user, device } = fixture();
  for (let index = 0; index < 5; index++) {
    outbox(store, "failed", 1);
    token(store, user.id, 1);
    store.createSession({ userId: user.id, deviceId: device.id, familyId: "obsolete-family",
      accessTokenHash: randomUUID(), accessExpiresAt: 2, refreshTokenHash: randomUUID(), now: 1, expiresAt: 2 });
    store.recordAuditEvent({ userId: user.id, action: "synthetic.old", now: NOW - 90 * DAY - 1 });
  }
  store.recordAuditEvent({ userId: user.id, action: "synthetic.boundary", now: NOW - 90 * DAY });
  const first = cleanupAccountHistory(store.database, NOW, 2);
  expect(first).toEqual({ email_outbox: 2, action_tokens: 2, account_sessions: 2, audit_events: 2 });
  const second = cleanupAccountHistory(store.database, NOW, 2);
  expect(second).toEqual({ email_outbox: 2, action_tokens: 2, account_sessions: 2, audit_events: 2 });
  expect(cleanupAccountHistory(store.database, NOW, 2).audit_events).toBe(1);
  expect(store.database.query("SELECT action FROM audit_events").all()).toEqual([{ action: "synthetic.boundary" }]);
});

test("rejects unbounded or invalid cleanup inputs before deleting and refuses an outer transaction", () => {
  const { store } = fixture();
  for (const limit of [0, -1, 1.5, 5_001, Infinity, NaN]) {
    expect(() => cleanupAccountHistory(store.database, NOW, limit)).toThrow(RangeError);
  }
  expect(() => cleanupAccountHistory(store.database, NaN)).toThrow(RangeError);
  store.database.transaction(() => {
    expect(() => cleanupAccountHistory(store.database, NOW)).toThrow("outside a transaction");
  })();
});

test("a failure rolls back only its table, and a later pass resumes cleanup", () => {
  const { store } = fixture();
  const record = outbox(store, "failed", 1);
  store.database.exec("CREATE TRIGGER synthetic_cleanup_failure BEFORE DELETE ON action_tokens BEGIN SELECT RAISE(ABORT, 'synthetic cleanup failure'); END");
  expect(() => cleanupAccountHistory(store.database, NOW)).toThrow("synthetic cleanup failure");
  expect(store.findEmailOutboxById(record.id)).toBeUndefined();
  expect(store.findActionTokenById(record.actionToken.id)).toBeDefined();
  expect(store.database.inTransaction).toBe(false);
  store.database.exec("DROP TRIGGER synthetic_cleanup_failure");
  expect(cleanupAccountHistory(store.database, NOW).action_tokens).toBe(1);
});

test("retention selections use time and reference indexes without sorting terminal history", () => {
  const { store } = fixture();
  const plan = (sql: string, values: number[]) => (store.database.query(`EXPLAIN QUERY PLAN ${sql}`).all(...values) as { detail: string }[])
    .map(row => row.detail).join("\n");
  const mailPlan = plan(`SELECT rowid FROM email_outbox INDEXED BY email_outbox_retention
    WHERE status IN ('delivered', 'failed') AND finished_at < ? ORDER BY finished_at, rowid LIMIT ?`, [NOW, 500]);
  expect(mailPlan).toContain("email_outbox_retention");
  expect(mailPlan).not.toContain("TEMP B-TREE");
  const tokenPlan = plan(`SELECT token.rowid FROM action_tokens token WHERE expires_at < ?
    AND NOT EXISTS (SELECT 1 FROM email_outbox WHERE action_token_id = token.id)
    ORDER BY expires_at, token.rowid LIMIT ?`, [NOW, 500]);
  expect(tokenPlan).toContain("action_tokens_retention");
  expect(tokenPlan).toContain("email_outbox_action_token");
  const sessionPlan = plan(`SELECT session.rowid FROM account_sessions session WHERE expires_at < ?
    AND NOT EXISTS (SELECT 1 FROM account_sessions family WHERE family.family_id = session.family_id AND family.expires_at >= ?)
    ORDER BY expires_at, session.rowid LIMIT ?`, [NOW, NOW, 500]);
  expect(sessionPlan).toContain("account_sessions_retention");
  expect(sessionPlan).toContain("account_sessions_family_retention");
});
