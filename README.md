# statsk.boo — WFTDA StatsBook Browser

A web application that downloads WFTDA statsbook `.xlsx` files from Google Drive, parses them, and serves searchable player, team, league, and game pages.

## What it does

- Polls a Google Drive folder for statsbook files on a configurable interval
- Parses `.xlsx` statsbooks using [calamine](https://github.com/tafia/calamine)
- Stores structured game data in CockroachDB
- Serves a web UI for browsing games and searching by player, team, or league
- Automatically re-parses all games when the parser version is bumped (reparses read from a local cache, `GAME_DATA_DIR`, instead of re-downloading)

## Prerequisites

- Rust (current stable)
- Docker (for CockroachDB)
- `cargo-watch` for the dev run script: `cargo install cargo-watch`
- A Google **service account** with Drive read access, **or** a local directory of `.xlsx` statsbooks

## Service account setup (Google Drive access)

The app talks to the Google Drive API as a **service account** (OAuth2 JWT bearer grant, `drive.readonly` scope) — no API key.

**Create the service account (one-time, GCP console):**

1. https://console.cloud.google.com/iam-admin/serviceaccounts — pick or create a project.
2. **Create service account** → name it (e.g. `statskboo-ingest`) → create.
3. Keys tab → **Add key → Create new key → JSON** → download the key file.
4. Enable the Drive API on the project: https://console.cloud.google.com/apis/library/drive.googleapis.com
5. No folder sharing is needed — the WFTDA repo folder is public ("anyone with the link"). If requests ever come back 404, share the folder with the service account email (`<name>@<project>.iam.gserviceaccount.com`) in Google Drive as a fallback.

**Where the key file lives:**

- Dev: set `GOOGLE_SERVICE_ACCOUNT_PATH` in `.env` to the downloaded JSON, e.g. `/home/<you>/path/to/wftda-sa.json`.
- Prod (`media-stack/docker-compose.yml`, `statskboo` service): copy the JSON to `media-stack/secrets/wftda-sa.json` (gitignored — never commit it), set `GOOGLE_SERVICE_ACCOUNT_PATH=/data/sa.json`, and mount it read-only: `./secrets/wftda-sa.json:/data/sa.json:ro`. Re-run `docker compose up -d --build statskboo` after placing it.

## Setup

**1. Start CockroachDB:**

```bash
docker run -d --name cockroach -p 26257:26257 -p 8081:8080 \
  cockroachdb/cockroach:latest start-single-node --insecure
docker exec cockroach cockroach sql --insecure -e "CREATE DATABASE statskboo;"
```

**2. Create `.env`:**

```bash
DATABASE_URL=postgresql://root@localhost:26257/statskboo?sslmode=disable
GOOGLE_SERVICE_ACCOUNT_PATH=/path/to/wftda-sa.json
# or, for a local directory of xlsx files instead of Drive:
# INGEST_DIR=/path/to/xlsx/files
```

**3. Run:**

```bash
./run.sh        # sources .env and runs cargo watch
```

## Configuration

All config is via environment variables. Exactly one of `GOOGLE_SERVICE_ACCOUNT_PATH` or `INGEST_DIR` must be set.

| Variable | Default | Description |
|---|---|---|
| `DATABASE_URL` | required | CockroachDB connection string |
| `GOOGLE_SERVICE_ACCOUNT_PATH` | — | Path to the service account JSON key (Drive access) |
| `INGEST_DIR` | — | Local directory of `.xlsx` files (alternative to Drive) |
| `GOOGLE_DRIVE_FOLDER_ID` | `1TC1QUmpIwy9NZX9DBPUPoHjkjFbbzyYr` | Drive folder to poll |
| `GAME_DATA_DIR` | — | Cache dir for raw game `.xlsx` files; reparses read from it instead of re-downloading |
| `INGEST_ENABLED` | `true` | Set to `false` to run the server without touching Drive |
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

Because SQL queries are validated at compile time via `sqlx::query!`, a running CockroachDB instance with the `statskboo` schema applied is required to compile.

For offline builds (e.g. Docker), use the checked-in `.sqlx` query cache:

```bash
SQLX_OFFLINE=true cargo build
```

## Docker

```bash
docker build -t statskboo .
docker run -e DATABASE_URL=... -e GOOGLE_SERVICE_ACCOUNT_PATH=... \
  -v /path/to/wftda-sa.json:/data/sa.json:ro -p 8080:8080 statskboo
```

The Dockerfile uses `SQLX_OFFLINE=true` so no database is needed at image build time.

## One-off backfill from a zip

```bash
cargo run --release --bin backfill_from_zip -- <zip> [--dry-run]
```

Populates `GAME_DATA_DIR` from a Google-exported zip of the folder and re-keys `source='file'` rows to their Drive IDs (requires `GOOGLE_SERVICE_ACCOUNT_PATH` and `GAME_DATA_DIR`).

## Architecture

Single Tokio binary. `main.rs` spawns a background ingest loop that polls for new statsbooks, then starts an Axum web server. Both share `Arc<PgPool>` and `Arc<Config>`.

**Ingest pipeline:**
1. List new/modified files from Drive (or local directory) since last ingest
2. Download and parse each `.xlsx` in parallel (bounded by CPU count)
3. Compute a `canonical_id` (SHA-256 of date + teams + score) to deduplicate across Drive and local sources
4. Write to DB in serialized transactions (CockroachDB serialization retries handled automatically)
5. On startup, re-parse any games stored with an older `parser_version` (from `GAME_DATA_DIR` when populated, else Drive)

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
docker exec cockroach cockroach sql --insecure -d statskboo -e "<sql>"

# Drop and recreate dev database
docker exec cockroach cockroach sql --insecure -e \
  "DROP DATABASE IF EXISTS statskboo; CREATE DATABASE statskboo;"
```
