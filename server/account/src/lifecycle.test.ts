import { describe, expect, test } from "bun:test";

import { createAccountApp } from "./app";
import { loadAccountConfig } from "./config";
import { AccountStore } from "./db/store";
import { DevMailer } from "./providers/dev-mailer";

const jsonHeaders = { "content-type": "application/json" };

function post(path: string, body: unknown, authorization?: string): Request {
  return new Request(`http://account.test${path}`, {
    method: "POST",
    headers: {
      ...jsonHeaders,
      ...(authorization ? { authorization } : {}),
    },
    body: JSON.stringify(body),
  });
}

describe("account lifecycle", () => {
  test("registers, verifies, logs in and refreshes without putting account data in remote credentials", async () => {
    const store = new AccountStore();
    const mailer = new DevMailer();
    const app = createAccountApp({
      config: loadAccountConfig({ NODE_ENV: "test" }),
      store,
      mailer,
      now: () => 1_000_000,
    });
    const deviceId = "ios-installation-001";

    const registration = await app.fetch(post("/v1/account/register", {
      email: "User@Example.com",
      password: "correct horse battery",
      deviceId,
      platform: "ios",
      deviceName: "我的 iPhone",
    }));
    expect(registration.status).toBe(202);
    expect(mailer.messages).toHaveLength(1);
    expect((await registration.json()).verificationRequired).toBe(true);

    const beforeVerification = await app.fetch(post("/v1/account/login", {
      email: "user@example.com",
      password: "correct horse battery",
      deviceId,
      platform: "ios",
      deviceName: "我的 iPhone",
    }));
    expect(beforeVerification.status).toBe(403);
    expect((await beforeVerification.json()).message).toContain("邮箱");

    const verificationToken = new URL(mailer.messages[0].text.match(/https?:\/\/\S+/)![0]).searchParams.get("token");
    const verification = await app.fetch(post("/v1/account/verify-email", { token: verificationToken }));
    expect(verification.status).toBe(200);

    const login = await app.fetch(post("/v1/account/login", {
      email: "user@example.com",
      password: "correct horse battery",
      deviceId,
      platform: "ios",
      deviceName: "我的 iPhone",
    }));
    expect(login.status).toBe(200);
    const loginBody = await login.json() as { tokens: { accessToken: string; refreshToken: string } };
    expect(loginBody.tokens.accessToken).not.toContain("user@example.com");
    expect(store.findUserByEmail("user@example.com")?.passwordHash).not.toContain("correct horse");

    const refreshed = await app.fetch(post("/v1/account/refresh", { refreshToken: loginBody.tokens.refreshToken }));
    expect(refreshed.status).toBe(200);
    const refreshedBody = await refreshed.json() as { tokens: { accessToken: string; refreshToken: string } };

    const replay = await app.fetch(post("/v1/account/refresh", { refreshToken: loginBody.tokens.refreshToken }));
    expect(replay.status).toBe(401);
    const sessionAfterReplay = await app.fetch(new Request("http://account.test/v1/account/me", {
      headers: { authorization: `Bearer ${refreshedBody.tokens.accessToken}` },
    }));
    expect(sessionAfterReplay.status).toBe(401);
    store.close();
  });

  test("logout revokes this account session and device without revoking remote host trust", async () => {
    const store = new AccountStore();
    const mailer = new DevMailer();
    const app = createAccountApp({
      config: loadAccountConfig({ NODE_ENV: "test" }),
      store,
      mailer,
      now: () => 2_000_000,
    });
    const deviceId = "mac-installation-001";
    await app.fetch(post("/v1/account/register", {
      email: "mac@example.com",
      password: "correct horse battery",
      deviceId,
      platform: "macos",
      deviceName: "Mac",
    }));
    const token = new URL(mailer.messages[0].text.match(/https?:\/\/\S+/)![0]).searchParams.get("token");
    await app.fetch(post("/v1/account/verify-email", { token }));
    const login = await app.fetch(post("/v1/account/login", {
      email: "mac@example.com",
      password: "correct horse battery",
      deviceId,
      platform: "macos",
      deviceName: "Mac",
    }));
    const accessToken = (await login.json() as { tokens: { accessToken: string } }).tokens.accessToken;
    const logout = await app.fetch(post("/v1/account/logout", {}, `Bearer ${accessToken}`));
    expect(logout.status).toBe(204);
    expect((await app.fetch(new Request("http://account.test/v1/account/me", {
      headers: { authorization: `Bearer ${accessToken}` },
    }))).status).toBe(401);
    expect(store.findDevice(deviceId)?.revokedAt).not.toBeNull();
    store.close();
  });

  test("resets a password, revokes old sessions and keeps the response generic", async () => {
    const store = new AccountStore();
    const mailer = new DevMailer();
    const app = createAccountApp({
      config: loadAccountConfig({ NODE_ENV: "test" }),
      store,
      mailer,
      now: () => 3_000_000,
    });
    const deviceId = "ios-reset-001";
    await app.fetch(post("/v1/account/register", {
      email: "reset@example.com",
      password: "old correct horse",
      deviceId,
      platform: "ios",
      deviceName: "测试 iPhone",
    }));
    const verificationToken = new URL(mailer.messages[0].text.match(/https?:\/\/\S+/)![0]).searchParams.get("token");
    await app.fetch(post("/v1/account/verify-email", { token: verificationToken }));
    const forgot = await app.fetch(post("/v1/account/password/forgot", { email: "reset@example.com" }));
    const unknownForgot = await app.fetch(post("/v1/account/password/forgot", { email: "unknown@example.com" }));
    expect(forgot.status).toBe(202);
    expect(unknownForgot.status).toBe(202);
    expect(await forgot.json()).toEqual(await unknownForgot.json());
    const resetToken = new URL(mailer.messages[1].text.match(/https?:\/\/\S+/)![0]).searchParams.get("token");
    const reset = await app.fetch(post("/v1/account/password/reset", {
      token: resetToken,
      password: "new correct horse",
    }));
    expect(reset.status).toBe(204);
    const replay = await app.fetch(post("/v1/account/password/reset", {
      token: resetToken,
      password: "another correct horse",
    }));
    expect(replay.status).toBe(400);

    const oldPassword = await app.fetch(post("/v1/account/login", {
      email: "reset@example.com",
      password: "old correct horse",
      deviceId,
      platform: "ios",
      deviceName: "测试 iPhone",
    }));
    expect(oldPassword.status).toBe(401);
    const newPassword = await app.fetch(post("/v1/account/login", {
      email: "reset@example.com",
      password: "new correct horse",
      deviceId,
      platform: "ios",
      deviceName: "测试 iPhone",
    }));
    expect(newPassword.status).toBe(200);
    store.close();
  });

  test("allows multiple account devices without copying remote connection material", async () => {
    const store = new AccountStore();
    const mailer = new DevMailer();
    const app = createAccountApp({
      config: loadAccountConfig({ NODE_ENV: "test" }),
      store,
      mailer,
      now: () => 4_000_000,
    });
    await app.fetch(post("/v1/account/register", {
      email: "multi@example.com",
      password: "correct horse battery",
      deviceId: "ios-multi-001",
      platform: "ios",
      deviceName: "iPhone",
    }));
    const verificationToken = new URL(mailer.messages[0].text.match(/https?:\/\/\S+/)![0]).searchParams.get("token");
    await app.fetch(post("/v1/account/verify-email", { token: verificationToken }));
    const first = await app.fetch(post("/v1/account/login", {
      email: "multi@example.com",
      password: "correct horse battery",
      deviceId: "ios-multi-001",
      platform: "ios",
      deviceName: "iPhone",
    }));
    const second = await app.fetch(post("/v1/account/login", {
      email: "multi@example.com",
      password: "correct horse battery",
      deviceId: "mac-multi-001",
      platform: "macos",
      deviceName: "Mac",
    }));
    const firstToken = (await first.json() as { tokens: { accessToken: string } }).tokens.accessToken;
    const secondToken = (await second.json() as { tokens: { accessToken: string } }).tokens.accessToken;
    const devices = await app.fetch(new Request("http://account.test/v1/account/devices", {
      headers: { authorization: `Bearer ${secondToken}` },
    }));
    expect((await devices.json()).devices).toHaveLength(2);
    await app.fetch(post("/v1/account/logout", {}, `Bearer ${firstToken}`));
    expect((await app.fetch(new Request("http://account.test/v1/account/me", {
      headers: { authorization: `Bearer ${secondToken}` },
    }))).status).toBe(200);
    store.close();
  });
});
