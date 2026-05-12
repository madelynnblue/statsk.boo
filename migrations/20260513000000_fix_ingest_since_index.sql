-- Replace MAX(ingested_at) with MAX(modified_time) for the ingest `since` cursor.
-- ingested_at is the DB write time (always today); modified_time is the Drive file's
-- upload timestamp, which is what the Drive API `since` filter actually compares against.

DROP INDEX IF EXISTS games_ingested_at_idx;

-- Ingest: MAX(modified_time)
CREATE INDEX IF NOT EXISTS games_modified_time_idx ON games (modified_time DESC);
