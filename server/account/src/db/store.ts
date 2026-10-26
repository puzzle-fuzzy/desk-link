import { randomUUID } from "node:crypto";
import { readFileSync } from "node:fs";

import { Database } from "bun:sqlite";
import { AccountDatabase } from "./database";

import type {
  AccountDevice,
  AccountPlatform,
  AccountSession,
  ActionTokenPurpose,
  ActionTokenRecord,
  EmailOutboxTemplate,
  EmailOutboxRecord,
  UserRecord,
  UserStatus,
} from "./types";

interface UserRow {
  id: string;
  email: string;
  password_hash: string;
  email_verified_at: number | null;
  status: UserStatus;
  created_at: number;
  updated_at: number;
  password_changed_at: number;
}

interface DeviceRow {
  id: string;
  user_id: string;
  platform: AccountPlatform;
  name: string;
  created_at: number;
  last_seen_at: number;
  revoked_at: number | null;
}

interface SessionRow {
  id: string;
  user_id: string;
  device_id: string;
  family_id: string;
  access_token_hash: string;
  access_expires_at: number;
  refresh_token_hash: string;
  created_at: number;
  last_seen_at: number;
  expires_at: number;
  rotated_at: number | null;
  revoked_at: number | null;
}

interface ActionTokenRow {
  id: string;
  user_id: string;
  purpose: ActionTokenPurpose;
  token_hash: string;
  created_at: number;
  expires_at: number;
  consumed_at: number | null;
}

interface EmailOutboxRow {
  id: string;
  user_id: string;
  recipient: string;
  template: EmailOutboxTemplate;
  action_token_id: string;
  created_at: number;
  available_at: number;
  attempts: number;
  sent_at: number | null;
  last_error: string | null;
  status: EmailOutboxRecord["status"];
  lease_token: string | null;
  lease_until: number | null;
  finished_at: number | null;
}

export interface CreateUserInput {
  email: string;
  passwordHash: string;
  now: number;
}

export interface CreateDeviceInput {
  id: string;
  userId: string;
  platform: AccountPlatform;
  name: string;
  now: number;
}

export interface CreateSessionInput {
  userId: string;
  deviceId: string;
  familyId: string;
  accessTokenHash: string;
  accessExpiresAt: number;
  refreshTokenHash: string;
  now: number;
  expiresAt: number;
}

export interface CreateActionTokenInput {
  userId: string;
  purpose: ActionTokenPurpose;
  tokenHash: string;
  now: number;
  expiresAt: number;
}

export interface CreateEmailOutboxInput {
  userId: string;
  recipient: string;
  template: EmailOutboxTemplate;
  actionTokenId: string;
  now: number;
}

export class AccountStore {
  readonly database: Database;

  constructor(path = ":memory:") {
    this.database = new AccountDatabase(path, { create: true, strict: true });
    this.database.exec("PRAGMA foreign_keys = ON");
    // SQLite calls are synchronous. Bound lock waiting inside SQLite rather
    // than a Promise timeout, which cannot interrupt a blocked SQLite call.
    this.database.exec("PRAGMA busy_timeout = 25");
    if (path !== ":memory:") {
      this.database.exec("PRAGMA journal_mode = WAL");
      this.database.exec("PRAGMA synchronous = NORMAL");
    }
    try {
      this.database.transaction(() => {
        const version = (this.database.query("PRAGMA user_version").all()[0] as { user_version: number }).user_version;
        if (version > 4) throw new Error("unsupported account schema version");
        if (version < 2) {
          this.database.exec(readFileSync(new URL("./migrations/001_account.sql", import.meta.url), "utf8"));
          this.database.exec(readFileSync(new URL("./migrations/002_readiness.sql", import.meta.url), "utf8"));
        }
        if (version < 3) this.database.exec(readFileSync(new URL("./migrations/003_lifecycle.sql", import.meta.url), "utf8"));
        if (version < 4) {
          if (this.database.query("PRAGMA foreign_key_check").all().length > 0) {
            throw new Error("account migration source failed foreign key validation");
          }
          const sessionCount = (this.database.query("SELECT count(*) AS n FROM account_sessions").get() as { n: number }).n;
          // Bun 1.3.14 exec() can continue a multi-statement script after a
          // constraint failure. This fixed DDL migration contains no embedded
          // semicolons or triggers: step/finalize every statement explicitly.
          for (const sql of readFileSync(new URL("./migrations/004_account_devices.sql", import.meta.url), "utf8").split(";")) {
            if (!sql.trim()) continue;
            const statement = this.database.prepare(sql);
            try { statement.run(); } finally { statement.finalize(); }
          }
          if (this.database.query("PRAGMA foreign_key_check").all().length > 0
            || (this.database.query("SELECT count(*) AS n FROM account_sessions").get() as { n: number }).n !== sessionCount) {
            throw new Error("account migration failed data validation");
          }
        }
      }).immediate();
    } catch (error) {
      this.database.close(true);
      throw error;
    }
  }

