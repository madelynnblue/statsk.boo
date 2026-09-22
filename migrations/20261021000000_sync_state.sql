-- Stage-2 games sync state.
--
-- A full refresh (which catches retroactive score corrections and results
-- entered long after the game) runs when this marker is missing or older than
-- `WFTDA_FULL_REFRESH_INTERVAL`; between full refreshes the sync stops at a date
-- watermark (`max(known date) - WFTDA_GAMES_LOOKBACK`), so a startup or daily
-- run fetches only a page or two instead of all ~66 pages of the statsbook era.
CREATE TABLE IF NOT EXISTS sync_state (
    key   TEXT PRIMARY KEY,
    value TIMESTAMPTZ NOT NULL
);
