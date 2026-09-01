-- The startup stale-game scan (SELECT id, source, modified_time, canonical_id
-- FROM games WHERE parser_version < $1) needs canonical_id, which was not in
-- the index's STORING columns, so the optimizer fell back to a full scan of the
-- primary index (~400ms on startup). Recreate the index covering it.
DROP INDEX IF EXISTS games_parser_version_idx;
CREATE INDEX games_parser_version_idx
    ON games (parser_version) STORING (source, modified_time, canonical_id);
