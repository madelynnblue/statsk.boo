CREATE EXTENSION IF NOT EXISTS pg_trgm;

CREATE TABLE IF NOT EXISTS games (
  drive_file_id  TEXT PRIMARY KEY,
  date           DATE,
  ingested_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  data           JSONB NOT NULL,
  player_search   TEXT NOT NULL DEFAULT '',
  team_search     TEXT NOT NULL DEFAULT '',
  parser_version  INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS games_data_idx ON games USING GIN (data);
CREATE INDEX IF NOT EXISTS games_player_search_idx ON games USING GIN (player_search gin_trgm_ops);
CREATE INDEX IF NOT EXISTS games_team_search_idx ON games USING GIN (team_search gin_trgm_ops);
