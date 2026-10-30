-- Preserve existing ciphertext and versions while allowing each account its own record IDs.
CREATE TABLE vault_records_scoped (
  id TEXT NOT NULL,
  user_id TEXT NOT NULL,
  record_type TEXT NOT NULL,
  encrypted_blob TEXT NOT NULL,
  version INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (user_id, id),
  FOREIGN KEY (user_id) REFERENCES users(id)
);
INSERT INTO vault_records_scoped
  SELECT id, user_id, record_type, encrypted_blob, version, updated_at FROM vault_records;
DROP TABLE vault_records;
ALTER TABLE vault_records_scoped RENAME TO vault_records;
CREATE INDEX vault_records_user_version_idx ON vault_records(user_id, version);
