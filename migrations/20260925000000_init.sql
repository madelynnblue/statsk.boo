-- statsk.boo schema (WFTDA-first).
--
-- The project is deployed against a fresh database, so this one migration is
-- the entire schema: there are no incremental migrations and no legacy tables.
-- Identity is WFTDA-first — `wftda_game_id` is the game, `wftda_team_id` is the
-- team, and a statsbook is an artifact that the matcher attaches to a game.
-- See docs/wftda-first-redesign.md for the rationale and measured behavior.

CREATE EXTENSION IF NOT EXISTS pg_trgm;

-- ============ WFTDA reference (authoritative identity) ============

CREATE TABLE wftda_teams (
    id            INT PRIMARY KEY,              -- WFTDA team id
    title         TEXT NOT NULL,                -- exact, e.g. "Boulder County Roller Derby: Flat Iron Phoenixes"
    short_name    TEXT,                         -- unreliable upstream; display only
    slug          TEXT UNIQUE,                  -- from /teams listing; NULL for defunct stubs
    region        TEXT,
    region_id     INT,
    location      TEXT,
    in_teams_api  BOOL NOT NULL DEFAULT false,  -- false = stub seen only in results
    fetched_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE wftda_games (
    id            INT PRIMARY KEY,              -- -> https://stats.wftda.com/games/{id}
    date          DATE NOT NULL,
    team_1_id     INT NOT NULL REFERENCES wftda_teams(id),
    team_2_id     INT NOT NULL REFERENCES wftda_teams(id),
    team_1_score  INT2 NOT NULL,
    team_2_score  INT2 NOT NULL,
    region        INT,
    seeding_game  BOOL NOT NULL DEFAULT false,
    fetched_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX wftda_games_date_idx   ON wftda_games (date DESC);
CREATE INDEX wftda_games_team_1_idx ON wftda_games (team_1_id);
CREATE INDEX wftda_games_team_2_idx ON wftda_games (team_2_id);

-- ============ Drive artifacts ============

CREATE TABLE statsbooks (
    drive_file_id  TEXT PRIMARY KEY,
    name           TEXT NOT NULL,               -- "[WFTDA]STATS-..."
    quarter        TEXT,                        -- "2026-Q3"
    mime_type      TEXT,                        -- xlsx vs google-sheet
    modified_time  TIMESTAMPTZ,                 -- Drive's exact modifiedTime
    first_seen     TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen      TIMESTAMPTZ NOT NULL DEFAULT now(),
    parse_status   TEXT NOT NULL DEFAULT 'pending'
                   CHECK (parse_status IN ('pending','parsed','failed','skipped')),
    parse_error    TEXT,
    parser_version INT
);
CREATE INDEX statsbooks_status_idx ON statsbooks (parse_status, parser_version);

-- ============ Parsed statsbook content ============

CREATE TABLE statsbook_games (
    drive_file_id  TEXT PRIMARY KEY REFERENCES statsbooks(drive_file_id) ON DELETE CASCADE,
    content_hash   TEXT NOT NULL UNIQUE,        -- dedup/version token only; never a URL or join key
    date           DATE NOT NULL,
    version        TEXT NOT NULL,
    home_score     INT2 NOT NULL,
    away_score     INT2 NOT NULL,
    game_data      JSONB,                       -- full serialized GameData
    -- Searchable fields derived from game_data. Mirror the legacy `games`
    -- generated columns (20260505000000_initial.sql); the parse stage writes
    -- only `game_data`, so these need no code change.
    tournament     TEXT GENERATED ALWAYS AS (game_data->>'tournament')     STORED,
    host_league    TEXT GENERATED ALWAYS AS (game_data->>'host_league')    STORED,
    venue_name     TEXT GENERATED ALWAYS AS (game_data->'venue'->>'name')  STORED,
    venue_city     TEXT GENERATED ALWAYS AS (game_data->'venue'->>'city')  STORED,
    venue_state    TEXT GENERATED ALWAYS AS (game_data->'venue'->>'state') STORED,
    parsed_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- CockroachDB expresses pg_trgm's GIN operator class as an inverted index.
CREATE INVERTED INDEX statsbook_games_tournament_idx ON statsbook_games (tournament gin_trgm_ops);
CREATE INVERTED INDEX statsbook_games_venue_name_idx ON statsbook_games (venue_name gin_trgm_ops);
CREATE INVERTED INDEX statsbook_games_venue_city_idx ON statsbook_games (venue_city gin_trgm_ops);

CREATE TABLE statsbook_sides (
    drive_file_id    TEXT NOT NULL REFERENCES statsbooks(drive_file_id) ON DELETE CASCADE,
    side             TEXT NOT NULL CHECK (side IN ('home','away')),
    wftda_team_id    INT REFERENCES wftda_teams(id),   -- resolved by stage 6; NULL after parse
    league           TEXT,
    team             TEXT,
    color            TEXT,
    league_canonical TEXT NOT NULL,
    team_canonical   TEXT NOT NULL,
    PRIMARY KEY (drive_file_id, side)
);
CREATE INDEX statsbook_sides_team_idx  ON statsbook_sides (wftda_team_id);
CREATE INDEX statsbook_sides_canon_idx ON statsbook_sides (league_canonical, team_canonical);

CREATE TABLE statsbook_skaters (
    drive_file_id  TEXT NOT NULL,
    side           TEXT NOT NULL,
    number         TEXT NOT NULL,               -- keep as text: leading zeros are identity
    name           TEXT NOT NULL,
    name_canonical TEXT NOT NULL,
    PRIMARY KEY (drive_file_id, side, number),
    FOREIGN KEY (drive_file_id, side) REFERENCES statsbook_sides (drive_file_id, side) ON DELETE CASCADE
);
-- CockroachDB expresses the pg_trgm GIN index as an inverted index.
CREATE INVERTED INDEX statsbook_skaters_name_idx ON statsbook_skaters (name gin_trgm_ops);

CREATE TABLE statsbook_summary (
    drive_file_id TEXT NOT NULL REFERENCES statsbook_games(drive_file_id) ON DELETE CASCADE,
    side          TEXT NOT NULL CHECK (side IN ('home','away')),
    stats         JSONB NOT NULL,
    PRIMARY KEY (drive_file_id, side)
);

-- ============ Resolution: statsbook <-> WFTDA game ============

CREATE TABLE game_matches (
    drive_file_id  TEXT PRIMARY KEY REFERENCES statsbooks(drive_file_id) ON DELETE CASCADE,
    wftda_game_id  INT REFERENCES wftda_games(id),   -- NULL = no confident match (hidden)
    method         TEXT NOT NULL,                    -- score|drift|window|pair|none
    team_similarity REAL,                            -- best-orientation mean side similarity
    score_delta    INT2,                             -- sum of abs score differences
    date_offset    INT2,                             -- 0, -1, or +1
    resolved_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX game_matches_wftda_idx ON game_matches (wftda_game_id);