  close(): void {
    this.database.close(true);
  }

  readiness(): { database: "ready" | "unavailable" | "schema_mismatch"; mail: "ready" | "degraded" } {
    let begun = false;
    try {
      if ((this.database.query("PRAGMA user_version").all()[0] as { user_version: number }).user_version !== 4) {
        return { database: "schema_mismatch", mail: "degraded" };
      }
      // Prepare against the current schema, including all request-critical columns.
      for (const sql of [
        "SELECT id, email, password_hash, email_verified_at, status, created_at, updated_at, password_changed_at FROM users LIMIT 0",
        "SELECT id, user_id, platform, name, created_at, last_seen_at, revoked_at FROM account_devices LIMIT 0",
        "SELECT id, user_id, device_id, family_id, access_token_hash, access_expires_at, refresh_token_hash, created_at, last_seen_at, expires_at, rotated_at, revoked_at FROM account_sessions LIMIT 0",
        "SELECT id, user_id, purpose, token_hash, created_at, expires_at, consumed_at FROM action_tokens LIMIT 0",
        "SELECT id, user_id, device_id, action, created_at, metadata_json FROM audit_events LIMIT 0",
        "SELECT id, user_id, recipient, template, action_token_id, created_at, available_at, attempts, sent_at, last_error, status, lease_token, lease_until, finished_at FROM email_outbox LIMIT 0",
      ]) this.database.query(sql).all();
      this.database.exec("BEGIN IMMEDIATE");
      begun = true;
      const written = this.database.query("UPDATE readiness_probe SET id = 1 WHERE id = 1").run();
      if (written.changes !== 1) return { database: "schema_mismatch", mail: "degraded" };
      const failed = this.database.query("SELECT 1 FROM email_outbox WHERE sent_at IS NULL AND attempts > 0 AND last_error IS NOT NULL LIMIT 1").get();
      return { database: "ready", mail: failed ? "degraded" : "ready" };
    } catch {
      return { database: "unavailable", mail: "degraded" };
    } finally {
      if (begun) this.database.exec("ROLLBACK");
    }
  }

  createUser(input: CreateUserInput): UserRecord {
    const id = randomUUID();
    this.database.query(`
      INSERT INTO users (
        id, email, password_hash, email_verified_at, status,
        created_at, updated_at, password_changed_at
      ) VALUES (?, ?, ?, NULL, 'active', ?, ?, ?)
    `).run(id, input.email, input.passwordHash, input.now, input.now, input.now);
    return this.findUserById(id)!;
  }

  createUserIfAbsent(input: CreateUserInput): UserRecord | undefined {
    const row = this.database.query(`
      INSERT INTO users (
        id, email, password_hash, email_verified_at, status,
        created_at, updated_at, password_changed_at
      ) VALUES (?, ?, ?, NULL, 'active', ?, ?, ?)
      ON CONFLICT(email) DO NOTHING RETURNING *
    `).get(randomUUID(), input.email, input.passwordHash, input.now, input.now, input.now) as UserRow | null;
    return row ? mapUser(row) : undefined;
  }

