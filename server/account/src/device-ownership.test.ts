import { expect, test } from "bun:test";
import { Database } from "bun:sqlite";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { createAccountApp } from "./app";
import { loadAccountConfig } from "./config";
import { AccountStore } from "./db/store";
import { hashPassword } from "./domain/password";
import { hashOpaqueToken } from "./domain/tokens";
import { DevMailer } from "./providers/dev-mailer";
import { SessionService } from "./services/session-service";

const password = "synthetic device ownership password";
const deviceId = "synthetic-shared-installation";
const config = loadAccountConfig({ NODE_ENV: "test" });
const post = (path: string, body: unknown, accessToken?: string) => new Request(`http://synthetic.test${path}`, {
  method: "POST", headers: { "content-type": "application/json", ...(accessToken ? { authorization: `Bearer ${accessToken}` } : {}) },
  body: JSON.stringify(body),
});
const loginBody = (email: string, id = deviceId) => ({ email, password, deviceId: id, platform: "windows", deviceName: email });
const me = (accessToken: string) => new Request("http://synthetic.test/v1/account/me", { headers: { authorization: `Bearer ${accessToken}` } });

test("installation IDs are isolated by account, so another login cannot claim or block their original owner", async () => {
  const store = new AccountStore();
  const app = createAccountApp({ store, config, mailer: new DevMailer(), now: () => 100 });
  try {
    const passwordHash = await hashPassword(password);
    const users = ["a-synthetic@example.com", "b-synthetic@example.com"].map(email => store.createUser({ email, passwordHash, now: 1 }));
    for (const user of users) store.markEmailVerified(user.id, 1);
    const first = await app.fetch(post("/v1/account/login", loginBody(users[0].email)));
    expect(first.status).toBe(200);
    const aTokens = (await first.json()).tokens;
    const aDevice = store.findDevice(users[0].id, deviceId)!;
    const other = await app.fetch(post("/v1/account/login", loginBody(users[1].email)));
    expect(other.status).toBe(200);
    const bTokens = (await other.json()).tokens;
    expect(store.findDevice(users[0].id, deviceId)).toEqual(aDevice);
    expect(store.findDevice(users[1].id, deviceId)?.userId).toBe(users[1].id);
    expect((await app.fetch(me(aTokens.accessToken))).status).toBe(200);
    expect((await app.fetch(me(bTokens.accessToken))).status).toBe(200);

    expect((await app.fetch(post("/v1/account/logout", {}, aTokens.accessToken))).status).toBe(204);
    expect((await app.fetch(me(aTokens.accessToken))).status).toBe(401);
    expect((await app.fetch(me(bTokens.accessToken))).status).toBe(200);
    const returnToA = await app.fetch(post("/v1/account/login", loginBody(users[0].email)));
    expect(returnToA.status).toBe(200);
    expect(store.listDevices(users[0].id)).toHaveLength(1);
    expect(store.listDevices(users[1].id)).toHaveLength(1);
    expect(store.database.query("PRAGMA foreign_key_check").all()).toEqual([]);
  } finally { await app.close(); }
});

test("unverified registration with another installation label cannot mutate or block a logged-out owner's device", async () => {
  const store = new AccountStore();
  const app = createAccountApp({ store, config, mailer: new DevMailer(), now: () => 100 });
  try {
    const user = store.createUser({ email: "owner-synthetic@example.com", passwordHash: await hashPassword(password), now: 1 });
    store.markEmailVerified(user.id, 1);
    const first = await app.fetch(post("/v1/account/login", loginBody(user.email)));
    const tokens = (await first.json()).tokens;
    await app.fetch(post("/v1/account/logout", {}, tokens.accessToken));
    const originalDevice = store.findDevice(user.id, deviceId);
    const registration = await app.fetch(post("/v1/account/register", loginBody("unverified-synthetic@example.com")));
    expect(registration.status).toBe(202);
    expect(store.findUserByEmail("unverified-synthetic@example.com")?.emailVerifiedAt).toBeNull();
    expect(store.findDevice(user.id, deviceId)).toEqual(originalDevice);
    expect((await app.fetch(post("/v1/account/login", loginBody(user.email)))).status).toBe(200);
    expect(store.listDevices(user.id)).toHaveLength(1);
  } finally { await app.close(); }
});

