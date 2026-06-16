CREATE EXTENSION IF NOT EXISTS pg_trgm;

CREATE TABLE IF NOT EXISTS games (
    id             TEXT PRIMARY KEY,
    source         TEXT NOT NULL CHECK (source IN ('drive', 'file')),
    date           DATE NOT NULL,
    ingested_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    parser_version INTEGER NOT NULL,
    version        TEXT NOT NULL,
    modified_time  TIMESTAMPTZ NOT NULL,
    fingerprint    JSONB NOT NULL,
    canonical_id   TEXT NOT NULL,
    game_data      JSONB CREATE FAMILY heavy,
    home_score     INT2 NOT NULL GENERATED ALWAYS AS (COALESCE((game_data->>'home_score')::INT2, 0)) STORED,
    away_score     INT2 NOT NULL GENERATED ALWAYS AS (COALESCE((game_data->>'away_score')::INT2, 0)) STORED,
    tournament     TEXT          GENERATED ALWAYS AS (game_data->>'tournament') STORED,
    host_league    TEXT          GENERATED ALWAYS AS (game_data->>'host_league') STORED,
    venue_name     TEXT          GENERATED ALWAYS AS (game_data->'venue'->>'name') STORED,
    venue_city     TEXT          GENERATED ALWAYS AS (game_data->'venue'->>'city') STORED,
    venue_state    TEXT          GENERATED ALWAYS AS (game_data->'venue'->>'state') STORED
);

CREATE UNIQUE INDEX IF NOT EXISTS games_canonical_id_idx   ON games (canonical_id) STORING (modified_time);
CREATE        INDEX IF NOT EXISTS games_date_ingested_idx  ON games (date DESC, ingested_at DESC) STORING (canonical_id, home_score, away_score);
CREATE INVERTED INDEX IF NOT EXISTS games_tournament_idx  ON games (tournament  gin_trgm_ops);
CREATE INVERTED INDEX IF NOT EXISTS games_venue_name_idx  ON games (venue_name  gin_trgm_ops);
CREATE INVERTED INDEX IF NOT EXISTS games_venue_city_idx  ON games (venue_city  gin_trgm_ops);

CREATE TABLE IF NOT EXISTS game_sides (
    game_id          TEXT NOT NULL REFERENCES games ON DELETE CASCADE,
    side             TEXT NOT NULL CHECK (side IN ('home', 'away')),
    league           TEXT,
    team             TEXT,
    color            TEXT,
    league_canonical TEXT NOT NULL,
    team_canonical   TEXT NOT NULL,
    PRIMARY KEY (game_id, side)
);

CREATE        INDEX IF NOT EXISTS game_sides_league_canonical_idx      ON game_sides (league_canonical) STORING (league, team, team_canonical);
CREATE        INDEX IF NOT EXISTS game_sides_league_team_canonical_idx ON game_sides (league_canonical, team_canonical) STORING (league, team);
CREATE INVERTED INDEX IF NOT EXISTS game_sides_league_idx              ON game_sides (league gin_trgm_ops);
CREATE INVERTED INDEX IF NOT EXISTS game_sides_team_idx                ON game_sides (team   gin_trgm_ops);

CREATE TABLE IF NOT EXISTS game_skaters (
    game_id TEXT NOT NULL,
    side    TEXT NOT NULL,
    number  TEXT NOT NULL,
    name    TEXT NOT NULL,
    PRIMARY KEY (game_id, side, number),
    FOREIGN KEY (game_id, side) REFERENCES game_sides ON DELETE CASCADE
);

CREATE INVERTED INDEX IF NOT EXISTS game_skaters_name_idx ON game_skaters (name gin_trgm_ops);
CREATE        INDEX IF NOT EXISTS game_skaters_name_number_idx ON game_skaters (name, number);

CREATE TABLE IF NOT EXISTS game_summary (
    game_id TEXT NOT NULL REFERENCES games ON DELETE CASCADE,
    side    TEXT NOT NULL CHECK (side IN ('home', 'away')),
    stats   JSONB NOT NULL,
    PRIMARY KEY (game_id, side)
);
