import type { createDiagnosticsApp } from "./app";
import type { DiagnosticMaintenance } from "./maintenance";
import type { DiagnosticStore } from "./store";
import { MAX_BATCH_BYTES } from "./validation";

export function createDiagnosticsRuntime(store: DiagnosticStore, maintenance: DiagnosticMaintenance,
  app: ReturnType<typeof createDiagnosticsApp>, options: { hostname: string; port: number }) {
  const server = Bun.serve({ ...options, maxRequestBodySize: MAX_BATCH_BYTES,
    fetch(request, instance) { return app.fetch(request, instance.requestIP(request)?.address); },
  });
  maintenance.start();
  let stopping: Promise<void> | undefined;
  return {
    server,
    shutdown(): Promise<void> {
      return stopping ??= (async () => {
        maintenance.stop();
        await server.stop(false);
        store.close();
      })();
    },
  };
}