test("composite device foreign keys reject cross-account sessions and deletion affects only that account", () => {
  const store = new AccountStore();
  try {
    const a = store.createUser({ email: "a-synthetic@example.com", passwordHash: "synthetic", now: 1 });
    const b = store.createUser({ email: "b-synthetic@example.com", passwordHash: "synthetic", now: 1 });
    const input = { id: deviceId, platform: "windows" as const, name: "Synthetic", now: 1 };
    store.upsertDevice({ ...input, userId: a.id });
    const sessionInput = { deviceId, familyId: "synthetic-family", accessTokenHash: "synthetic-access", accessExpiresAt: 100,
      refreshTokenHash: "synthetic-refresh", now: 1, expiresAt: 200 };
    expect(() => store.createSession({ ...sessionInput, userId: b.id })).toThrow("FOREIGN KEY");
    store.upsertDevice({ ...input, userId: b.id });
    const bSession = store.createSession({ ...sessionInput, userId: b.id });
    const aSession = store.createSession({ ...sessionInput, userId: a.id, accessTokenHash: "synthetic-a-access", refreshTokenHash: "synthetic-a-refresh" });
    store.database.query("DELETE FROM account_devices WHERE user_id = ? AND id = ?").run(a.id, deviceId);
    expect(store.findSessionById(aSession.id)).toBeUndefined();
    expect(store.findSessionById(bSession.id)).toBeDefined();
    expect(store.findDevice(b.id, deviceId)).toBeDefined();
  } finally { store.close(); }
});

function seedLegacy(path: string, corrupt = false): void {
  const database = new Database(path, { create: true, strict: true });
  try {
    for (const migration of ["001_account.sql", "002_readiness.sql", "003_lifecycle.sql"]) {
      database.exec(readFileSync(new URL(`./db/migrations/${migration}`, import.meta.url), "utf8"));
    }
    database.exec(`
      INSERT INTO users VALUES ('a','a-synthetic@example.com','synthetic',1,'active',1,1,1);
      INSERT INTO users VALUES ('b','b-synthetic@example.com','synthetic',1,'active',1,1,1);
      INSERT INTO users VALUES ('c','c-synthetic@example.com','synthetic',1,'active',1,1,1);
      INSERT INTO account_devices VALUES ('shared','b','windows','Private B name',6,9,NULL);
      INSERT INTO account_devices VALUES ('owned','c','macos','C Mac',1,9,NULL);
      INSERT INTO account_sessions VALUES ('a-old','a','shared','a-family','a-access',2000,'a-refresh',1,4,5000,4,5);
      INSERT INTO account_sessions VALUES ('a-inconsistent','a','shared','a-family','a-invalid-access',2000,'a-invalid-refresh',2,8,5000,NULL,NULL);
      INSERT INTO account_sessions VALUES ('b-live','b','shared','b-family','${hashOpaqueToken("b-access-raw")}',2000,'${hashOpaqueToken("b-refresh-raw")}',6,9,5000,NULL,NULL);
      INSERT INTO account_sessions VALUES ('c-old','c','owned','c-family','c-old-access',2000,'${hashOpaqueToken("c-old-refresh-raw")}',1,4,5000,4,NULL);
      INSERT INTO account_sessions VALUES ('c-live','c','owned','c-family','${hashOpaqueToken("c-live-access-raw")}',2000,'${hashOpaqueToken("c-live-refresh-raw")}',4,9,5000,NULL,NULL);
    `);
    if (corrupt) {
      database.exec("PRAGMA foreign_keys = OFF");
      database.exec("DELETE FROM users WHERE id = 'a'");
    }
  } finally { database.close(true); }
}

