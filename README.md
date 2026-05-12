# WSB — WFTDA Statsbook Browser

A web application that downloads WFTDA statsbook `.xlsx` files from Google Drive, parses them, and serves searchable player, team, league, and game pages.

## What it does

- Polls a Google Drive folder for statsbook files on a configurable interval
- Parses `.xlsx` statsbooks using [calamine](https://github.com/tafia/calamine)
- Stores structured game data in CockroachDB
- Serves a web UI for browsing games and searching by player, team, or league
- Automatically re-parses all games when the parser version is bumped

## Prerequisites

- Rust (current stable)
- Docker (for CockroachDB)
- `cargo-watch` for the dev run script: `cargo install cargo-watch`
- A Google API key with Drive read access, **or** a local directory of `.xlsx` statsbooks

## Setup

**1. Start CockroachDB:**

```bash
docker run -d --name cockroach -p 26257:26257 -p 8081:8080 \
  cockroachdb/cockroach:latest start-single-node --insecure
docker exec cockroach cockroach sql --insecure -e "CREATE DATABASE wsb;"
```

**2. Create `.env`:**

```bash
DATABASE_URL=postgresql://root@localhost:26257/wsb?sslmode=disable
GOOGLE_API_KEY=your_api_key_here
# or, for a local directory of xlsx files instead of Drive:
# INGEST_DIR=/path/to/xlsx/files
```

**3. Run:**

```bash
./run.sh        # sources .env and runs cargo watch
```

## Configuration

All config is via environment variables. Exactly one of `GOOGLE_API_KEY` or `INGEST_DIR` must be set.

| Variable | Default | Description |
|---|---|---|
| `DATABASE_URL` | required | CockroachDB connection string |
| `GOOGLE_API_KEY` | — | Google API key for Drive access |
| `INGEST_DIR` | — | Local directory of `.xlsx` files (alternative to Drive) |
| `GOOGLE_DRIVE_FOLDER_ID` | `1TC1QUmpIwy9NZX9DBPUPoHjkjFbbzyYr` | Drive folder to poll |
| `BIND_ADDR` | `0.0.0.0:8080` | HTTP listen address |
| `PORT` | — | If set, overrides port in bind addr |
| `INGEST_INTERVAL` | `24h` | How often to poll for new files |
| `INGEST_JITTER` | `1h` | Lookback window subtracted from last ingest time |

## Building

```bash
cargo build              # compile
cargo test               # all tests
cargo test -- test_parse # single test by name
```

Because SQL queries are validated at compile time via `sqlx::query!`, a running CockroachDB instance with the `wsb` schema applied is required to compile.

For offline builds (e.g. Docker), use the checked-in `.sqlx` query cache:

```bash
SQLX_OFFLINE=true cargo build
```

## Docker

```bash
docker build -t wsb .
docker run -e DATABASE_URL=... -e GOOGLE_API_KEY=... -p 8080:8080 wsb
```

The Dockerfile uses `SQLX_OFFLINE=true` so no database is needed at image build time.

## Architecture

Single Tokio binary. `main.rs` spawns a background ingest loop that polls for new statsbooks, then starts an Axum web server. Both share `Arc<PgPool>` and `Arc<Config>`.

**Ingest pipeline:**
1. List new/modified files from Drive (or local directory) since last ingest
2. Download and parse each `.xlsx` in parallel (bounded by CPU count)
3. Compute a `canonical_id` (SHA-256 of date + teams + score) to deduplicate across Drive and local sources
4. Write to DB in serialized transactions (CockroachDB serialization retries handled automatically)
5. On startup, re-parse any games stored with an older `parser_version`

**Identity:**
- Game: `canonical_id` (content hash) — stable across file renames/re-uploads
- Player: `(league, name, number)` triple
- Team: `(league, team)` pair

**Web routes:**
- `GET /` — recent games index
- `GET /search?q=...` — full-text search across players, teams, leagues, tournaments
- `GET /player?league=...&name=...&number=...` — player page
- `GET /team?league=...&team=...` — team page
- `GET /league?league=...` — league page
- `GET /game/{canonical_id}` — game detail page

**Templates** are compiled into the binary at build time via `include_str!` (Minijinja).

## Database

CockroachDB, accessed via sqlx. Migrations run automatically on startup.

```bash
# Run SQL directly
docker exec cockroach cockroach sql --insecure -d wsb -e "<sql>"

# Drop and recreate dev database
docker exec cockroach cockroach sql --insecure -e \
  "DROP DATABASE IF EXISTS wsb; CREATE DATABASE wsb;"
```
