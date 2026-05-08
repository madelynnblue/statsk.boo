CREATE EXTENSION IF NOT EXISTS pg_trgm;

CREATE TABLE IF NOT EXISTS games (
    id             TEXT PRIMARY KEY,
    source         TEXT NOT NULL CHECK (source IN ('drive', 'file')),
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
    game_id        TEXT NOT NULL REFERENCES games ON DELETE CASCADE,
    side           TEXT NOT NULL CHECK (side IN ('home','away')),
    league         TEXT,
    team           TEXT,
    color          TEXT,
    league_canonical TEXT NOT NULL,
    team_canonical   TEXT NOT NULL,
    PRIMARY KEY (game_id, side)
);

-- Exact team/league canonical lookup + search dedup
CREATE INDEX IF NOT EXISTS game_sides_league_canonical_idx ON game_sides (league_canonical);
CREATE INDEX IF NOT EXISTS game_sides_league_team_canonical_idx ON game_sides (league_canonical, team_canonical);


CREATE TABLE IF NOT EXISTS game_skaters (
    game_id TEXT NOT NULL,
    side    TEXT NOT NULL,
    number  TEXT NOT NULL,
    name    TEXT NOT NULL,
    PRIMARY KEY (game_id, side, number),
    FOREIGN KEY (game_id, side) REFERENCES game_sides ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS game_summary (
    game_id TEXT NOT NULL REFERENCES games ON DELETE CASCADE,
    side    TEXT NOT NULL CHECK (side IN ('home','away')),
    stats   JSONB NOT NULL,
    PRIMARY KEY (game_id, side)
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

-- Search: tournament / venue ILIKE
CREATE INDEX IF NOT EXISTS games_tournament_idx ON games USING GIN (tournament gin_trgm_ops);
CREATE INDEX IF NOT EXISTS games_venue_name_idx ON games USING GIN (venue_name gin_trgm_ops);
CREATE INDEX IF NOT EXISTS games_venue_city_idx ON games USING GIN (venue_city gin_trgm_ops);
