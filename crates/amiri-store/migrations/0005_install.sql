-- The machine/install an event came from, apart from the person it acts for
-- (decisions.md 2026-10-09). Older rows stay NULL: this install, before the split.
ALTER TABLE events ADD COLUMN install_id TEXT;
