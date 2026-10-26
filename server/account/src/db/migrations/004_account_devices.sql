-- A client-supplied installation label is scoped to its authenticated account.
-- Keep foreign_keys enabled: the new child table references the new parent,
-- and the old child is removed before the old parent inside one transaction.
CREATE TABLE account_devices_v4 (
  id TEXT NOT NULL,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  platform TEXT NOT NULL CHECK (platform IN ('windows', 'macos', 'ios')),
  name TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  last_seen_at INTEGER NOT NULL,
  revoked_at INTEGER,
  PRIMARY KEY (user_id, id)
);

INSERT INTO account_devices_v4
  SELECT id, user_id, platform, name, created_at, last_seen_at, revoked_at FROM account_devices;

-- The old global row may have been reassigned after logout. Preserve every
-- historical session reference without copying the other account's name.
INSERT INTO account_devices_v4 (id, user_id, platform, name, created_at, last_seen_at, revoked_at)
  SELECT session.device_id, session.user_id, COALESCE(device.platform, 'windows'), '历史设备',
    MIN(session.created_at), MAX(session.last_seen_at),
    MAX(COALESCE(session.revoked_at, session.last_seen_at, session.created_at))
  FROM account_sessions AS session
  LEFT JOIN account_devices AS device ON device.id = session.device_id
  WHERE NOT EXISTS (
    SELECT 1 FROM account_devices AS owned
    WHERE owned.id = session.device_id AND owned.user_id = session.user_id
  )
  GROUP BY session.user_id, session.device_id;

CREATE TABLE account_sessions_v4 (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  device_id TEXT NOT NULL,
  family_id TEXT NOT NULL,
  access_token_hash TEXT UNIQUE NOT NULL,
  access_expires_at INTEGER NOT NULL,
  refresh_token_hash TEXT UNIQUE NOT NULL,
  created_at INTEGER NOT NULL,
  last_seen_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  rotated_at INTEGER,
  revoked_at INTEGER,
  FOREIGN KEY (user_id, device_id) REFERENCES account_devices_v4(user_id, id) ON DELETE CASCADE
);

INSERT INTO account_sessions_v4
  SELECT session.id, session.user_id, session.device_id, session.family_id,
    session.access_token_hash, session.access_expires_at, session.refresh_token_hash,
    session.created_at, session.last_seen_at, session.expires_at, session.rotated_at,
    CASE WHEN EXISTS (
      SELECT 1 FROM account_devices AS owned
      WHERE owned.id = session.device_id AND owned.user_id = session.user_id
    ) THEN session.revoked_at
      ELSE COALESCE(session.revoked_at, session.last_seen_at, session.created_at)
    END
  FROM account_sessions AS session;

DROP TABLE account_sessions;
DROP TABLE account_devices;
ALTER TABLE account_devices_v4 RENAME TO account_devices;
ALTER TABLE account_sessions_v4 RENAME TO account_sessions;

CREATE INDEX account_devices_by_user ON account_devices(user_id, revoked_at, last_seen_at);
CREATE INDEX account_sessions_by_user ON account_sessions(user_id, revoked_at, expires_at);
CREATE INDEX account_sessions_retention ON account_sessions(expires_at);
CREATE INDEX account_sessions_family_retention ON account_sessions(family_id, expires_at);
PRAGMA user_version = 4;