  findUserById(id: string): UserRecord | undefined {
    const row = this.database.query("SELECT * FROM users WHERE id = ?").get(id) as UserRow | null;
    return row ? mapUser(row) : undefined;
  }

  findUserByEmail(email: string): UserRecord | undefined {
    const row = this.database.query("SELECT * FROM users WHERE email = ?").get(email) as UserRow | null;
    return row ? mapUser(row) : undefined;
  }

  markEmailVerified(userId: string, now: number): void {
    this.database.query("UPDATE users SET email_verified_at = ?, updated_at = ? WHERE id = ?")
      .run(now, now, userId);
  }

  updatePassword(userId: string, passwordHash: string, now: number): void {
    this.database.query("UPDATE users SET password_hash = ?, password_changed_at = ?, updated_at = ? WHERE id = ?")
      .run(passwordHash, now, now, userId);
  }

  upsertDevice(input: CreateDeviceInput): AccountDevice {
    this.database.query(`INSERT INTO account_devices (id, user_id, platform, name, created_at, last_seen_at, revoked_at)
      VALUES (?, ?, ?, ?, ?, ?, NULL)
      ON CONFLICT(user_id, id) DO UPDATE SET platform = excluded.platform, name = excluded.name,
        last_seen_at = excluded.last_seen_at, revoked_at = NULL`)
      .run(input.id, input.userId, input.platform, input.name, input.now, input.now);
    return this.findDevice(input.userId, input.id)!;
  }

  findDevice(userId: string, id: string): AccountDevice | undefined {
    const row = this.database.query("SELECT * FROM account_devices WHERE user_id = ? AND id = ?").get(userId, id) as DeviceRow | null;
    return row ? mapDevice(row) : undefined;
  }

  listDevices(userId: string): AccountDevice[] {
    const rows = this.database.query("SELECT * FROM account_devices WHERE user_id = ? ORDER BY last_seen_at DESC")
      .all(userId) as DeviceRow[];
    return rows.map(mapDevice);
  }

  revokeDevice(userId: string, deviceId: string, now: number): boolean {
    const result = this.database.query("UPDATE account_devices SET revoked_at = ?, last_seen_at = ? WHERE id = ? AND user_id = ? AND revoked_at IS NULL")
      .run(now, now, deviceId, userId);
    this.database.query("UPDATE account_sessions SET revoked_at = ? WHERE user_id = ? AND device_id = ? AND revoked_at IS NULL")
      .run(now, userId, deviceId);
    return result.changes > 0;
  }

  createSession(input: CreateSessionInput): AccountSession {
    const id = randomUUID();
    this.database.query(`
      INSERT INTO account_sessions (
        id, user_id, device_id, family_id, access_token_hash, access_expires_at,
        refresh_token_hash, created_at, last_seen_at, expires_at, rotated_at, revoked_at
      ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, NULL)
    `).run(
      id,
      input.userId,
      input.deviceId,
      input.familyId,
      input.accessTokenHash,
      input.accessExpiresAt,
      input.refreshTokenHash,
      input.now,
      input.now,
      input.expiresAt,
    );
    return this.findSessionById(id)!;
  }

  findSessionById(id: string): AccountSession | undefined {
    const row = this.database.query("SELECT * FROM account_sessions WHERE id = ?").get(id) as SessionRow | null;
    return row ? mapSession(row) : undefined;
  }

  findSessionByAccessHash(accessTokenHash: string, now: number): AccountSession | undefined {
    const row = this.database.query("SELECT * FROM account_sessions WHERE access_token_hash = ? AND access_expires_at > ? AND revoked_at IS NULL")
      .get(accessTokenHash, now) as SessionRow | null;
    return row ? mapSession(row) : undefined;
  }

