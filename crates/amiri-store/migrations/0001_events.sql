-- Append-only event log: the source of truth (ARCHITECTURE D4, §7.2).
CREATE TABLE events (
  seq         INTEGER PRIMARY KEY,
  id          TEXT NOT NULL UNIQUE,
  ts          TEXT NOT NULL,
  kind        TEXT NOT NULL,
  actor       TEXT NOT NULL,
  project_id  TEXT,
  task_id     TEXT,
  session_id  TEXT,
  machine_id  TEXT,
  payload     TEXT NOT NULL,
  redacted    INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX events_kind ON events(kind, seq);
CREATE INDEX events_session ON events(session_id, seq);
