-- Sessions projection (§7.2 columns plus the Phase 1 shell fields). Rebuilt from events.
CREATE TABLE sessions (
  id                    TEXT PRIMARY KEY,
  task_id               TEXT,
  agent_id              TEXT,
  provider              TEXT,
  provider_session_ref  TEXT,
  machine_id            TEXT,
  cwd                   TEXT,
  channel               TEXT CHECK(channel IN ('structured','pty')),
  shell_kind            TEXT,
  status                TEXT NOT NULL,
  exit_code             INTEGER,
  lock_holder           TEXT,
  created_at            TEXT NOT NULL,
  updated_at            TEXT NOT NULL,
  archived              INTEGER NOT NULL DEFAULT 0
);
