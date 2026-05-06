CREATE EXTENSION IF NOT EXISTS pg_trgm;

CREATE TABLE IF NOT EXISTS games (
    drive_file_id  TEXT PRIMARY KEY,
    date           DATE NOT NULL,
    ingested_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    parser_version INTEGER NOT NULL,
    version        TEXT NOT NULL,
    tournament     TEXT,
    host_league    TEXT,
    venue_name     TEXT,
    venue_city     TEXT,
    venue_state    TEXT,
    periods        JSONB NOT NULL,
    penalties      JSONB NOT NULL,
    modified_time  TIMESTAMPTZ NOT NULL,
    fingerprint    JSONB NOT NULL
);

CREATE TABLE IF NOT EXISTS game_sides (
    drive_file_id TEXT NOT NULL REFERENCES games ON DELETE CASCADE,
    side          TEXT NOT NULL CHECK (side IN ('home','away')),
    league        TEXT,
    team          TEXT,
    color         TEXT,
    PRIMARY KEY (drive_file_id, side)
);

CREATE TABLE IF NOT EXISTS game_skaters (
    drive_file_id TEXT NOT NULL,
    side          TEXT NOT NULL,
    number        TEXT NOT NULL,
    name          TEXT NOT NULL,
    PRIMARY KEY (drive_file_id, side, number),
    FOREIGN KEY (drive_file_id, side) REFERENCES game_sides ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS game_summary (
    drive_file_id TEXT NOT NULL REFERENCES games ON DELETE CASCADE,
    side          TEXT NOT NULL CHECK (side IN ('home','away')),
    stats         JSONB NOT NULL,
    PRIMARY KEY (drive_file_id, side)
);

-- Index page ORDER BY / LIMIT
CREATE INDEX IF NOT EXISTS games_date_ingested_idx ON games (date DESC, ingested_at DESC);

-- Ingest: MAX(ingested_at)
CREATE INDEX IF NOT EXISTS games_ingested_at_idx ON games (ingested_at DESC);

-- Ingest: stale game scan
CREATE INDEX IF NOT EXISTS games_parser_version_idx ON games (parser_version);

-- Dedup: fingerprint lookup. CockroachDB silently accepts a B-tree index on JSONB
-- but does NOT use it for `=` lookups (it falls back to a full scan), so use an
-- inverted index. The dedup query uses `@>` containment so the planner can use it.
CREATE INDEX IF NOT EXISTS games_fingerprint_idx ON games USING GIN (fingerprint);

-- Search: player name ILIKE
CREATE INDEX IF NOT EXISTS game_skaters_name_idx ON game_skaters USING GIN (name gin_trgm_ops);

-- Search: team/league ILIKE
CREATE INDEX IF NOT EXISTS game_sides_league_idx ON game_sides USING GIN (league gin_trgm_ops);
CREATE INDEX IF NOT EXISTS game_sides_team_idx ON game_sides USING GIN (team gin_trgm_ops);

-- Exact team/league lookup (team and league handlers)
CREATE INDEX IF NOT EXISTS game_sides_league_team_idx ON game_sides (league, team);

-- Search: tournament / venue ILIKE
CREATE INDEX IF NOT EXISTS games_tournament_idx ON games USING GIN (tournament gin_trgm_ops);
CREATE INDEX IF NOT EXISTS games_venue_name_idx ON games USING GIN (venue_name gin_trgm_ops);
CREATE INDEX IF NOT EXISTS games_venue_city_idx ON games USING GIN (venue_city gin_trgm_ops);

