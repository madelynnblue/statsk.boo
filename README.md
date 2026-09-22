# statsk.boo — WFTDA StatsBook Browser

A WFTDA-first web app: it syncs WFTDA team and game reference data from
[stats.wftda.com](https://stats.wftda.com), matches WFTDA StatsBook `.xlsx`
files from a public Google Drive folder to those games, and serves searchable
player, team, league, and game pages.

## What it does

- Syncs WFTDA teams and results from `stats.wftda.com` (2023 onward — the
  statsbook era)
- Indexes the public WFTDA Google Drive folder of StatsBook files and parses
  them with [calamine](https://github.com/tafia/calamine)
- Matches each statsbook to its WFTDA game and stores the parsed detail in
  CockroachDB
- Serves a web UI: recent games, game pages with rosters/jams/penalties/Game
  Summary, per-team player career pages, teams, leagues, and search
- Links each game to `https://stats.wftda.com/games/{wftda_game_id}` and each
  team to `https://stats.wftda.com/team/{slug}`; a game with a matched statsbook
  also links to the file on Google Drive
- Re-parses statsbooks whose parser version is stale (the bytes are
  re-downloaded from Drive — there is no local `.xlsx` cache)

## Prerequisites

- Rust (current stable)
- Docker (for CockroachDB)
- `cargo-watch` for the dev run script: `cargo install cargo-watch`
- A Google **service account** with Drive read access (StatsBook discovery and
  download go through the Drive API)

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

`GOOGLE_SERVICE_ACCOUNT_PATH` is optional: without it the app still runs (and
the WFTDA team/game sync still works), but it skips the Drive index/parse/resolve
stages with a warning.

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

# Optional overrides (defaults shown)
# GOOGLE_DRIVE_FOLDER_ID=1TC1QUmpIwy9NZX9DBPUPoHjkjFbbzyYr
# GAME_DATA_DIR=/data/game-data   # raw statsbook byte cache
# WFTDA_SYNC_INTERVAL=24h
# WFTDA_GAMES_LOOKBACK=14d     # incremental games sync watermark
# WFTDA_FULL_REFRESH_INTERVAL=7d
# DRIVE_INDEX_INTERVAL=24h
# WFTDA_SYNC_ENABLED=true     # false/0/no serves the web layer only
# BIND_ADDR=0.0.0.0:8080
```

**3. Run:**

```bash
./run.sh        # sources .env and runs cargo watch
```

Migrations are applied automatically on startup.

## Configuration

All config is via environment variables.

| Variable | Default | Description |
|---|---|---|
| `DATABASE_URL` | required | CockroachDB connection string |
| `GOOGLE_SERVICE_ACCOUNT_PATH` | — | Path to the service account JSON key; optional (Drive stages no-op without it) |
| `GOOGLE_DRIVE_FOLDER_ID` | `1TC1QUmpIwy9NZX9DBPUPoHjkjFbbzyYr` | Drive folder holding the quarterly statsbook folders |
| `GAME_DATA_DIR` | — | Raw statsbook byte cache (`{dir}/{drive_file_id}.xlsx`); with it set, parser-version bumps re-parse from disk instead of re-downloading |
| `PARSE_CONCURRENCY` | all cores | Concurrent download+parse tasks; the CPU parse runs on the blocking pool |
| `WFTDA_SYNC_INTERVAL` | `24h` | How often to sync WFTDA teams + results from stats.wftda.com |
| `WFTDA_GAMES_LOOKBACK` | `14d` | Incremental games sync stops once a page's oldest date is older than `max(known date) - lookback` |
| `WFTDA_FULL_REFRESH_INTERVAL` | `7d` | How often to force a full games scan, catching retroactive corrections and late-added results |
| `DRIVE_INDEX_INTERVAL` | `24h` | How often to index Drive, parse pending statsbooks, and resolve matches |
| `WFTDA_SYNC_ENABLED` | `true` | Master switch for all background sync/parse/resolve tasks |
| `BIND_ADDR` | `0.0.0.0:8080` | HTTP listen address |
| `PORT` | — | If set, overrides the port in the bind address |

Boolean values are false only for `false`, `0`, or `no` (case-insensitive);
durations accept `humantime` format (e.g. `6h`, `30m`).

## How it works

A single Tokio binary runs two independent background loops alongside the Axum
web server:

1. **WFTDA teams** — `GET /api/teams` (plus the `/teams` listing for slugs) into
   `wftda_teams`.
2. **WFTDA games** — `GET /api/results` (date-descending) until the results pass
   before 2023-01-01, into `wftda_games`; teams seen only in results get stub
   rows. Requests are serialized and retried with backoff on rate limits.
3. **Statsbook index** — lists the quarterly folders under
   `GOOGLE_DRIVE_FOLDER_ID` with the service account and upserts one `statsbooks`
   row per Drive file. Files that vanish from Drive are deleted.
4. **Parse** — downloads each pending statsbook (`alt=media` for `.xlsx`, Sheets
   export for Google Sheets), parses it with calamine, and writes
   `statsbook_games`/`statsbook_sides`/`statsbook_skaters`/`statsbook_summary`.
   Duplicate uploads are both stored; resolve keeps the newest per game.
   Unfilled placeholders are skipped.
5. **Resolve** — matches each parsed statsbook to a WFTDA game (exact team
   pair + score first, then a ±1-day score/name window) and assigns each
   statsbook side to a WFTDA team.

A statsbook is shown only once it has matched a WFTDA game; WFTDA games always
have a page, even without a statsbook.

## Web routes

- `GET /` — recent WFTDA games
- `GET /game/{wftda_game_id}` — game page (WFTDA score/teams; statsbook detail when matched)
- `GET /team/{wftda_team_id}` — WFTDA team record and game history
- `GET /player?team=&name=&number=` — player career page, scoped to one WFTDA team
- `GET /search?q=...` — players, teams, and derived leagues
- `GET /leagues` and `GET /league?league=...` — teams grouped by a league name derived from team titles
- `GET /about`, `GET /robots.txt`

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

## Database

CockroachDB, accessed via sqlx. There is a single migration,
`migrations/20260925000000_init.sql`, containing the whole WFTDA-first schema;
it runs automatically on startup. The project assumes a fresh database, so there
are no incremental migrations and no legacy tables.

```bash
# Run SQL directly (COCKROACH_URL points at the service)
cockroach sql --url "$COCKROACH_URL" -e "<sql>"

# Drop and recreate the database
cockroach sql --url "postgresql://root@cockroachdb:26257/defaultdb?sslmode=disable" \
  -e "DROP DATABASE IF EXISTS statskboo; CREATE DATABASE statskboo;"
```
