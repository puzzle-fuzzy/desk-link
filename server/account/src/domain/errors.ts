export type AccountErrorCode =
  | "invalid_request"
  | "not_found"
  | "unauthorized"
  | "forbidden"
  | "conflict"
  | "email_not_verified"
  | "invalid_credentials"
  | "token_invalid"
  | "rate_limited"
  | "internal";

export class AccountError extends Error {
  constructor(
    readonly code: AccountErrorCode,
    readonly status: number,
    message: string,
  ) {
    super(message);
    this.name = "AccountError";
  }
}
