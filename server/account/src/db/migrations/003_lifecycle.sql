ALTER TABLE email_outbox ADD COLUMN status TEXT NOT NULL DEFAULT 'pending'
  CHECK (status IN ('pending', 'delivering', 'delivered', 'failed'));
ALTER TABLE email_outbox ADD COLUMN lease_token TEXT;
ALTER TABLE email_outbox ADD COLUMN lease_until INTEGER;
ALTER TABLE email_outbox ADD COLUMN finished_at INTEGER;
UPDATE email_outbox SET status = CASE WHEN sent_at IS NOT NULL THEN 'delivered'
  WHEN attempts >= 10 THEN 'failed' ELSE 'pending' END,
  finished_at = CASE WHEN sent_at IS NOT NULL THEN sent_at WHEN attempts >= 10 THEN available_at END,
  last_error = CASE WHEN last_error IS NOT NULL THEN 'mail_delivery_failed' END;
DROP INDEX email_outbox_pending;
CREATE INDEX email_outbox_pending ON email_outbox(status, available_at, created_at);
CREATE INDEX email_outbox_leases ON email_outbox(status, lease_until);
CREATE INDEX email_outbox_final_leases ON email_outbox(lease_until)
  WHERE status = 'delivering' AND attempts >= 10;
CREATE INDEX email_outbox_retention ON email_outbox(finished_at) WHERE status IN ('delivered','failed');
CREATE INDEX email_outbox_action_token ON email_outbox(action_token_id);
CREATE INDEX account_sessions_retention ON account_sessions(expires_at);
CREATE INDEX account_sessions_family_retention ON account_sessions(family_id, expires_at);
CREATE INDEX action_tokens_retention ON action_tokens(expires_at);
CREATE INDEX audit_events_retention ON audit_events(created_at);
PRAGMA user_version = 3;
