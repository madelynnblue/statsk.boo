-- Player identity = (league_canonical, name_canonical, number), so names
-- that differ only by case/punctuation/captain markers collapse into one player.
-- Split from the backfill/index migration: CockroachDB schema changes are not
-- visible to later statements in the same migration transaction.
ALTER TABLE game_skaters ADD COLUMN name_canonical TEXT NOT NULL DEFAULT '';
