-- Replace the split periods/penalties columns with a single game_data JSONB
-- column that stores the full GameData struct. home_score and away_score
-- become computed columns derived from a simple JSON path, which CockroachDB
-- supports now that the scores are top-level fields rather than a nested
-- aggregation that required set-returning functions.

ALTER TABLE games ADD COLUMN game_data JSONB FAMILY heavy;

-- Rebuild score columns as computed from game_data.
-- The covering index must be dropped first since it STOREs these columns.
DROP INDEX games_date_ingested_idx;
ALTER TABLE games DROP COLUMN home_score;
ALTER TABLE games DROP COLUMN away_score;
ALTER TABLE games ADD COLUMN home_score INT2 NOT NULL GENERATED ALWAYS AS (COALESCE((game_data->>'home_score')::INT2, 0)) STORED;
ALTER TABLE games ADD COLUMN away_score INT2 NOT NULL GENERATED ALWAYS AS (COALESCE((game_data->>'away_score')::INT2, 0)) STORED;
CREATE INDEX games_date_ingested_idx ON games (date DESC, ingested_at DESC) STORING (canonical_id, home_score, away_score);

-- Drop the old split columns; their data now lives in game_data.
ALTER TABLE games DROP COLUMN periods;
ALTER TABLE games DROP COLUMN penalties;
