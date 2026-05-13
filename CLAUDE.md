# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & run

```bash
./run.sh                 # sources .env and runs cargo run
cargo build              # compile
cargo test               # all tests (unit + integration)
cargo test -- test_parse # single test
```

## Database

CockroachDB runs locally in a Docker container named `cockroachdb`. The `cockroach` binary is **not** on the host — always use `docker exec` to reach it:

```bash
# Run SQL (uses .env DATABASE_URL's database name)
docker exec cockroachdb cockroach sql --insecure -d wsb -e "<sql>"

# Drop and recreate the dev database
docker exec cockroachdb cockroach sql --insecure -e "DROP DATABASE IF EXISTS wsb; CREATE DATABASE wsb;"

# Recreate DB (reads DATABASE_URL from .env)
source .env && docker exec cockroachdb cockroach sql --insecure -e "DROP DATABASE IF EXISTS wsb; CREATE DATABASE wsb;"
```

After adding, removing, or changing any `sqlx::query!` call, regenerate the compile-time query metadata:

```bash
source .env && cargo sqlx prepare
```

This updates the `.sqlx/` directory. Commit the result alongside the query change.

## Architecture

WSB (WFTDA Statsbook Browser) downloads WFTDA statsbook `.xlsx` files from a public Google Drive folder, parses them, stores the data in CockroachDB as JSONB, and serves searchable player/team/game pages.

**Runtime model:** Single Tokio binary. `main.rs` spawns a background ingest loop (`wsb::ingest::ingest_loop`) that polls Google Drive on a configurable interval, then starts the Axum web server. Both share `Arc<PgPool>` and `Arc<Config>`.

**Identity:**
- Player = `(league, name, number)` triple — all three together are the identity
- Team = `(league, team)` pair
- Game = unique `id` text PK (Drive file ID for `source='drive'`, relative path for `source='file'`)

**Database:** Relational schema with four tables and GIN indexes for full-text search:
- `games` (`id TEXT PK`, `source TEXT` (`'drive'`|`'file'`), `date`, `ingested_at`, `parser_version`, `version`, `tournament`, `host_league`, `venue_*`, `modified_time`, `fingerprint JSONB`, `canonical_id TEXT`, `home_score INT2` (computed), `away_score INT2` (computed), `game_data JSONB` — full serialized `GameData` struct; `home_score`/`away_score` are derived from `game_data->>'home_score'`/`away_score`)
- `game_sides` (`game_id FK`, `side`, `league`, `team`, `color`, `league_canonical`, `team_canonical`)
- `game_skaters` (`game_id FK`, `side`, `number`, `name`)
- `game_summary` (`game_id FK`, `side`, `stats JSONB`)
- Search uses GIN trigram indexes (`pg_trgm`) on name/league/team/tournament/venue columns — no materialized views

**SQL queries:** All use compile-time `sqlx::query!` macros, which validate SQL against the live database schema at build time. A running CockroachDB instance with the `wsb` schema applied is required to compile.

**Migrations:** `sqlx::migrate!("./migrations").set_locking(false)` — locking is disabled because CockroachDB doesn't support PostgreSQL advisory locks.

**Deduplication:** Each game gets a `canonical_id` — a SHA-256 hash (first 4 bytes, hex) of `(date, home_league_canonical, home_team_canonical, away_league_canonical, away_team_canonical, home_score, away_score)`. This deduplicates across Drive re-uploads and local files representing the same game. If two files share a `canonical_id`, only the one with the newer `modified_time` is kept.

**Parsing:** `ingest/parse.rs` uses calamine to read `.xlsx` files. Cell values are read by address (e.g., `(0, 0)` for A1). The calamine API uses `Data` enum (not `DataType` — that's a trait). Calamine reads **cached** cell values only — it does NOT evaluate Excel formulas. `worksheet_formula()` returns `Range<String>` with raw formula text.

**Formula resolution for IGRF cross-sheet references:** Some statsbooks use `=IF(IGRF!B14="","",IGRF!B14)` formulas in the Penalties and Game Summary sheets to auto-populate skater numbers/names from the IGRF roster. When these formulas have no cached value (file saved without calculating), calamine returns empty cells. The parser handles this by:
1. Building an IGRF cell map (`read_igrf_cells`) — maps Excel refs like "B14" to their string values
2. `cell_str_with_formula` — tries cached value first, falls back to parsing the formula string and extracting IGRF cell references
3. `resolve_igrf_formula` — extracts the first `IGRF!` cell reference from a formula string and looks it up in the IGRF map
4. All sheets that may reference IGRF (Penalties, Penalties-Lineups, Game Summary) use `cell_str_with_formula` for number/name columns

**Web layer:**
- `AppState` holds `Arc<PgPool>` and `Arc<Environment<'static>>` (Minijinja templates)
- Templates are compiled at build time via `include_str!` in `build_template_env()`
- Error handling: `AppError` enum (Internal / NotFound) logs internal errors with `tracing::error!` before returning a generic 500 response
- Axum 0.8 uses `{param}` syntax for path captures (not `:param`)

**Config:** `Config::from_env()` reads from environment variables. `.env` file at repo root with `DATABASE_URL` and `GOOGLE_API_KEY`.

## Key conventions

- This repo uses **jj** (Jujutsu) for source control, not git directly
- Never run linters
- Keep scores and small integers as `i16` throughout (matches DB `SMALLINT`-equivalent values in JSONB)
- Player numbers must preserve leading zeros — `"1"`, `"01"`, and `"001"` are different identities. Always use `String` for player numbers, never integer types. Use `cell_str()` when parsing them from Excel.
- Use `cargo add` to add dependencies (not manual edits to Cargo.toml) so the latest version is always used
- Run `cargo fmt` after modifying Rust code
- **Parser version:** `PARSER_VERSION` in `src/ingest/parse.rs` must be bumped whenever the parsing logic changes. The ingester re-parses all games with an older `parser_version` on startup, so downstream consumers always see fresh data.
