ALTER TABLE games ADD COLUMN IF NOT EXISTS league_search TEXT NOT NULL DEFAULT '';

UPDATE games
SET league_search = COALESCE(data->'home'->>'league', '') || E'\n' || COALESCE(data->'away'->>'league', '')
WHERE league_search = '';

CREATE INDEX IF NOT EXISTS games_league_search_idx ON games USING GIN (league_search gin_trgm_ops);

CREATE INDEX IF NOT EXISTS games_date_ingested_idx ON games (date DESC NULLS LAST, ingested_at DESC);

CREATE INDEX IF NOT EXISTS games_ingested_at_idx ON games (ingested_at DESC);

CREATE INDEX IF NOT EXISTS games_parser_version_idx ON games (parser_version);
