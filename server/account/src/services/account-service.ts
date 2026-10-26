import type { AccountConfig } from "../config";
import { normalizeEmail } from "../domain/email";
import { AccountError } from "../domain/errors";
import { hashPassword, validatePassword, verifyPassword } from "../domain/password";
import { createOpaqueToken, hashOpaqueToken } from "../domain/tokens";
import type { AccountStore } from "../db/store";
import type { AccountPlatform, ActionTokenPurpose, EmailOutboxRecord, UserRecord } from "../db/types";
import type { MailMessage, Mailer } from "../providers/mailer";
import type { SessionService, SessionTokenPair } from "./session-service";

export interface RegisterInput {
  email: unknown;
  password: unknown;
  deviceId: unknown;
  platform: unknown;
  deviceName: unknown;
}

export interface LoginInput extends RegisterInput {}

export class AccountService {
  private outboxFlushInFlight?: Promise<number>;
  private outboxStopped = false;

  constructor(
    private readonly store: AccountStore,
    private readonly config: AccountConfig,
    private readonly mailer: Mailer,
    private readonly sessions: SessionService,
    private readonly now: () => number,
  ) {}

  async register(input: RegisterInput): Promise<{ verificationRequired: true }> {
    const email = normalizeEmail(input.email);
    validatePassword(input.password);
    const device = readDeviceInput(input);
    if (this.store.findUserByEmail(email)) {
      return { verificationRequired: true };
    }
    const now = this.now();
    // Hashing stays outside the synchronous transaction; independent registrations
    // can hash concurrently, while only the winner of the email insert has effects.
    const passwordHash = await hashPassword(input.password);
    this.store.database.transaction(() => {
      const user = this.store.createUserIfAbsent({ email, passwordHash, now });
      if (!user) return undefined;
      this.store.upsertDevice({ ...device, userId: user.id, now });
      this.store.recordAuditEvent({ userId: user.id, deviceId: device.id, action: "account.registered", now });
      return this.enqueueActionEmail(user, "verify_email");
    })();
    return { verificationRequired: true };
  }

  async resendVerification(emailInput: unknown): Promise<void> {
    const email = normalizeEmail(emailInput);
    const user = this.store.findUserByEmail(email);
    if (!user || user.emailVerifiedAt !== null || user.status !== "active") return;
    this.store.database.transaction(() => {
      this.store.recordAuditEvent({ userId: user.id, action: "account.verification_resent", now: this.now() });
      this.enqueueActionEmail(user, "verify_email");
    })();
  }

  async verifyEmail(tokenInput: unknown): Promise<PublicUser> {
    const token = readToken(tokenInput, "邮箱验证令牌");
    const record = this.store.consumeActionToken(hashOpaqueToken(token), "verify_email", this.now());
    if (!record) throw new AccountError("invalid_request", 400, "邮箱验证链接无效或已过期。 ");
    const user = this.store.findUserById(record.userId);
    if (!user || user.status !== "active") throw new AccountError("invalid_request", 400, "邮箱验证链接无效或已过期。 ");
    this.store.markEmailVerified(user.id, this.now());
    this.store.recordAuditEvent({ userId: user.id, action: "account.email_verified", now: this.now() });
    return publicUser(this.store.findUserById(user.id)!);
  }

  async login(input: LoginInput): Promise<{ user: PublicUser; tokens: SessionTokenPair }> {
    const email = normalizeEmail(input.email);
    const password = input.password;
    if (typeof password !== "string") {
      throw new AccountError("invalid_credentials", 401, "邮箱或密码不正确。 ");
    }
    const user = this.store.findUserByEmail(email);
    if (!user || user.status !== "active" || !(await verifyPassword(password, user.passwordHash))) {
      throw new AccountError("invalid_credentials", 401, "邮箱或密码不正确。 ");
    }
    if (user.emailVerifiedAt === null) {
      throw new AccountError("email_not_verified", 403, "请先完成邮箱验证。 ");
    }
    const device = readDeviceInput(input);
    const accountDevice = this.store.upsertDevice({ ...device, userId: user.id, now: this.now() });
    const tokens = this.sessions.create(user, accountDevice);
    this.store.recordAuditEvent({ userId: user.id, deviceId: accountDevice.id, action: "account.logged_in", now: this.now() });
    return { user: publicUser(user), tokens };
  }

  async requestPasswordReset(emailInput: unknown): Promise<void> {
    const email = normalizeEmail(emailInput);
    const user = this.store.findUserByEmail(email);
    if (!user || user.status !== "active") return;
    this.store.database.transaction(() => {
      this.store.recordAuditEvent({ userId: user.id, action: "account.password_reset_requested", now: this.now() });
      this.enqueueActionEmail(user, "reset_password");
    })();
  }

