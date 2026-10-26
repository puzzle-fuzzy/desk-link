import { createAccountApp } from "./app";
import { loadAccountConfig } from "./config";
import { AccountWorker } from "./worker";

const config = loadAccountConfig();
const app = createAccountApp({ config });
const port = Number(process.env.DESKLINK_ACCOUNT_PORT ?? "3412");

if (!Number.isInteger(port) || port < 1 || port > 65_535) {
  throw new Error("DESKLINK_ACCOUNT_PORT is invalid");
}

export const server = Bun.serve({
  hostname: process.env.DESKLINK_ACCOUNT_ADDR ?? "127.0.0.1",
  port,
  fetch(request, server) {
    return app.fetch(request, { remoteAddress: server.requestIP(request)?.address });
  },
});

console.log(`DeskLink account listening on http://${server.hostname}:${server.port}`);
const worker = new AccountWorker(app);
worker.start();
let stopping = false;
async function shutdown() {
  if (stopping) return;
  stopping = true;
  await server.stop();
  await worker.stop();
}
process.once("SIGTERM", shutdown);
process.once("SIGINT", shutdown);
