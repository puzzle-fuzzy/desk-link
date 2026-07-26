import { randomUUID } from "node:crypto";
import { readFileSync } from "node:fs";

import { Database } from "bun:sqlite";

import { AccountError } from "../domain/errors";
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
    this.database = new Database(path, { create: true, strict: true });
    this.database.exec("PRAGMA foreign_keys = ON");
    if (path !== ":memory:") {
      this.database.exec("PRAGMA journal_mode = WAL");
      this.database.exec("PRAGMA synchronous = NORMAL");
    }
    this.database.exec(readFileSync(new URL("./migrations/001_account.sql", import.meta.url), "utf8"));
  }

  close(): void {
    this.database.close(false);
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
    const existing = this.database.query("SELECT * FROM account_devices WHERE id = ?")
      .get(input.id) as DeviceRow | null;
    if (existing && existing.user_id !== input.userId && existing.revoked_at === null) {
      throw new AccountError("conflict", 409, "设备正在使用其他账号。请先退出原账号。");
    }
    if (existing && existing.user_id !== input.userId) {
      this.database.query("UPDATE account_devices SET user_id = ?, platform = ?, name = ?, created_at = ?, last_seen_at = ?, revoked_at = NULL WHERE id = ?")
        .run(input.userId, input.platform, input.name, input.now, input.now, input.id);
      return this.findDevice(input.id)!;
    }
    if (existing) {
      this.database.query("UPDATE account_devices SET name = ?, platform = ?, last_seen_at = ?, revoked_at = NULL WHERE id = ?")
        .run(input.name, input.platform, input.now, input.id);
    } else {
      this.database.query("INSERT INTO account_devices (id, user_id, platform, name, created_at, last_seen_at, revoked_at) VALUES (?, ?, ?, ?, ?, ?, NULL)")
        .run(input.id, input.userId, input.platform, input.name, input.now, input.now);
    }
    return this.findDevice(input.id)!;
  }

  findDevice(id: string): AccountDevice | undefined {
    const row = this.database.query("SELECT * FROM account_devices WHERE id = ?").get(id) as DeviceRow | null;
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
    const rows = this.database.query("SELECT * FROM email_outbox WHERE sent_at IS NULL AND available_at <= ? AND attempts < 10 ORDER BY created_at ASC LIMIT ?")
      .all(now, limit) as EmailOutboxRow[];
    return rows.map(mapEmailOutbox);
  }

  markEmailSent(id: string, now: number): void {
    this.database.query("UPDATE email_outbox SET sent_at = ?, last_error = NULL WHERE id = ? AND sent_at IS NULL")
      .run(now, id);
  }

  markEmailFailed(record: EmailOutboxRecord, now: number, error: string): void {
    const delay = Math.min(3_600, 10 * (2 ** Math.min(record.attempts, 8)));
    this.database.query("UPDATE email_outbox SET attempts = attempts + 1, available_at = ?, last_error = ? WHERE id = ? AND sent_at IS NULL")
      .run(now + delay, error.slice(0, 500), record.id);
  }

  replaceEmailActionToken(id: string, actionTokenId: string): void {
    this.database.query("UPDATE email_outbox SET action_token_id = ? WHERE id = ? AND sent_at IS NULL")
      .run(actionTokenId, id);
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
  };
}
