CREATE EXTENSION IF NOT EXISTS pg_trgm;

CREATE TABLE IF NOT EXISTS games (
  id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  drive_file_id  TEXT NOT NULL UNIQUE,
  date           DATE,
  home_score     SMALLINT,
  away_score     SMALLINT,
  ingested_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  data           JSONB NOT NULL,
  player_search  TEXT NOT NULL DEFAULT '',
  team_search    TEXT NOT NULL DEFAULT ''
);

CREATE INDEX IF NOT EXISTS games_data_idx ON games USING GIN (data);
CREATE INDEX IF NOT EXISTS games_player_search_idx ON games USING GIN (player_search gin_trgm_ops);
CREATE INDEX IF NOT EXISTS games_team_search_idx ON games USING GIN (team_search gin_trgm_ops);
