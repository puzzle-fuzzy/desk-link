import { AccountStore } from "./db/store";

// Local operator interface only. No public route can resurrect failed jobs.
const args = process.argv.slice(2);
const path = args[0];
if (!path || path === ":memory:" || !["list-failed", "retry"].includes(args[1]) || (args[1] === "retry" && !args[2])) {
  throw new Error("usage: bun run src/outbox-admin.ts <database-file> list-failed | retry <outbox-id>");
}
if (!(await Bun.file(path).exists())) throw new Error("database file must already exist");
const store = new AccountStore(path);
try {
  if (args[1] === "retry") {
    console.log(JSON.stringify({ requeued: store.requeueFailedEmail(args[2], Math.floor(Date.now() / 1_000)) }));
  } else {
    console.log(JSON.stringify(store.database.query(`SELECT id, status, attempts, created_at, finished_at, last_error
      FROM email_outbox WHERE status = 'failed' ORDER BY finished_at LIMIT 100`).all()));
  }
} finally { store.close(); }
