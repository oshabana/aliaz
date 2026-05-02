CREATE TABLE auth_rate_limits (
  key TEXT PRIMARY KEY,
  attempts INTEGER NOT NULL,
  window_start INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);

CREATE INDEX auth_rate_limits_updated_at_idx
  ON auth_rate_limits(updated_at);
