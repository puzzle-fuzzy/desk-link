import { createAccountApp } from "./app";
import { loadAccountConfig } from "./config";

const config = loadAccountConfig();
const app = createAccountApp({ config });
const port = Number(process.env.DESKLINK_ACCOUNT_PORT ?? "3412");

if (!Number.isInteger(port) || port < 1 || port > 65_535) {
  throw new Error("DESKLINK_ACCOUNT_PORT is invalid");
}

export const server = Bun.serve({
  hostname: process.env.DESKLINK_ACCOUNT_ADDR ?? "127.0.0.1",
  port,
  fetch: app.fetch,
});

console.log(`DeskLink account listening on http://${server.hostname}:${server.port}`);
void app.flushOutbox();
setInterval(() => {
  void app.flushOutbox().catch((error) => console.error("account outbox flush failed", error));
}, 30_000);
