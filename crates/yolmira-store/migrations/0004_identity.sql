-- Person-aware identity (team mode, decisions.md 2026-10-09): who an event acts for and
-- which agent did it. Older rows stay NULL: the local person, before identity existed.
ALTER TABLE events ADD COLUMN person_id TEXT;
ALTER TABLE events ADD COLUMN agent_id TEXT;
