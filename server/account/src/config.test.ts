import { describe, expect, test } from "bun:test";

import { loadAccountConfig } from "./config";

describe("account production configuration", () => {
  test("requires durable storage, HTTPS origin and HTTPS mail delivery", () => {
    expect(() => loadAccountConfig({ NODE_ENV: "production" })).toThrow();
    expect(() => loadAccountConfig({
      NODE_ENV: "production",
      DESKLINK_ACCOUNT_DATABASE: "/data/account.sqlite",
      DESKLINK_ACCOUNT_ORIGIN: "http://account.example.com",
      DESKLINK_ACCOUNT_MAIL_URL: "https://mail.example/send",
      DESKLINK_ACCOUNT_MAIL_TOKEN: "token",
      DESKLINK_ACCOUNT_MAIL_FROM: "DeskLink <noreply@example.com>",
    })).toThrow("HTTPS");
    expect(() => loadAccountConfig({
      NODE_ENV: "production",
      DESKLINK_ACCOUNT_DATABASE: "/data/account.sqlite",
      DESKLINK_ACCOUNT_ORIGIN: "https://account.example.com",
      DESKLINK_ACCOUNT_MAIL_URL: "http://mail.example/send",
      DESKLINK_ACCOUNT_MAIL_TOKEN: "token",
      DESKLINK_ACCOUNT_MAIL_FROM: "DeskLink <noreply@example.com>",
    })).toThrow("HTTPS");
  });
});
