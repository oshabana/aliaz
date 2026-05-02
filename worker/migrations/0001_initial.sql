CREATE TABLE users (
  id TEXT PRIMARY KEY,
  username TEXT UNIQUE NOT NULL,
  password_hash TEXT NOT NULL,
  created_at INTEGER NOT NULL
);

CREATE TABLE vault_records (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL,
  record_type TEXT NOT NULL,
  encrypted_blob TEXT NOT NULL,
  version INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  FOREIGN KEY (user_id) REFERENCES users(id)
);

CREATE INDEX vault_records_user_version_idx
  ON vault_records(user_id, version);

CREATE TABLE sync_state (
  user_id TEXT PRIMARY KEY,
  latest_version INTEGER NOT NULL,
  FOREIGN KEY (user_id) REFERENCES users(id)
);
