import { AccountError } from "./errors";

export function validatePassword(password: unknown): asserts password is string {
  if (typeof password !== "string" || password.length < 12 || password.length > 128) {
    throw new AccountError("invalid_request", 400, "密码长度需要为 12 到 128 个字符。");
  }
}

export async function hashPassword(password: string): Promise<string> {
  validatePassword(password);
  return Bun.password.hash(password, { algorithm: "argon2id" });
}

export async function verifyPassword(password: string, passwordHash: string): Promise<boolean> {
  if (typeof password !== "string" || typeof passwordHash !== "string") return false;
  return Bun.password.verify(password, passwordHash);
}
