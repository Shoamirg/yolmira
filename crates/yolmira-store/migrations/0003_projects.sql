-- Project registry (phase1-plan Slice D) and each session's project. Rebuilt from events.
CREATE TABLE projects (
  name        TEXT PRIMARY KEY,
  path        TEXT NOT NULL,
  created_at  TEXT NOT NULL,
  updated_at  TEXT NOT NULL
);
ALTER TABLE sessions ADD COLUMN project TEXT;