  findSessionByRefreshHash(refreshTokenHash: string): AccountSession | undefined {
    const row = this.database.query("SELECT * FROM account_sessions WHERE refresh_token_hash = ?")
      .get(refreshTokenHash) as SessionRow | null;
    return row ? mapSession(row) : undefined;
  }

  touchSession(id: string, now: number): void {
    this.database.query("UPDATE account_sessions SET last_seen_at = ? WHERE id = ? AND revoked_at IS NULL")
      .run(now, id);
  }

  revokeSession(id: string, now: number): void {
    this.database.query("UPDATE account_sessions SET revoked_at = ? WHERE id = ? AND revoked_at IS NULL")
      .run(now, id);
  }

  revokeUserSessions(userId: string, now: number): void {
    this.database.query("UPDATE account_sessions SET revoked_at = ? WHERE user_id = ? AND revoked_at IS NULL")
      .run(now, userId);
  }

  revokeDeviceSessions(userId: string, deviceId: string, now: number): void {
    this.database.query("UPDATE account_sessions SET revoked_at = ? WHERE user_id = ? AND device_id = ? AND revoked_at IS NULL")
      .run(now, userId, deviceId);
  }

  revokeSessionFamily(familyId: string, now: number): void {
    this.database.query("UPDATE account_sessions SET revoked_at = ? WHERE family_id = ? AND revoked_at IS NULL")
      .run(now, familyId);
  }

  recordAuditEvent(input: {
    userId?: string;
    deviceId?: string;
    action: string;
    now: number;
    metadata?: Record<string, string | number | boolean | null>;
  }): void {
    this.database.query("INSERT INTO audit_events (id, user_id, device_id, action, created_at, metadata_json) VALUES (?, ?, ?, ?, ?, ?)")
      .run(
        randomUUID(),
        input.userId ?? null,
        input.deviceId ?? null,
        input.action,
        input.now,
        JSON.stringify(input.metadata ?? {}),
      );
  }

  enqueueEmail(input: CreateEmailOutboxInput): EmailOutboxRecord {
    const id = randomUUID();
    this.database.query("INSERT INTO email_outbox (id, user_id, recipient, template, action_token_id, created_at, available_at, attempts, sent_at, last_error) VALUES (?, ?, ?, ?, ?, ?, ?, 0, NULL, NULL)")
      .run(id, input.userId, input.recipient, input.template, input.actionTokenId, input.now, input.now);
    return this.findEmailOutboxById(id)!;
  }

  findEmailOutboxById(id: string): EmailOutboxRecord | undefined {
    const row = this.database.query("SELECT * FROM email_outbox WHERE id = ?")
      .get(id) as EmailOutboxRow | null;
    return row ? mapEmailOutbox(row) : undefined;
  }

  listPendingEmails(now: number, limit = 20): EmailOutboxRecord[] {
    const rows = this.database.query("SELECT * FROM email_outbox WHERE status = 'pending' AND available_at <= ? AND attempts < 10 ORDER BY created_at ASC LIMIT ?")
      .all(now, limit) as EmailOutboxRow[];
    return rows.map(mapEmailOutbox);
  }

  claimEmail(now: number, leaseSeconds = 30): EmailOutboxRecord | undefined {
    if (!Number.isSafeInteger(leaseSeconds) || leaseSeconds < 15 || leaseSeconds > 300) throw new RangeError("invalid email lease duration");
    return this.database.transaction(() => {
      this.database.query(`UPDATE email_outbox SET status = 'failed', finished_at = ?,
        lease_token = NULL, lease_until = NULL, last_error = 'delivery_lease_expired'
        WHERE rowid IN (SELECT rowid FROM email_outbox INDEXED BY email_outbox_final_leases
          WHERE status = 'delivering' AND attempts >= 10 AND lease_until <= ?
          ORDER BY lease_until LIMIT 100)`).run(now, now);
      const row = this.database.query(`UPDATE email_outbox SET status = 'delivering', attempts = attempts + 1,
        lease_token = ?, lease_until = ? WHERE id = COALESCE(
          (SELECT id FROM email_outbox WHERE status = 'delivering' AND lease_until <= ? AND attempts < 10
            ORDER BY lease_until LIMIT 1),
          (SELECT id FROM email_outbox WHERE status = 'pending' AND available_at <= ? AND attempts < 10
            ORDER BY available_at, created_at LIMIT 1)
        ) RETURNING *`).get(randomUUID(), now + leaseSeconds, now, now) as EmailOutboxRow | null;
      return row ? mapEmailOutbox(row) : undefined;
    }).immediate();
  }

