import { expect, spyOn, test } from "bun:test";
import { createConnection } from "node:net";
import { createAccountApp } from "./app";
import { AccountStore } from "./db/store";
import { DUMMY_PASSWORD_HASH } from "./domain/password";
import { createAccountRuntime } from "./runtime";

const connectable = (port: number) => new Promise<boolean>(resolve => {
  const socket = createConnection({ host: "127.0.0.1", port });
  socket.once("connect", () => { socket.destroy(); resolve(true); });
  socket.once("error", () => { socket.destroy(); resolve(false); });
});

test("real Bun socket shutdown rejects new connections, drains an accepted request and then closes SQLite once", async () => {
  const store = new AccountStore();
  let release!: () => void;
  let entered!: () => void;
  const gate = new Promise<void>(resolve => { release = resolve; });
  const started = new Promise<void>(resolve => { entered = resolve; });
  const app = createAccountApp({ store, passwords: {
    async hash() { return DUMMY_PASSWORD_HASH; }, async verify() { entered(); await gate; return false; },
  } });
  const close = spyOn(app, "close");
  const runtime = createAccountRuntime(app, { hostname: "127.0.0.1", port: 0 });
  const port = runtime.server.port!;
  const response = fetch(`http://127.0.0.1:${port}/v1/account/login`, { method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ email: "synthetic@example.com", password: "synthetic-password" }) });
  try {
    await started;
    const closing = runtime.shutdown();
    expect(runtime.shutdown()).toBe(closing);
    expect(await connectable(port)).toBe(false);
    expect(close).not.toHaveBeenCalled();
    expect(store.database.query("SELECT 1 AS n").get()).toEqual({ n: 1 });
    release();
    expect((await response).status).toBe(401);
    await closing;
    expect(close).toHaveBeenCalledTimes(1);
    expect(() => store.database.query("SELECT 1").get()).toThrow();
  } finally { release(); await runtime.shutdown(); close.mockRestore(); }
});

test("startup worker rejection is handled and its timer can be stopped", async () => {
  const app = createAccountApp();
  const flush = spyOn(app, "flushOutbox").mockRejectedValue(new Error("synthetic SQLite failure"));
  const log = spyOn(console, "error").mockImplementation(() => {});
  const unhandled: unknown[] = [];
  const listener = (reason: unknown) => { unhandled.push(reason); };
  process.on("unhandledRejection", listener);
  const runtime = createAccountRuntime(app, { hostname: "127.0.0.1", port: 0 });
  try {
    await Bun.sleep(30);
    expect(flush).toHaveBeenCalled();
    expect(log).toHaveBeenCalledWith("account maintenance failed");
    await runtime.shutdown();
    expect(unhandled).toEqual([]);
  } finally {
    await runtime.shutdown(); process.off("unhandledRejection", listener); flush.mockRestore(); log.mockRestore();
  }
});
