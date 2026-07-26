import { describe, expect, test } from "bun:test";

import { createAccountApp } from "./app";
import { loadAccountConfig } from "./config";

const app = createAccountApp({ config: loadAccountConfig({ NODE_ENV: "test" }) });

describe("account app boundary", () => {
  test("serves a health response without opening a socket", async () => {
    const response = await app.fetch(new Request("http://account.test/health"));
    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({
      schema: 1,
      status: "ok",
      service: "desklink-account",
    });
    expect(response.headers.get("cache-control")).toBe("no-store");
    expect(response.headers.get("x-content-type-options")).toBe("nosniff");
    expect(response.headers.get("x-request-id")).toBeTruthy();
  });

  test("exposes a readiness response after the database has migrated", async () => {
    const response = await app.fetch(new Request("http://account.test/ready"));
    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({
      schema: 1,
      status: "ready",
      service: "desklink-account",
    });
  });

  test("returns stable not found errors", async () => {
    const response = await app.fetch(new Request("http://account.test/missing"));
    expect(response.status).toBe(404);
    expect(await response.json()).toEqual({ error: "not_found", message: "请求地址不存在。" });
  });

  test("rejects unsafe cross-origin requests", async () => {
    const response = await app.fetch(new Request("http://account.test/health", {
      headers: { origin: "https://untrusted.example" },
    }));
    expect(response.headers.get("access-control-allow-origin")).toBeNull();
  });

  test("limits password recovery requests without revealing account existence", async () => {
    const app = createAccountApp({
      config: loadAccountConfig({ NODE_ENV: "test" }),
    });
    for (let index = 0; index < 5; index += 1) {
      const response = await app.fetch(new Request("http://account.test/v1/account/password/forgot", {
        method: "POST",
        headers: {
          ...{"content-type": "application/json"},
          "x-forwarded-for": "rate-limit-test",
        },
        body: JSON.stringify({ email: `unknown-${index}@example.com` }),
      }));
      expect(response.status).toBe(202);
    }
    const limited = await app.fetch(new Request("http://account.test/v1/account/password/forgot", {
      method: "POST",
      headers: {
        ...{"content-type": "application/json"},
        "x-forwarded-for": "rate-limit-test",
      },
      body: JSON.stringify({ email: "unknown-final@example.com" }),
    }));
    expect(limited.status).toBe(429);
    expect((await limited.json()).message).toContain("频繁");
  });
});