  completeClaimedEmail(record: EmailOutboxRecord, now: number): boolean {
    return this.database.query(`UPDATE email_outbox SET status = 'delivered', sent_at = ?, finished_at = ?,
      last_error = NULL, lease_token = NULL, lease_until = NULL
      WHERE id = ? AND status = 'delivering' AND lease_token = ? AND lease_until > ?`).run(now, now, record.id, record.leaseToken, now).changes === 1;
  }

  failClaimedEmail(record: EmailOutboxRecord, now: number, reason: "mail_delivery_failed" | "account_unavailable"): boolean {
    const delay = Math.min(3_600, 10 * (2 ** Math.min(record.attempts - 1, 8)));
    return this.database.query(`UPDATE email_outbox SET status = CASE WHEN attempts >= 10 THEN 'failed' ELSE 'pending' END,
      finished_at = CASE WHEN attempts >= 10 THEN ? ELSE NULL END, available_at = ?, last_error = ?, lease_token = NULL, lease_until = NULL
      WHERE id = ? AND status = 'delivering' AND lease_token = ? AND lease_until > ?`).run(now, now + delay, reason, record.id, record.leaseToken, now).changes === 1;
  }

  prepareClaimedEmailToken(record: EmailOutboxRecord, tokenHash: string, now: number): boolean {
    return this.database.transaction(() => {
      const owned = this.database.query("SELECT 1 FROM email_outbox WHERE id = ? AND status = 'delivering' AND lease_token = ? AND lease_until > ?")
        .get(record.id, record.leaseToken, now);
      if (!owned) return false;
      this.invalidateActionToken(record.actionTokenId, now);
      const token = this.createActionToken({ userId: record.userId, purpose: record.template, tokenHash, now,
        expiresAt: now + (record.template === "verify_email" ? 86_400 : 1_800) });
      this.database.query("UPDATE email_outbox SET action_token_id = ? WHERE id = ?").run(token.id, record.id);
      return true;
    }).immediate();
  }

  requeueFailedEmail(id: string, now: number): boolean {
    return this.database.transaction(() => {
      const row = this.database.query(`UPDATE email_outbox SET status = 'pending', attempts = 0, available_at = ?,
        finished_at = NULL, last_error = NULL WHERE id = ? AND status = 'failed' RETURNING user_id`).get(now, id) as { user_id: string } | null;
      if (!row) return false;
      this.recordAuditEvent({ userId: row.user_id, action: "account.email_requeued", now, metadata: { outboxId: id } });
      return true;
    }).immediate();
  }

  rotateSession(oldId: string, replacement: CreateSessionInput, now: number): AccountSession {
    const transaction = this.database.transaction(() => {
      const old = this.findSessionById(oldId);
      if (!old || old.revokedAt !== null || old.expiresAt <= now) {
        throw new Error("session is not rotatable");
      }
      const result = this.database.query("UPDATE account_sessions SET rotated_at = ?, last_seen_at = ? WHERE id = ? AND rotated_at IS NULL AND revoked_at IS NULL")
        .run(now, now, oldId);
      if (result.changes !== 1) {
        throw new Error("session was already rotated");
      }
      return this.createSession(replacement);
    });
    return transaction();
  }

