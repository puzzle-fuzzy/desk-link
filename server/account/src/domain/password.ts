import { AccountError } from "./errors";

// Public, non-user dummy value with the same Argon2id parameters as new users.
export const DUMMY_PASSWORD_HASH = "$argon2id$v=19$m=65536,t=2,p=1$490MgwetMHn8bpQky0LpxbYoXx4Rcypb3n8AiKJIvuk$8EwDLyVz0CgzSnhoIFEAkzeWGwPl0NvhZoEaQmTN4E0";

export interface PasswordOperations {
  hash(password: string): Promise<string>;
  verify(password: string, hash: string): Promise<boolean>;
}

export class PasswordWorkBudget {
  private active = 0;
  private readonly waiting: (() => void)[] = [];

  constructor(private readonly parallel = 2, private readonly maximumQueued = 16) {
    if (!Number.isSafeInteger(parallel) || parallel < 1 || parallel > 8
      || !Number.isSafeInteger(maximumQueued) || maximumQueued < 0 || maximumQueued > 64) throw new RangeError("invalid password work budget");
  }

  async run<T>(operation: () => Promise<T>): Promise<T> {
    if (this.active >= this.parallel) {
      if (this.waiting.length >= this.maximumQueued) throw new AccountError("rate_limited", 429, "请求过于频繁，请稍后再试。");
      await new Promise<void>(resolve => this.waiting.push(resolve));
    } else this.active++;
    try { return await operation(); }
    finally {
      const next = this.waiting.shift();
      if (next) next();
      else this.active--;
    }
  }
}

const work = new PasswordWorkBudget();
export const protectedPasswordOperations: PasswordOperations = {
  hash: password => work.run(() => hashPassword(password)),
  verify: (password, hash) => work.run(() => verifyPassword(password, hash)),
};

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
