ALTER TABLE games ADD COLUMN IF NOT EXISTS canonical_id TEXT;
CREATE UNIQUE INDEX IF NOT EXISTS games_canonical_id_idx ON games (canonical_id) WHERE canonical_id IS NOT NULL;
