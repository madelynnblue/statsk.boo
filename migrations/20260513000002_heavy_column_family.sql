-- Move periods and penalties into a separate column family so that queries
-- selecting only small columns (scores, dates, canonical_id) do not pay
-- the cost of reading the large JSONB blobs.
--
-- CockroachDB stores each family as a separate KV entry per row, so a
-- JOIN into games that only needs home_score/away_score will skip the
-- heavy family entirely once periods and penalties live there.

ALTER TABLE games ADD COLUMN IF NOT EXISTS periods_new  JSONB CREATE FAMILY heavy;
ALTER TABLE games ADD COLUMN IF NOT EXISTS penalties_new JSONB FAMILY heavy;

UPDATE games SET periods_new = periods, penalties_new = penalties;

ALTER TABLE games DROP COLUMN IF EXISTS periods;
ALTER TABLE games DROP COLUMN IF EXISTS penalties;

ALTER TABLE games RENAME COLUMN periods_new  TO periods;
ALTER TABLE games RENAME COLUMN penalties_new TO penalties;

ALTER TABLE games ALTER COLUMN periods   SET NOT NULL;
ALTER TABLE games ALTER COLUMN penalties SET NOT NULL;
