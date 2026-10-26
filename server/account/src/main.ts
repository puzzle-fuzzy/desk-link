import { createAccountApp } from "./app";
import { loadAccountConfig } from "./config";
import { createAccountRuntime } from "./runtime";

const config = loadAccountConfig();
const port = Number(process.env.DESKLINK_ACCOUNT_PORT ?? "3412");

if (!Number.isInteger(port) || port < 1 || port > 65_535) {
  throw new Error("DESKLINK_ACCOUNT_PORT is invalid");
}

const runtime = createAccountRuntime(createAccountApp({ config }), {
  hostname: process.env.DESKLINK_ACCOUNT_ADDR ?? "127.0.0.1",
  port,
});
export const server = runtime.server;

console.log(`DeskLink account listening on http://${server.hostname}:${server.port}`);
function shutdown() {
  void runtime.shutdown().catch(() => {
    console.error("account shutdown failed");
    process.exitCode = 1;
  });
}
process.once("SIGTERM", shutdown);
process.once("SIGINT", shutdown);
