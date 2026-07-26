import { randomUUID } from "node:crypto";

import type { AccountConfig } from "../config";
import { AccountError } from "../domain/errors";
import { createOpaqueToken, hashOpaqueToken } from "../domain/tokens";
import type { AccountStore, CreateSessionInput } from "../db/store";
import type { AccountDevice, AccountSession, UserRecord } from "../db/types";

export interface SessionTokenPair {
  tokenType: "Bearer";
  accessToken: string;
  refreshToken: string;
  accessExpiresAt: number;
  refreshExpiresAt: number;
}

export interface AuthenticatedSession {
  user: UserRecord;
  device: AccountDevice;
  session: AccountSession;
}

export class SessionService {
  constructor(
    private readonly store: AccountStore,
    private readonly config: AccountConfig,
    private readonly now: () => number,
  ) {}

  create(user: UserRecord, device: AccountDevice): SessionTokenPair {
    const now = this.now();
    const access = createOpaqueToken();
    const refresh = createOpaqueToken();
    const accessExpiresAt = now + this.config.accessTokenTtlSeconds;
    const refreshExpiresAt = now + this.config.refreshTokenTtlSeconds;
    this.store.revokeDeviceSessions(user.id, device.id, now);
    this.store.createSession({
      userId: user.id,
      deviceId: device.id,
      familyId: randomUUID(),
      accessTokenHash: access.hash,
      accessExpiresAt,
      refreshTokenHash: refresh.hash,
      now,
      expiresAt: refreshExpiresAt,
    });
    return {
      tokenType: "Bearer",
      accessToken: access.value,
      refreshToken: refresh.value,
      accessExpiresAt,
      refreshExpiresAt,
    };
  }

  authenticate(request: Request): AuthenticatedSession {
    const token = readBearerToken(request.headers.get("authorization"));
    const now = this.now();
    const session = this.store.findSessionByAccessHash(hashOpaqueToken(token), now);
    if (!session) throw new AccountError("token_invalid", 401, "登录状态已失效，请重新登录。 ");
    const user = this.store.findUserById(session.userId);
    const device = this.store.findDevice(session.deviceId);
    if (!user || user.status !== "active" || !device || device.revokedAt !== null) {
      throw new AccountError("token_invalid", 401, "登录状态已失效，请重新登录。 ");
    }
    this.store.touchSession(session.id, now);
    return { user, device, session };
  }

  refresh(refreshToken: string): SessionTokenPair {
    if (!refreshToken.trim() || refreshToken.length > 512) {
      throw new AccountError("token_invalid", 401, "刷新令牌无效，请重新登录。 ");
    }
    const now = this.now();
    const existing = this.store.findSessionByRefreshHash(hashOpaqueToken(refreshToken));
    if (!existing) throw new AccountError("token_invalid", 401, "刷新令牌无效，请重新登录。 ");
    if (existing.rotatedAt !== null) {
      this.store.revokeSessionFamily(existing.familyId, now);
      throw new AccountError("token_invalid", 401, "刷新令牌已重复使用，请重新登录。 ");
    }
    if (existing.revokedAt !== null || existing.expiresAt <= now) {
      throw new AccountError("token_invalid", 401, "刷新令牌已失效，请重新登录。 ");
    }
    const user = this.store.findUserById(existing.userId);
    const device = this.store.findDevice(existing.deviceId);
    if (!user || user.status !== "active" || !device || device.revokedAt !== null) {
      throw new AccountError("token_invalid", 401, "登录状态已失效，请重新登录。 ");
    }

    const access = createOpaqueToken();
    const refresh = createOpaqueToken();
    const accessExpiresAt = now + this.config.accessTokenTtlSeconds;
    const refreshExpiresAt = now + this.config.refreshTokenTtlSeconds;
    const replacement: CreateSessionInput = {
      userId: existing.userId,
      deviceId: existing.deviceId,
      familyId: existing.familyId,
      accessTokenHash: access.hash,
      accessExpiresAt,
      refreshTokenHash: refresh.hash,
      now,
      expiresAt: refreshExpiresAt,
    };
    try {
      this.store.rotateSession(existing.id, replacement, now);
    } catch {
      this.store.revokeSessionFamily(existing.familyId, now);
      throw new AccountError("token_invalid", 401, "刷新令牌已重复使用，请重新登录。 ");
    }
    return {
      tokenType: "Bearer",
      accessToken: access.value,
      refreshToken: refresh.value,
      accessExpiresAt,
      refreshExpiresAt,
    };
  }

  logout(authenticated: AuthenticatedSession): void {
    const now = this.now();
    this.store.revokeSession(authenticated.session.id, now);
    this.store.revokeDevice(authenticated.user.id, authenticated.device.id, now);
    this.store.recordAuditEvent({
      userId: authenticated.user.id,
      deviceId: authenticated.device.id,
      action: "account.logged_out",
      now,
    });
  }
}

export function readBearerToken(header: string | null): string {
  if (!header) throw new AccountError("token_invalid", 401, "请先登录。 ");
  const match = /^Bearer\s+([^\s]+)$/i.exec(header.trim());
  if (!match) throw new AccountError("token_invalid", 401, "登录状态无效，请重新登录。 ");
  return match[1];
}
