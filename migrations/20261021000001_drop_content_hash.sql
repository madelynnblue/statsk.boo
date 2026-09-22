-- Game identity is `wftda_game_id`, so duplicate-upload suppression belongs at
-- the game level (`resolve::dedup_matched_games`, newest `modified_time` wins),
-- not in a content fingerprint. The hash also missed corrected re-uploads
-- (different scores => different hash), so drop it.
ALTER TABLE statsbook_games DROP COLUMN IF EXISTS content_hash CASCADE;
