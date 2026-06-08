CREATE TABLE IF NOT EXISTS users (
  id TEXT PRIMARY KEY,
  email TEXT NOT NULL UNIQUE,
  created_at_ms INTEGER NOT NULL,
  disabled_at_ms INTEGER
);

CREATE TABLE IF NOT EXISTS devices (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  platform TEXT,
  token_hash TEXT NOT NULL UNIQUE,
  created_at_ms INTEGER NOT NULL,
  last_seen_at_ms INTEGER,
  revoked_at_ms INTEGER
);

CREATE TABLE IF NOT EXISTS sessions (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  session_id TEXT NOT NULL,
  title TEXT,
  cwd TEXT,
  provider_name TEXT,
  model TEXT,
  source_dir TEXT,
  relative_path TEXT,
  archived INTEGER NOT NULL DEFAULT 0,
  important INTEGER NOT NULL DEFAULT 0,
  first_seen_at_ms INTEGER NOT NULL,
  last_seen_at_ms INTEGER NOT NULL,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  UNIQUE(user_id, session_id)
);

CREATE TABLE IF NOT EXISTS session_versions (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  session_row_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  session_id TEXT NOT NULL,
  device_id TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
  raw_sha256 TEXT NOT NULL,
  encrypted_sha256 TEXT NOT NULL,
  encrypted_size INTEGER NOT NULL,
  blob_key TEXT NOT NULL,
  relative_path TEXT NOT NULL,
  source_dir TEXT NOT NULL,
  title TEXT,
  cwd TEXT,
  provider_name TEXT,
  model TEXT,
  uploaded_at_ms INTEGER NOT NULL,
  created_at_ms INTEGER NOT NULL,
  UNIQUE(user_id, session_id, raw_sha256)
);

CREATE TABLE IF NOT EXISTS audit_events (
  id TEXT PRIMARY KEY,
  user_id TEXT,
  device_id TEXT,
  event_type TEXT NOT NULL,
  target TEXT,
  created_at_ms INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_devices_user_id ON devices(user_id);
CREATE INDEX IF NOT EXISTS idx_sessions_user_updated ON sessions(user_id, updated_at_ms DESC);
CREATE INDEX IF NOT EXISTS idx_session_versions_user_uploaded ON session_versions(user_id, uploaded_at_ms DESC);
CREATE INDEX IF NOT EXISTS idx_session_versions_session ON session_versions(user_id, session_id, uploaded_at_ms DESC);
