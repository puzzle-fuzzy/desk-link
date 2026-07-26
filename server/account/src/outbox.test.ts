import { describe, expect, test } from "bun:test";

import { createAccountApp } from "./app";
import { loadAccountConfig } from "./config";
import { AccountStore } from "./db/store";
import { DevMailer } from "./providers/dev-mailer";
import type { MailMessage, Mailer } from "./providers/mailer";

class RetryMailer implements Mailer {
  attempts = 0;
  readonly attempted: MailMessage[] = [];
  readonly messages: MailMessage[] = [];

  async send(message: MailMessage): Promise<void> {
    this.attempts += 1;
    this.attempted.push(message);
    if (this.attempts === 1) throw new Error("provider unavailable");
    this.messages.push(message);
  }
}

describe("email outbox", () => {
  test("keeps registration successful and retries a transient mail failure", async () => {
    const store = new AccountStore();
    const mailer = new RetryMailer();
    let now = 10_000;
    const app = createAccountApp({
      config: loadAccountConfig({ NODE_ENV: "test" }),
      store,
      mailer,
      now: () => now,
    });

    const response = await app.fetch(new Request("http://account.test/v1/account/register", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        email: "retry@example.com",
        password: "correct horse battery",
        deviceId: "ios-retry-001",
        platform: "ios",
        deviceName: "测试 iPhone",
      }),
    }));
    expect(response.status).toBe(202);
    expect(mailer.attempts).toBe(1);
    const firstToken = mailer.attempted[0].text.match(/token=([^\s]+)/)?.[1];
    expect(firstToken).toBeTruthy();
    const storedBeforeRetry = store.database.query("SELECT * FROM email_outbox").all();
    expect(JSON.stringify(storedBeforeRetry)).not.toContain(firstToken!);
    expect(store.listPendingEmails(now + 9)).toHaveLength(0);

    now += 10;
    expect(await app.flushOutbox()).toBe(1);
    expect(mailer.messages).toHaveLength(1);
    const storedAfterRetry = store.database.query("SELECT * FROM action_tokens").all();
    expect(JSON.stringify(storedAfterRetry)).not.toContain(mailer.messages[0].text.match(/token=([^\s]+)/)?.[1]!);
    expect(store.listPendingEmails(now)).toHaveLength(0);
    store.close();
  });

  test("does not run overlapping outbox flushes in one process", async () => {
    const store = new AccountStore();
    const mailer = new DevMailer();
    const app = createAccountApp({
      config: loadAccountConfig({ NODE_ENV: "test" }),
      store,
      mailer,
    });
    const first = app.flushOutbox();
    const second = app.flushOutbox();
    expect(first).toBe(second);
    await first;
    store.close();
  });
});
