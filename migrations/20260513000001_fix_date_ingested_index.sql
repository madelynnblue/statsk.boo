-- Add pre-computed score columns so home/away scores don't require
-- deserializing the full periods JSONB at query time.
ALTER TABLE games ADD COLUMN IF NOT EXISTS home_score SMALLINT NOT NULL DEFAULT 0;
ALTER TABLE games ADD COLUMN IF NOT EXISTS away_score SMALLINT NOT NULL DEFAULT 0;

-- Backfill scores for existing rows.
UPDATE games SET
  home_score = (
    SELECT COALESCE(SUM((jam->'home'->>'score')::INT2), 0)
    FROM jsonb_array_elements(periods) AS p,
         jsonb_array_elements(p->'jams') AS jam
  ),
  away_score = (
    SELECT COALESCE(SUM((jam->'away'->>'score')::INT2), 0)
    FROM jsonb_array_elements(periods) AS p,
         jsonb_array_elements(p->'jams') AS jam
  );

-- Rebuild the date/ingested index: drop the huge periods STORING clause,
-- add canonical_id and score columns so the home page query is fully covered.
DROP INDEX IF EXISTS games_date_ingested_idx;
CREATE INDEX games_date_ingested_idx ON games (date DESC, ingested_at DESC)
  STORING (canonical_id, home_score, away_score);
