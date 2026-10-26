import type { AccountApp } from "./app";
import { MAX_ACCOUNT_BODY_BYTES } from "./http";
import { AccountWorker } from "./worker";

export function createAccountRuntime(app: AccountApp, options: { hostname: string; port: number }) {
  const server = Bun.serve({
    ...options, maxRequestBodySize: MAX_ACCOUNT_BODY_BYTES,
    fetch(request, instance) { return app.fetch(request, { remoteAddress: instance.requestIP(request)?.address }); },
  });
  const worker = new AccountWorker(app);
  worker.start();
  let stopping: Promise<void> | undefined;
  return {
    server,
    shutdown(): Promise<void> {
      return stopping ??= (async () => {
        // false drains accepted requests. true would abort them before commit.
        await server.stop(false);
        await worker.stop();
      })();
    },
  };
}
