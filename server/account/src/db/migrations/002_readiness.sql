CREATE TABLE IF NOT EXISTS readiness_probe (
  id INTEGER PRIMARY KEY CHECK (id = 1)
);
INSERT OR IGNORE INTO readiness_probe(id) VALUES (1);
CREATE INDEX IF NOT EXISTS email_outbox_errors
  ON email_outbox(attempts) WHERE sent_at IS NULL AND attempts > 0;
PRAGMA user_version = 2;
