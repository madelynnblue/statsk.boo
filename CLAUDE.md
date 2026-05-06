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

CockroachDB runs locally in a Docker container named `cockroach`. The `cockroach` binary is **not** on the host — always use `docker exec` to reach it:

```bash
# Run SQL (uses .env DATABASE_URL's database name)
docker exec cockroach cockroach sql --insecure -d wsb -e "<sql>"

# Drop and recreate the dev database
docker exec cockroach cockroach sql --insecure -e "DROP DATABASE IF EXISTS wsb; CREATE DATABASE wsb;"

# Recreate DB (reads DATABASE_URL from .env)
source .env && docker exec cockroach cockroach sql --insecure -e "DROP DATABASE IF EXISTS wsb; CREATE DATABASE wsb;"
```

## Architecture

WSB (WFTDA Statsbook Browser) downloads WFTDA statsbook `.xlsx` files from a public Google Drive folder, parses them, stores the data in CockroachDB as JSONB, and serves searchable player/team/game pages.

**Runtime model:** Single Tokio binary. `main.rs` spawns a background ingest loop (`wsb::ingest::ingest_loop`) that polls Google Drive on a configurable interval, then starts the Axum web server. Both share `Arc<PgPool>` and `Arc<Config>`.

**Identity:**
- Player = `(league, name, number)` triple — all three together are the identity
- Team = `(league, team)` pair
- Game = Google Drive `file_id` (the table's primary key)

**Database:** Single `games` table with `drive_file_id TEXT PRIMARY KEY`, `date DATE`, `ingested_at TIMESTAMPTZ`, `data JSONB NOT NULL`, `player_search TEXT`, `team_search TEXT`. GIN indexes on `data` (JSONB containment queries `@>`), and trigram GIN indexes on `player_search` and `team_search` (for `ILIKE` fuzzy search).

**SQL queries:** All use runtime `sqlx::query().bind()` — the project avoids compile-time macros (`query!`, `query_as!`) because they require a live database at build time. Type extraction uses `row.try_get::<Type, _>(col)?`.

**Migrations:** `sqlx::migrate!("./migrations").set_locking(false)` — locking is disabled because CockroachDB doesn't support PostgreSQL advisory locks.

**Parsing:** `ingest/parse.rs` uses calamine 0.26 to read `.xlsx` files. The calamine API uses `Data` enum (not `DataType` — that's a trait in 0.26). Cell values are read by address (e.g., `(0, 0)` for A1).

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
