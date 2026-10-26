import { expect, test } from "bun:test";
import { createAccountApp } from "./app";
import { AccountStore } from "./db/store";
import { DUMMY_PASSWORD_HASH, PasswordWorkBudget } from "./domain/password";

const input = (email: string) => ({ email, password: "synthetic-password", deviceId: "synthetic-device", platform: "windows" });
const request = (path: string, email: string) => new Request(`http://test/v1/account/${path}`, {
  method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(input(email)),
});

test("existing and absent accounts both await one password operation before register/login/recovery responses", async () => {
  for (const route of ["register", "login", "password/forgot", "verify-email/resend"]) {
    for (const present of [false, true]) {
      const store = new AccountStore();
      if (present) store.createUser({ email: "synthetic@example.com", passwordHash: DUMMY_PASSWORD_HASH, now: 1 });
      let release!: () => void;
      const gate = new Promise<void>(resolve => { release = resolve; });
      let entered!: () => void;
      const start = new Promise<void>(resolve => { entered = resolve; });
      let operations = 0;
      const app = createAccountApp({ store, passwords: {
        async hash() { operations++; entered(); await gate; return DUMMY_PASSWORD_HASH; },
        async verify() { operations++; entered(); await gate; return false; },
      } });
      let settled = false;
      const response = app.fetch(request(route, "synthetic@example.com")).then(value => { settled = true; return value; });
      await start;
      expect(settled).toBe(false);
      expect(operations).toBe(1);
      release();
      expect((await response).status).toBe(route === "login" ? 401 : 202);
      await app.close();
    }
  }
});

test("password work has bounded concurrency and queue, rejects overflow and recovers slots after failures", async () => {
  const budget = new PasswordWorkBudget(2, 2);
  let release!: () => void;
  const gate = new Promise<void>(resolve => { release = resolve; });
  let active = 0;
  let peak = 0;
  const tasks = Array.from({ length: 4 }, (_, index) => budget.run(async () => {
    active++; peak = Math.max(peak, active);
    try { await gate; if (index === 0) throw new Error("synthetic failure"); return index; }
    finally { active--; }
  }));
  const settled = Promise.allSettled(tasks);
  await expect(budget.run(async () => 5)).rejects.toMatchObject({ status: 429 });
  expect(peak).toBe(2);
  release();
  expect((await settled).filter(result => result.status === "fulfilled")).toHaveLength(3);
  expect(await budget.run(async () => 6)).toBe(6);
  expect(peak).toBe(2);
});

test("global unauthenticated admission runs before body reading even when peers rotate", async () => {
  const app = createAccountApp();
  try {
    for (let index = 0; index < 120; index++) {
      const response = await app.fetch(new Request("http://test/v1/account/login", { method: "POST", headers: { "content-type": "application/json" }, body: "invalid" }),
        { remoteAddress: `192.0.2.${index + 1}` });
      expect(response.status).toBe(400);
    }
    let reads = 0;
    const stream = new ReadableStream({ pull(controller) { reads++; controller.enqueue(new TextEncoder().encode("invalid")); controller.close(); } }, { highWaterMark: 0 });
    const response = await app.fetch(new Request("http://test/v1/account/login", { method: "POST", headers: { "content-type": "application/json" }, body: stream }));
    expect(response.status).toBe(429);
    expect(reads).toBe(0);
  } finally { await app.close(); }
});

test("oversized and stalled bodies fail before entering password work", async () => {
  let operations = 0;
  const app = createAccountApp({ passwords: {
    async hash() { operations++; return DUMMY_PASSWORD_HASH; }, async verify() { operations++; return false; },
  } });
  try {
    const oversized = new Request("http://test/v1/account/register", { method: "POST", headers: { "content-type": "application/json" }, body: " ".repeat(16 * 1_024 + 1) });
    expect((await app.fetch(oversized)).status).toBe(413);
    const stalled = new Request("http://test/v1/account/login", { method: "POST", headers: { "content-type": "application/json" }, body: new ReadableStream({ pull() {} }, { highWaterMark: 0 }) });
    expect((await app.fetch(stalled)).status).toBe(408);
    expect(operations).toBe(0);
  } finally { await app.close(); }
});