  async resetPassword(tokenInput: unknown, passwordInput: unknown): Promise<void> {
    const token = readToken(tokenInput, "密码重置令牌");
    validatePassword(passwordInput);
    const record = this.store.consumeActionToken(hashOpaqueToken(token), "reset_password", this.now());
    if (!record) throw new AccountError("invalid_request", 400, "密码重置链接无效或已过期。 ");
    const passwordHash = await hashPassword(passwordInput);
    this.store.updatePassword(record.userId, passwordHash, this.now());
    this.store.revokeUserSessions(record.userId, this.now());
    this.store.recordAuditEvent({ userId: record.userId, action: "account.password_reset", now: this.now() });
  }

  flushOutbox(limit = 20): Promise<number> {
    if (this.outboxFlushInFlight) return this.outboxFlushInFlight;
    if (this.outboxStopped) return Promise.resolve(0);
    this.outboxFlushInFlight = this.flushOutboxInternal(limit).finally(() => {
      this.outboxFlushInFlight = undefined;
    });
    return this.outboxFlushInFlight;
  }

  async stopOutbox(): Promise<void> {
    this.outboxStopped = true;
    await this.outboxFlushInFlight;
  }

  private async flushOutboxInternal(limit: number): Promise<number> {
    if (!Number.isSafeInteger(limit) || limit < 1 || limit > 100) throw new RangeError("invalid outbox batch size");
    let delivered = 0;
    for (let index = 0; index < limit && !this.outboxStopped; index++) {
      // Claim only the next send, so queued messages cannot expire behind a slow send.
      const record = this.store.claimEmail(this.now());
      if (!record) break;
      const user = this.store.findUserById(record.userId);
      if (!user || user.status !== "active") {
        this.store.failClaimedEmail(record, this.now(), "account_unavailable");
        continue;
      }
      try {
        const token = createOpaqueToken();
        if (!this.store.prepareClaimedEmailToken(record, token.hash, this.now())) continue;
        await this.mailer.send(this.actionEmailMessage(user, record.template, token.value));
        if (this.store.completeClaimedEmail(record, this.now())) delivered++;
      } catch {
        // Provider errors may contain URLs/credentials; persist a fixed category.
        this.store.failClaimedEmail(record, this.now(), "mail_delivery_failed");
      }
    }
    return delivered;
  }

  private enqueueActionEmail(user: UserRecord, purpose: ActionTokenPurpose): EmailOutboxRecord {
    const token = createOpaqueToken();
    const actionToken = this.store.createActionToken({
      userId: user.id, purpose, tokenHash: token.hash, now: this.now(),
      expiresAt: this.now() + (purpose === "verify_email" ? 86_400 : 1_800),
    });
    return this.store.enqueueEmail({
      userId: user.id, recipient: user.email, template: purpose,
      actionTokenId: actionToken.id, now: this.now(),
    });
  }

  private actionEmailMessage(user: UserRecord, purpose: ActionTokenPurpose, token: string): MailMessage {
    const action = purpose === "verify_email" ? "verify-email" : "reset-password";
    const title = purpose === "verify_email" ? "验证你的 DeskLink 邮箱" : "重置 DeskLink 密码";
    const duration = purpose === "verify_email" ? "24 小时" : "30 分钟";
    const url = this.actionUrl(action, token);
    return {
      to: user.email,
      subject: title,
      text: `请在 ${duration}内使用以下链接处理 DeskLink 账号：${url}`,
      html: `<p>请在 ${duration}内处理 DeskLink 账号。</p><p><a href="${url}">${title}</a></p>`,
    };
  }

  private actionUrl(action: "verify-email" | "reset-password", token: string): string {
    return new URL(`/v1/account/${action}?token=${encodeURIComponent(token)}`, `${this.config.publicOrigin}/`).toString();
  }
}

export interface PublicUser {
  id: string;
  email: string;
  emailVerified: boolean;
}

export function publicUser(user: UserRecord): PublicUser {
  return { id: user.id, email: user.email, emailVerified: user.emailVerifiedAt !== null };
}

export function readDeviceInput(input: RegisterInput): {
  id: string;
  platform: AccountPlatform;
  name: string;
} {
  if (typeof input.deviceId !== "string" || !/^[A-Za-z0-9._:-]{8,128}$/.test(input.deviceId)) {
    throw new AccountError("invalid_request", 400, "设备标识无效。 ");
  }
  if (input.platform !== "windows" && input.platform !== "macos" && input.platform !== "ios") {
    throw new AccountError("invalid_request", 400, "设备平台无效。 ");
  }
  const name = typeof input.deviceName === "string" && input.deviceName.trim()
    ? input.deviceName.trim().slice(0, 120)
    : input.platform === "ios" ? "iPhone" : input.platform === "macos" ? "Mac" : "Windows 电脑";
  return { id: input.deviceId, platform: input.platform, name };
}

function readToken(value: unknown, label: string): string {
  if (typeof value !== "string" || value.trim().length < 32 || value.length > 512) {
    throw new AccountError("invalid_request", 400, `${label}无效。`);
  }
  return value.trim();
}
