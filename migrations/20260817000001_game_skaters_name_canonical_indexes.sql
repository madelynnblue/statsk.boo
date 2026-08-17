-- The exact canonical (lowercase + alphanumeric skeleton + "(C)" strip) is
-- computed in Rust at ingest; the lower(name) backfill below is a placeholder
-- until the PARSER_VERSION bump re-ingests every game on the next startup.
UPDATE game_skaters SET name_canonical = lower(name);

CREATE INDEX game_skaters_name_canonical_number_idx
    ON game_skaters (name_canonical, number) STORING (name);

-- Superseded by the canonical index: exact (name, number) lookups now use
-- name_canonical.
DROP INDEX IF EXISTS game_skaters_name_number_idx;
