import type { AccountConfig } from "../config";
import { normalizeEmail } from "../domain/email";
import { AccountError } from "../domain/errors";
import { hashPassword, validatePassword, verifyPassword } from "../domain/password";
import { createOpaqueToken, hashOpaqueToken } from "../domain/tokens";
import type { AccountStore } from "../db/store";
import type { AccountPlatform, ActionTokenPurpose, UserRecord } from "../db/types";
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
    const user = this.store.createUser({ email, passwordHash: await hashPassword(input.password), now });
    this.store.upsertDevice({ ...device, userId: user.id, now });
    this.store.recordAuditEvent({ userId: user.id, deviceId: device.id, action: "account.registered", now });
    await this.sendVerification(user);
    return { verificationRequired: true };
  }

  async resendVerification(emailInput: unknown): Promise<void> {
    const email = normalizeEmail(emailInput);
    const user = this.store.findUserByEmail(email);
    if (!user || user.emailVerifiedAt !== null || user.status !== "active") return;
    this.store.recordAuditEvent({ userId: user.id, action: "account.verification_resent", now: this.now() });
    await this.sendVerification(user);
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
    this.store.recordAuditEvent({ userId: user.id, action: "account.password_reset_requested", now: this.now() });
    await this.sendActionEmail(user, "reset_password");
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
    this.outboxFlushInFlight = this.flushOutboxInternal(limit).finally(() => {
      this.outboxFlushInFlight = undefined;
    });
    return this.outboxFlushInFlight;
  }

  private async flushOutboxInternal(limit: number): Promise<number> {
    let delivered = 0;
    for (const record of this.store.listPendingEmails(this.now(), limit)) {
      const user = this.store.findUserById(record.userId);
      if (!user || user.status !== "active") {
        this.store.markEmailFailed(record, this.now(), "account user is unavailable");
        continue;
      }
      this.store.invalidateActionToken(record.actionTokenId, this.now());
      const token = createOpaqueToken();
      const actionToken = this.store.createActionToken({
        userId: user.id,
        purpose: record.template,
        tokenHash: token.hash,
        now: this.now(),
        expiresAt: this.now() + (record.template === "verify_email" ? 24 * 60 * 60 : 30 * 60),
      });
      this.store.replaceEmailActionToken(record.id, actionToken.id);
      try {
        await this.mailer.send(this.actionEmailMessage(user, record.template, token.value));
        this.store.markEmailSent(record.id, this.now());
        delivered += 1;
      } catch (error) {
        this.store.markEmailFailed(record, this.now(), error instanceof Error ? error.message : "mail delivery failed");
      }
    }
    return delivered;
  }

  private async sendVerification(user: UserRecord): Promise<void> {
    await this.sendActionEmail(user, "verify_email");
  }

  private async sendActionEmail(user: UserRecord, purpose: ActionTokenPurpose): Promise<void> {
    const token = createOpaqueToken();
    const actionToken = this.store.createActionToken({
      userId: user.id,
      purpose,
      tokenHash: token.hash,
      now: this.now(),
      expiresAt: this.now() + (purpose === "verify_email" ? 24 * 60 * 60 : 30 * 60),
    });
    await this.queueEmail(user, purpose, actionToken.id, token.value);
  }

  private async queueEmail(
    user: UserRecord,
    purpose: ActionTokenPurpose,
    actionTokenId: string,
    token: string,
  ): Promise<void> {
    const message = this.actionEmailMessage(user, purpose, token);
    const outbox = this.store.enqueueEmail({
      userId: user.id,
      recipient: user.email,
      template: purpose,
      actionTokenId,
      now: this.now(),
    });
    try {
      await this.mailer.send(message);
      this.store.markEmailSent(outbox.id, this.now());
    } catch (error) {
      this.store.markEmailFailed(outbox, this.now(), error instanceof Error ? error.message : "mail delivery failed");
    }
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
