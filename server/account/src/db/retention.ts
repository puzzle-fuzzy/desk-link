import type { Database } from "bun:sqlite";

const DAY = 86_400;

export interface AccountHistoryCleanup {
  email_outbox: number;
  action_tokens: number;
  account_sessions: number;
  audit_events: number;
}

/** Run outside request transactions: each table holds its own bounded write lock. */
export function cleanupAccountHistory(database: Database, now: number, limit = 500): AccountHistoryCleanup {
  if (!Number.isSafeInteger(now) || now < 0) throw new RangeError("cleanup time must be a nonnegative integer");
  if (!Number.isSafeInteger(limit) || limit < 1 || limit > 5_000) throw new RangeError("cleanup limit must be between 1 and 5000");
  if (database.inTransaction) throw new Error("history cleanup must run outside a transaction");

  const remove = (sql: string, cutoff: number): number => database.transaction(() =>
    database.query(sql).run(cutoff, limit).changes,
  ).immediate();

  // Delete outbox rows before tokens, so expired unreferenced tokens can be
  // collected in this pass. Never let token ON DELETE CASCADE drop a live job.
  const email_outbox = remove(`
    DELETE FROM email_outbox WHERE rowid IN (
      SELECT rowid FROM email_outbox INDEXED BY email_outbox_retention
      WHERE status IN ('delivered', 'failed') AND finished_at < ?
      ORDER BY finished_at, rowid LIMIT ?
    )
  `, now - 30 * DAY);

  const action_tokens = remove(`
    DELETE FROM action_tokens WHERE rowid IN (
      SELECT token.rowid FROM action_tokens AS token
      WHERE token.expires_at < ? AND NOT EXISTS (
        SELECT 1 FROM email_outbox WHERE action_token_id = token.id
      )
      ORDER BY token.expires_at, token.rowid LIMIT ?
    )
  `, now - 7 * DAY);

  // A rotated token can still revoke its active descendants after its own
  // expiry. Keep every member until the whole family's last expiry plus 30 days.
  // The same SQL statement evaluates that horizon and deletes a bounded slice,
  // preventing intermediate batches from losing the family's replay history.
  const sessionCutoff = now - 30 * DAY;
  const account_sessions = database.transaction(() => database.query(`
    DELETE FROM account_sessions WHERE rowid IN (
      SELECT session.rowid FROM account_sessions AS session
      WHERE session.expires_at < ? AND NOT EXISTS (
        SELECT 1 FROM account_sessions AS family
        WHERE family.family_id = session.family_id AND family.expires_at >= ?
      )
      ORDER BY session.expires_at, session.rowid LIMIT ?
    )
  `).run(sessionCutoff, sessionCutoff, limit).changes).immediate();

  const audit_events = remove(`
    DELETE FROM audit_events WHERE rowid IN (
      SELECT rowid FROM audit_events WHERE created_at < ?
      ORDER BY created_at, rowid LIMIT ?
    )
  `, now - 90 * DAY);

  return { email_outbox, action_tokens, account_sessions, audit_events };
}