test("real legacy file migration retains sessions and replay history, reconstructs revoked cross-owner devices and survives restart", () => {
  const directory = mkdtempSync(join(tmpdir(), "desklink-device-migration-"));
  const path = join(directory, "synthetic.sqlite");
  seedLegacy(path);
  try {
    for (let restart = 0; restart < 2; restart++) {
      const store = new AccountStore(path);
      try {
        expect(store.readiness().database).toBe("ready");
        expect(store.database.query("PRAGMA user_version").all()).toEqual([{ user_version: 4 }]);
        expect(store.database.query("PRAGMA foreign_key_check").all()).toEqual([]);
        expect(store.database.query("SELECT count(*) AS n FROM account_sessions").get()).toEqual({ n: 5 });
        expect(store.findDevice("a", "shared")).toEqual({ id: "shared", userId: "a", platform: "windows", name: "历史设备", createdAt: 1, lastSeenAt: 8, revokedAt: 8 });
        expect(store.findDevice("b", "shared")?.name).toBe("Private B name");
        const history = store.findSessionById("a-old")!;
        expect([history.familyId, history.accessTokenHash, history.refreshTokenHash, history.rotatedAt, history.revokedAt]).toEqual(["a-family", "a-access", "a-refresh", 4, 5]);
        expect(store.findSessionById("a-inconsistent")?.revokedAt).toBe(8);
        const sessions = new SessionService(store, config, () => 1000);
        expect(sessions.authenticate(me("b-access-raw")).user.id).toBe("b");
        if (restart === 0) {
          const refreshed = sessions.refresh("b-refresh-raw");
          expect(sessions.authenticate(me(refreshed.accessToken)).user.id).toBe("b");
          // Restore the seeded active session for the restart's exact-token check.
          store.database.query("DELETE FROM account_sessions WHERE user_id = 'b' AND id <> 'b-live'").run();
          store.database.query("UPDATE account_sessions SET rotated_at = NULL WHERE id = 'b-live'").run();
          expect(() => sessions.refresh("c-old-refresh-raw")).toThrow("重复使用");
          expect(() => sessions.authenticate(me("c-live-access-raw"))).toThrow();
        }
      } finally { store.close(); }
    }
  } finally { rmSync(directory, { recursive: true, force: true, maxRetries: 5, retryDelay: 20 }); }
});

test("a failed file migration rolls schema and data back atomically and releases the file", () => {
  const directory = mkdtempSync(join(tmpdir(), "desklink-device-rollback-"));
  const path = join(directory, "synthetic.sqlite");
  seedLegacy(path, true);
  try {
    let unexpectedlyOpened: AccountStore | undefined;
    try {
      expect(() => { unexpectedlyOpened = new AccountStore(path); }).toThrow("source failed foreign key validation");
    } finally { unexpectedlyOpened?.close(); }
    const database = new Database(path, { readonly: true });
    try {
      const rows = (sql: string) => { const statement = database.prepare(sql); try { return statement.all(); } finally { statement.finalize(); } };
      expect(rows("PRAGMA user_version")).toEqual([{ user_version: 3 }]);
      expect(rows("SELECT name FROM sqlite_schema WHERE name LIKE '%_v4'")).toEqual([]);
      expect(rows("SELECT id, user_id FROM account_devices WHERE id = 'shared'")).toEqual([{ id: "shared", user_id: "b" }]);
      expect(rows("SELECT id, family_id, revoked_at FROM account_sessions WHERE id = 'a-old'")).toEqual([{ id: "a-old", family_id: "a-family", revoked_at: 5 }]);
    } finally { database.close(true); }
  } finally { rmSync(directory, { recursive: true, force: true, maxRetries: 5, retryDelay: 20 }); }
});

test("a checked migration statement failure after copying and replacing tables rolls the entire file back", () => {
  const directory = mkdtempSync(join(tmpdir(), "desklink-device-ddl-rollback-"));
  const path = join(directory, "synthetic.sqlite");
  seedLegacy(path);
  const legacy = new Database(path);
  try {
    // Move this name to another table, so recreating the device index fails
    // only after the migration has copied and replaced both old tables.
    legacy.exec("DROP INDEX account_devices_by_user; CREATE INDEX account_devices_by_user ON users(id)");
  } finally { legacy.close(true); }
  try {
    let unexpectedlyOpened: AccountStore | undefined;
    try {
      expect(() => { unexpectedlyOpened = new AccountStore(path); }).toThrow();
    } finally { unexpectedlyOpened?.close(); }
    const database = new Database(path, { readonly: true });
    try {
      const rows = (sql: string) => { const statement = database.prepare(sql); try { return statement.all(); } finally { statement.finalize(); } };
      expect(rows("PRAGMA user_version")).toEqual([{ user_version: 3 }]);
      expect(rows("SELECT name FROM sqlite_schema WHERE name LIKE '%_v4'")).toEqual([]);
      expect(rows("SELECT count(*) AS n FROM account_sessions")).toEqual([{ n: 5 }]);
      expect(rows("SELECT id, user_id FROM account_devices WHERE id = 'shared'")).toEqual([{ id: "shared", user_id: "b" }]);
      expect(rows("PRAGMA foreign_key_check")).toEqual([]);
    } finally { database.close(true); }
  } finally { rmSync(directory, { recursive: true, force: true, maxRetries: 5, retryDelay: 20 }); }
});