  createActionToken(input: CreateActionTokenInput): ActionTokenRecord {
    const id = randomUUID();
    this.database.query("INSERT INTO action_tokens (id, user_id, purpose, token_hash, created_at, expires_at, consumed_at) VALUES (?, ?, ?, ?, ?, ?, NULL)")
      .run(id, input.userId, input.purpose, input.tokenHash, input.now, input.expiresAt);
    return this.findActionTokenById(id)!;
  }

  findActionTokenById(id: string): ActionTokenRecord | undefined {
    const row = this.database.query("SELECT * FROM action_tokens WHERE id = ?").get(id) as ActionTokenRow | null;
    return row ? mapActionToken(row) : undefined;
  }

  findValidActionToken(tokenHash: string, purpose: ActionTokenPurpose, now: number): ActionTokenRecord | undefined {
    const row = this.database.query("SELECT * FROM action_tokens WHERE token_hash = ? AND purpose = ? AND consumed_at IS NULL AND expires_at > ?")
      .get(tokenHash, purpose, now) as ActionTokenRow | null;
    return row ? mapActionToken(row) : undefined;
  }

  consumeActionToken(tokenHash: string, purpose: ActionTokenPurpose, now: number): ActionTokenRecord | undefined {
    const transaction = this.database.transaction(() => {
      const row = this.database.query("SELECT * FROM action_tokens WHERE token_hash = ? AND purpose = ? AND consumed_at IS NULL AND expires_at > ?")
        .get(tokenHash, purpose, now) as ActionTokenRow | null;
      if (!row) return undefined;
      const result = this.database.query("UPDATE action_tokens SET consumed_at = ? WHERE id = ? AND consumed_at IS NULL")
        .run(now, row.id);
      return result.changes === 1 ? mapActionToken({ ...row, consumed_at: now }) : undefined;
    });
    return transaction();
  }

  invalidateActionToken(id: string, now: number): void {
    this.database.query("UPDATE action_tokens SET consumed_at = COALESCE(consumed_at, ?) WHERE id = ?")
      .run(now, id);
  }
}

function mapUser(row: UserRow): UserRecord {
  return {
    id: row.id,
    email: row.email,
    passwordHash: row.password_hash,
    emailVerifiedAt: row.email_verified_at,
    status: row.status,
    createdAt: row.created_at,
    updatedAt: row.updated_at,
    passwordChangedAt: row.password_changed_at,
  };
}

function mapDevice(row: DeviceRow): AccountDevice {
  return {
    id: row.id,
    userId: row.user_id,
    platform: row.platform,
    name: row.name,
    createdAt: row.created_at,
    lastSeenAt: row.last_seen_at,
    revokedAt: row.revoked_at,
  };
}

function mapSession(row: SessionRow): AccountSession {
  return {
    id: row.id,
    userId: row.user_id,
    deviceId: row.device_id,
    familyId: row.family_id,
    accessTokenHash: row.access_token_hash,
    accessExpiresAt: row.access_expires_at,
    refreshTokenHash: row.refresh_token_hash,
    createdAt: row.created_at,
    lastSeenAt: row.last_seen_at,
    expiresAt: row.expires_at,
    rotatedAt: row.rotated_at,
    revokedAt: row.revoked_at,
  };
}

function mapActionToken(row: ActionTokenRow): ActionTokenRecord {
  return {
    id: row.id,
    userId: row.user_id,
    purpose: row.purpose,
    tokenHash: row.token_hash,
    createdAt: row.created_at,
    expiresAt: row.expires_at,
    consumedAt: row.consumed_at,
  };
}

function mapEmailOutbox(row: EmailOutboxRow): EmailOutboxRecord {
  return {
    id: row.id,
    userId: row.user_id,
    recipient: row.recipient,
    template: row.template,
    actionTokenId: row.action_token_id,
    createdAt: row.created_at,
    availableAt: row.available_at,
    attempts: row.attempts,
    sentAt: row.sent_at,
    lastError: row.last_error,
    status: row.status,
    leaseToken: row.lease_token,
    leaseUntil: row.lease_until,
    finishedAt: row.finished_at,
  };
}
