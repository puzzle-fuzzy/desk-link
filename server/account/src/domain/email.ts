import { AccountError } from "./errors";

export function normalizeEmail(value: unknown): string {
  if (typeof value !== "string") {
    throw new AccountError("invalid_request", 400, "邮箱格式不正确。");
  }
  const email = value.trim().toLowerCase();
  if (email.length < 3 || email.length > 320 || !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email)) {
    throw new AccountError("invalid_request", 400, "邮箱格式不正确。");
  }
  return email;
}
