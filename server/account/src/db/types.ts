export type AccountPlatform = "windows" | "macos" | "ios";
export type UserStatus = "active" | "disabled";
export type ActionTokenPurpose = "verify_email" | "reset_password";
export type EmailOutboxTemplate = ActionTokenPurpose;

export interface UserRecord {
  id: string;
  email: string;
  passwordHash: string;
  emailVerifiedAt: number | null;
  status: UserStatus;
  createdAt: number;
  updatedAt: number;
  passwordChangedAt: number;
}

export interface AccountDevice {
  id: string;
  userId: string;
  platform: AccountPlatform;
  name: string;
  createdAt: number;
  lastSeenAt: number;
  revokedAt: number | null;
}

export interface AccountSession {
  id: string;
  userId: string;
  deviceId: string;
  familyId: string;
  accessTokenHash: string;
  accessExpiresAt: number;
  refreshTokenHash: string;
  createdAt: number;
  lastSeenAt: number;
  expiresAt: number;
  rotatedAt: number | null;
  revokedAt: number | null;
}

export interface ActionTokenRecord {
  id: string;
  userId: string;
  purpose: ActionTokenPurpose;
  tokenHash: string;
  createdAt: number;
  expiresAt: number;
  consumedAt: number | null;
}

export interface EmailOutboxRecord {
  id: string;
  userId: string;
  recipient: string;
  template: EmailOutboxTemplate;
  actionTokenId: string;
  createdAt: number;
  availableAt: number;
  attempts: number;
  sentAt: number | null;
  lastError: string | null;
}
