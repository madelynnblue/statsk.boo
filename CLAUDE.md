# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & run

```bash
./run.sh                 # sources .env and runs cargo watch -x "run --bin statskboo"
cargo build              # compile
cargo test               # all tests (unit + integration)
cargo test -- test_parse # single test by name
```

## Database

The `cockroach` binary is installed on the host (`/usr/local/bin/cockroach`) and the server is reached through the `COCKROACH_URL` environment variable (a Postgres-protocol URL, e.g. `postgresql://root@cockroachdb:26257/defaultdb?sslmode=disable`). `docker` may or may not be available; prefer the binary. The dev database is named `statskboo` (from `.env`'s `DATABASE_URL`); the ignored integration tests use a separate scratch database, `statskboo_v2`, and seed it from the checked-in `tests/fixtures/` files (no live-database dependency).

```bash
# Run SQL against a database (COCKROACH_URL points at the service; swap the path)
cockroach sql --url "$COCKROACH_URL" -e "SHOW DATABASES;"
cockroach sql --url "postgresql://root@cockroachdb:26257/statskboo?sslmode=disable" -e "<sql>"

# Recreate the dev database
cockroach sql --url "postgresql://root@cockroachdb:26257/defaultdb?sslmode=disable" \
  -e "DROP DATABASE IF EXISTS statskboo; CREATE DATABASE statskboo;"

# `--format=csv` is handy for piping query results
cockroach sql --url "$COCKROACH_URL" --format=csv -e "SELECT ..."
```

**Migrations run automatically when the app starts** — `src/db.rs::migrate` calls `sqlx::migrate!("./migrations").run(pool)`, so there is no separate migrate command. Create the database, then start the app and it applies `migrations/*.sql`. `set_locking(false)` is required because CockroachDB has no PostgreSQL advisory locks. The schema is `migrations/20260925000000_init.sql` (the whole WFTDA-first schema) plus `migrations/20261021000000_sync_state.sql` (the incremental games-sync marker) and `migrations/20261021000001_drop_content_hash.sql` (game-level dedup replaced the content fingerprint); the project targets a **fresh database**, so there are no legacy tables.

After adding, removing, or changing any `sqlx::query!` call, regenerate the compile-time query metadata:

```bash
source .env && cargo sqlx prepare
```

This updates the `.sqlx/` directory. Commit the result alongside the query change.

## Architecture

statsk.boo (WFTDA StatsBook Browser) is **WFTDA-first**: `stats.wftda.com` is the source of game/team identity and of canonical scores. WFTDA StatsBook `.xlsx` files from a public Google Drive folder are an optional artifact that is attached to a WFTDA game when the matcher resolves one.

**Runtime model:** Single Tokio binary (`src/main.rs`): load `Config` from the environment, connect to CockroachDB, run migrations, then — when `WFTDA_SYNC_ENABLED` (default `true`) — spawn two independent background loops and the Axum web server. Everything shares `Arc<PgPool>` and `Arc<Config>`.

- `ingest::wftda_sync_loop` runs stages 1–2 (WFTDA teams + games) every `WFTDA_SYNC_INTERVAL`.
- `ingest::drive_index_loop` runs stage 3 (Drive statsbook index), stage 4 (parse), and stages 5–6 (resolve) every `DRIVE_INDEX_INTERVAL`. Without a service account it logs a warning and returns, so the server still runs offline.

**Identity:**
- Game = `wftda_game_id` (the id at `https://stats.wftda.com/games/{id}`). Route: `GET /game/{wftda_game_id}`.
- Team = `wftda_team_id`. Route: `GET /team/{wftda_team_id}`.
- Player = `(wftda_team_id, name_canonical, number)` — per-team, because WFTDA has no player entities (decision 10). Route: `GET /player?team=&name=&number=`.
- Statsbook artifact = `statsbooks.drive_file_id`. Duplicate uploads are resolved by game identity: stage 6's `dedup_matched_games` keeps the newest `statsbooks.modified_time` per `wftda_game_id` and hides the rest (`wftda_game_id = NULL`, `method = 'duplicate'`), so there is no content fingerprint.
- League is not an entity: WFTDA has no league table, so the site derives leagues heuristically from team titles (`web/handlers/leagues.rs::derive_league` — the text before the first `:`, else the whole title).

**Database:** Nine tables (the schema plus the games-sync marker):

- `wftda_teams` (`id INT PK`, `title`, `short_name`, `slug UNIQUE`, `region`, `region_id`, `location`, `in_teams_api` — `false` for stub teams seen only in results, `fetched_at`)
- `wftda_games` (`id INT PK`, `date DATE`, `team_1_id`/`team_2_id` FK `wftda_teams`, `team_1_score`/`team_2_score INT2`, `region`, `seeding_game`, `fetched_at`)
- `statsbooks` (`drive_file_id TEXT PK`, `name`, `quarter`, `mime_type`, `modified_time`, `first_seen`, `last_seen`, `parse_status` in `pending|parsed|failed|skipped`, `parse_error`, `parser_version`)
- `statsbook_games` (`drive_file_id PK` FK `statsbooks` `ON DELETE CASCADE`, `date`, `version`, `home_score`/`away_score INT2`, `game_data JSONB` — full serialized `GameData`, `parsed_at`, plus generated `STORED` search columns `tournament`, `host_league`, `venue_name`, `venue_city`, `venue_state` derived from `game_data`)
- `statsbook_sides` (`drive_file_id`, `side`) PK, FK `statsbooks`; `wftda_team_id` nullable FK `wftda_teams` (assigned by stage 6, NULL after parse), `league`, `team`, `color`, `league_canonical`, `team_canonical`
- `statsbook_skaters` (`drive_file_id`, `side`, `number`) PK, FK `statsbook_sides` `ON DELETE CASCADE`; `name`, `name_canonical`
- `statsbook_summary` (`drive_file_id`, `side`) PK, FK `statsbook_games` `ON DELETE CASCADE`; `stats JSONB`
- `game_matches` (`drive_file_id PK` FK `statsbooks` `ON DELETE CASCADE`, `wftda_game_id` nullable FK `wftda_games`, `method` in `score|drift|window|pair|none`, `team_similarity REAL`, `score_delta INT2`, `date_offset INT2`, `resolved_at`)
- `sync_state` (`key TEXT PK`, `value TIMESTAMPTZ`) — one row, `wftda_games_full_sync`, recording the last successful full games scan so subsequent runs can be incremental

Search/proximity indexes are CockroachDB **inverted indexes** (`CREATE INVERTED INDEX ... gin_trgm_ops`), not PostgreSQL's `CREATE INDEX ... USING gin`; the `pg_trgm` extension provides `similarity`. The inverted indexes are `statsbook_skaters_name_idx` plus the tournament/venue search indexes on `statsbook_games` (`statsbook_games_tournament_idx`, `statsbook_games_venue_name_idx`, `statsbook_games_venue_city_idx`). The game matcher itself runs in memory (see below), not through `similarity()`.

**Visibility rule:** a statsbook's derived data is visible only when `game_matches.wftda_game_id IS NOT NULL`. Unmatched statsbooks (unsanctioned games, or games whose WFTDA result is not published yet) are stored and re-evaluated on every resolve run, but contribute nothing to player, team, search, league, or index pages — and there is no `/statsbook/{drive_file_id}` route.

**SQL queries:** All use compile-time `sqlx::query!` macros, which validate SQL against a live database schema at build time. A running CockroachDB with the schema applied is required to compile, unless `SQLX_OFFLINE=true` is set and the checked-in `.sqlx/` cache is complete.

**Ingestion (stages, each idempotent):**
1. `sync_wftda_teams` (`src/ingest/wftda.rs`) — `GET /api/teams` plus the `/teams?page=N` listing for slugs; upserts `wftda_teams` on `id`.
2. `sync_wftda_games` (`src/ingest/wftda.rs`) — `GET /api/results?page=N` (date DESC), stopping at a date watermark. Normally incremental: it scans until a page's oldest date is older than `max(known date) - WFTDA_GAMES_LOOKBACK` (usually 1–3 pages), so a startup does not refetch the whole era. Every `WFTDA_FULL_REFRESH_INTERVAL` (or when `sync_state` has no marker) it does a full scan to 2023-01-01 (the statsbook era, decision 3), catching retroactive score corrections and late-added results. Upserts `wftda_games` and stub-inserts any referenced team into `wftda_teams`. `stats.wftda.com` rate-limits, so requests are serialized behind a semaphore with a fixed 400 ms spacing and exponential backoff on 429/5xx.
3. `sync_statsbook_index` (`src/ingest/statsbook_index.rs`) — lists the quarter folders under `GOOGLE_DRIVE_FOLDER_ID` with the service-account `DriveClient` (`list_tree_with_paths`), upserts `statsbooks` keyed by `drive_file_id`, and deletes rows whose `last_seen` predates the run (removal from Drive is retraction, decision 9).
4. `parse_statsbooks` (`src/ingest/statsbook_parse.rs`) — selects rows with `parse_status IN ('pending','failed') OR parser_version < PARSER_VERSION` and, when `GAME_DATA_DIR` is set, reads `{GAME_DATA_DIR}/{drive_file_id}.xlsx` first (`src/ingest/data_cache.rs`); only a cache miss or a stale entry (cache mtime `<` `statsbooks.modified_time`) downloads bytes (`alt=media` for `.xlsx`, Sheets export for `application/vnd.google-apps.spreadsheet`) and writes them through to the cache. A `PARSER_VERSION` bump therefore re-parses from disk instead of re-downloading. The CPU parse runs on `spawn_blocking`, and up to `PARSE_CONCURRENCY` (default: all available cores) files are downloaded and parsed concurrently. Parses with calamine and writes `statsbook_games`/`statsbook_sides`/`statsbook_skaters`/`statsbook_summary`. Duplicate uploads are both stored and left to stage 6's game-level dedup; unfilled placeholders (`parse::has_no_game_data`) are marked `skipped` and never stored.
5. `resolve_games` + stage 6 (`src/ingest/resolve.rs`) — an in-process, precision-first matcher: signature match (independently resolved team pair + exact score within ±400 days), then a score/name window over `D-1..D+1`. It upserts `game_matches` and assigns `statsbook_sides.wftda_team_id`. The core comparison uses `canon::TrigramSet` (a precomputed, in-memory `pg_trgm`-equivalent Jaccard similarity), so a full pass needs no per-pair database round trips. It then enforces one visible statsbook per matched game (`dedup_matched_games`): the newest `statsbooks.modified_time` wins, and the rest get `wftda_game_id = NULL`, `method = 'duplicate'`, and cleared side assignments.

`resolve` runs after each WFTDA game sync and after each parse batch. There is no separate reconcile/backfill stage and no local-directory ingest mode. The raw-bytes cache is `GAME_DATA_DIR`-backed and keyed by `drive_file_id` (stage 4), so parser bumps re-parse from disk; `statskboo rekey-cache --cache-dir <dir> (--source-db-url <url> | --mapping-csv <path>) [--copy] [--dry-run]` is a one-off subcommand that converts a legacy `{canonical_id}.xlsx` cache to the new naming (it dispatches before config/DB, so it needs no `DATABASE_URL`).

**Parsing:** `src/ingest/parse.rs` uses calamine to read `.xlsx` files. Cell values are read by address (e.g., `(0, 0)` for A1). The calamine API uses the `Data` enum (not `DataType` — that's a trait). Calamine reads **cached** cell values only and does NOT evaluate Excel formulas; `worksheet_formula()` returns `Range<String>` with raw formula text. When the cached Game Summary is blank but jams exist, the parser falls back to `formualizer` (`parse_game_summary_formualizer`) to evaluate the formulas.

**Formula resolution for IGRF cross-sheet references:** Some statsbooks use `=IF(IGRF!B14="","",IGRF!B14)` formulas in the Penalties and Game Summary sheets to auto-populate skater numbers/names from the IGRF roster. When these formulas have no cached value (file saved without calculating), calamine returns empty cells. The parser handles this by:
1. Building an IGRF cell map (`read_igrf_cells`) — maps Excel refs like "B14" to their string values
2. `cell_str_with_formula` — tries cached value first, falls back to parsing the formula string and extracting IGRF cell references
3. `resolve_igrf_formula` — extracts the first `IGRF!` cell reference from a formula string and looks it up in the IGRF map
4. All sheets that may reference IGRF (Penalties, Penalties-Lineups, Game Summary) use `cell_str_with_formula` for number/name columns

Two parser guards carried over from the redesign (see `docs/wftda-first-redesign.md`):
- `has_no_game_data` treats a statsbook with no jams, no penalties, and a 0–0 score as an unfilled placeholder; `store_parsed` marks it `skipped`.
- A cached IGRF point total of `0` on a side whose Score-sheet jam sum is positive is overridden with the jam sum (a stale cached formula), for that side only; IGRF stays authoritative otherwise.

**Web layer:**
- Routes: `GET /` (recent WFTDA games), `GET /search?q=` (players, teams, derived leagues, and tournaments/venues from matched statsbooks), `GET /player?team=&name=&number=`, `GET /team/{wftda_team_id}`, `GET /leagues`, `GET /league?league=`, `GET /game/{wftda_game_id}`, `GET /about`, `GET /robots.txt`
- `AppState` holds `Arc<PgPool>`, `Arc<Environment<'static>>` (Minijinja templates), and `Arc<Cache>` (the in-process brotli response cache from `src/web/cache.rs`; `/search` is never cached)
- Templates are compiled at build time via `include_str!` in `build_template_env()`
- A game page renders WFTDA identity/score always, and rosters/jams/penalties/summary only when a `game_matches` row links a statsbook; it links out to `stats.wftda.com/games/{id}` and (when matched) the Drive file
- Error handling: `AppError` enum (Internal / NotFound) logs internal errors with `tracing::error!` before returning a generic 500 response
- Axum 0.8 uses `{param}` syntax for path captures (not `:param`)

**Config:** `Config::from_env()` reads from environment variables (`.env` at the repo root). It requires `DATABASE_URL`; `GOOGLE_SERVICE_ACCOUNT_PATH` is optional (without it the Drive loop no-ops), `GOOGLE_DRIVE_FOLDER_ID` defaults to the WFTDA public folder, `GAME_DATA_DIR` is optional (the raw-bytes cache directory; unset means always download), `PARSE_CONCURRENCY` defaults to all available cores (read by `parse_concurrency()`, not `Config`), `WFTDA_SYNC_INTERVAL` and `DRIVE_INDEX_INTERVAL` default to `24h`, `WFTDA_GAMES_LOOKBACK` defaults to `14d`, `WFTDA_FULL_REFRESH_INTERVAL` defaults to `7d`, `WFTDA_SYNC_ENABLED` defaults to `true`, and `BIND_ADDR`/`PORT` control the listen address. See README.md "Service account setup".

## Key conventions

- This repo uses **jj** (Jujutsu) for source control, not git directly
- Never run linters
- Keep scores and small integers as `i16` throughout (matches the `INT2` columns)
- Player numbers must preserve leading zeros — `"1"`, `"01"`, and `"001"` are different identities. Always use `String` for player numbers, never integer types. Use `cell_str()` when parsing them from Excel.
- Use `cargo add` to add dependencies (not manual edits to Cargo.toml) so the latest version is always used
- Run `cargo fmt` after modifying Rust code
- **Parser version:** `PARSER_VERSION` in `src/ingest/parse.rs` must be bumped whenever the parsing logic changes. `parse_pending` re-parses any statsbook whose `parser_version` is older, so downstream consumers always see fresh data.
