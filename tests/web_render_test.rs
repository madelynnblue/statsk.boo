//! Router-level render verification for the WFTDA-first web layer.
//!
//! The test is self-contained: it seeds a migrated `statskboo_v2` from the
//! checked-in `tests/fixtures/standbys-flatiron.xlsx` fixture and a handful of
//! synthetic rows, then exercises the real router (including the cache
//! middleware) through `tower::ServiceExt::oneshot`. It never reads the legacy
//! live database.
//!
//! The seed mirrors what the pipeline stages would write: `store_parsed`
//! (stage 4) fills `statsbooks` + `statsbook_games`/`_sides`/`_skaters`/`_summary`,
//! and `resolve_games` (stages 5-6) links the artifact to a `wftda_games` row
//! and assigns `statsbook_sides.wftda_team_id`.
//!
//! The synthetic rows use fixed sentinel ids — `drive_file_id` values prefixed
//! `__web_render_test__` and negative integer ids for `wftda_teams`/`wftda_games`
//! that cannot collide with WFTDA's positive ids. They are deleted at the start
//! and end of the run, so `statskboo_v2` is left unchanged apart from its
//! pre-existing `wftda_teams`/`wftda_games` rows.
//!
//! The two synthetic teams deliberately share the fixture's real titles so the
//! matcher resolves the statsbook sides to them; their negative ids sort first,
//! so they win the tie against any real team with the same title.
//!
//! Ignored by default (needs a migrated `statskboo_v2`). Run with:
//!
//! ```text
//! DATABASE_URL=postgresql://root@cockroachdb:26257/statskboo_v2?sslmode=disable \
//!   SQLX_OFFLINE=false cargo test --test web_render_test -- --ignored --nocapture
//! ```

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{NaiveDate, Utc};
use sqlx::PgPool;
use statskboo::cache::Cache;
use statskboo::canon::canonicalize_league;
use statskboo::ingest::resolve::resolve_games;
use statskboo::ingest::statsbook_parse::{ParseOutcome, store_parsed};
use statskboo::web::handlers::leagues::derive_league;
use statskboo::web::{AppState, build_router, build_template_env};
use tower::ServiceExt;

/// The single statsbook fixture this test seeds.
const FIXTURE: &str = "tests/fixtures/standbys-flatiron.xlsx";
const DRIVE_FILE_ID: &str = "__web_render_test__standbys_flatiron";

/// Synthetic statsbooks seeded directly (no fixture, no Drive) to exercise the
/// tournament/venue generated columns and their search visibility rule. They
/// carry the same tournament string; only [`SEARCH_DRIVE_ID`] is linked to a
/// WFTDA game, while [`SEARCH_HIDDEN_DRIVE_ID`] stays unmatched (hidden).
const SEARCH_DRIVE_ID: &str = "__web_render_test__search_matched";
const SEARCH_HIDDEN_DRIVE_ID: &str = "__web_render_test__search_hidden";
const SEARCH_DRIVE_IDS: [&str; 2] = [SEARCH_DRIVE_ID, SEARCH_HIDDEN_DRIVE_ID];
const SEARCH_TOURNAMENT: &str = "Web Render Test Invitational";
const SEARCH_VENUE: &str = "Web Render Test Arena";
const SEARCH_VENUE_CITY: &str = "Testville";
const SEARCH_VENUE_STATE: &str = "TS";
const SEARCH_HIDDEN_VENUE: &str = "Web Render Test Hidden Arena";

/// Synthetic WFTDA team ids. Negative so they can never collide with a real
/// WFTDA id and so they sort first when the matcher resolves the fixture's side
/// names against duplicate real teams.
const TEAM_HOME_ID: i64 = -900001;
const TEAM_AWAY_ID: i64 = -900002;
const TEAM_UNMATCHED_1_ID: i64 = -900003;
const TEAM_UNMATCHED_2_ID: i64 = -900004;
const TEAM_IDS: [i64; 4] = [
    TEAM_HOME_ID,
    TEAM_AWAY_ID,
    TEAM_UNMATCHED_1_ID,
    TEAM_UNMATCHED_2_ID,
];

const TEAM_HOME_SLUG: &str = "__web_render_test__standbys";
const TEAM_AWAY_SLUG: &str = "__web_render_test__flatiron";
const TEAM_UNMATCHED_1_SLUG: &str = "__web_render_test__unmatched_alpha";
const TEAM_UNMATCHED_2_SLUG: &str = "__web_render_test__unmatched_beta";

/// Titles that canonicalize to the fixture's side names, so the matcher links
/// the statsbook to [`GAME_MATCHED_ID`].
const TEAM_HOME_TITLE: &str = "Denver Roller Derby: Standbys";
const TEAM_AWAY_TITLE: &str = "Boulder County Roller Derby: Flat Iron Phoenixes";
const TEAM_UNMATCHED_1_TITLE: &str = "Web Render Test Unmatched: Alpha";
const TEAM_UNMATCHED_2_TITLE: &str = "Web Render Test Unmatched: Beta";

/// Synthetic WFTDA games; the second deliberately has no matching statsbook.
const GAME_MATCHED_ID: i64 = -910001;
const GAME_UNMATCHED_ID: i64 = -910002;
/// Games for the directly-seeded search statsbooks: one visible, one whose only
/// statsbook is hidden.
const GAME_SEARCH_ID: i64 = -910003;
const GAME_SEARCH_HIDDEN_ID: i64 = -910004;
const GAME_IDS: [i64; 4] = [
    GAME_MATCHED_ID,
    GAME_UNMATCHED_ID,
    GAME_SEARCH_ID,
    GAME_SEARCH_HIDDEN_ID,
];

const FIXTURE_DATE: (i32, u32, u32) = (2026, 6, 6);
const FIXTURE_SCORE: (i16, i16) = (202, 88);

fn db_url() -> String {
    std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        "postgresql://root@cockroachdb:26257/statskboo_v2?sslmode=disable".into()
    })
}

/// Deletes every synthetic row. `statsbooks` cascades to `statsbook_games`,
/// `statsbook_sides`, `statsbook_skaters`, `statsbook_summary`, and
/// `game_matches`, so the following deletes are unblocked.
async fn cleanup(pool: &PgPool) {
    for drive_file_id in std::iter::once(DRIVE_FILE_ID).chain(SEARCH_DRIVE_IDS) {
        sqlx::query("DELETE FROM statsbooks WHERE drive_file_id = $1")
            .bind(drive_file_id)
            .execute(pool)
            .await
            .unwrap();
    }
    sqlx::query("DELETE FROM wftda_games WHERE id = ANY($1)")
        .bind(&GAME_IDS[..])
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM wftda_teams WHERE id = ANY($1)")
        .bind(&TEAM_IDS[..])
        .execute(pool)
        .await
        .unwrap();
}

async fn insert_statsbook(pool: &PgPool) {
    sqlx::query(
        r#"INSERT INTO statsbooks
             (drive_file_id, name, quarter, mime_type, modified_time,
              first_seen, last_seen, parse_status, parser_version)
           VALUES ($1, $2, 'test', NULL, $3, now(), now(), 'pending', NULL)"#,
    )
    .bind(DRIVE_FILE_ID)
    .bind(format!("{DRIVE_FILE_ID}.xlsx"))
    .bind(Utc::now())
    .execute(pool)
    .await
    .unwrap();
}

async fn insert_team(pool: &PgPool, id: i64, title: &str, slug: &str) {
    sqlx::query(
        r#"INSERT INTO wftda_teams (id, title, slug, in_teams_api)
           VALUES ($1, $2, $3, true)"#,
    )
    .bind(id)
    .bind(title)
    .bind(slug)
    .execute(pool)
    .await
    .unwrap();
}

async fn insert_game(
    pool: &PgPool,
    id: i64,
    date: NaiveDate,
    team_1_id: i64,
    team_2_id: i64,
    team_1_score: i16,
    team_2_score: i16,
) {
    sqlx::query(
        r#"INSERT INTO wftda_games
             (id, date, team_1_id, team_2_id, team_1_score, team_2_score)
           VALUES ($1, $2, $3, $4, $5, $6)"#,
    )
    .bind(id)
    .bind(date)
    .bind(team_1_id)
    .bind(team_2_id)
    .bind(team_1_score)
    .bind(team_2_score)
    .execute(pool)
    .await
    .unwrap();
}

/// Inserts a `statsbooks` + `statsbook_games` pair directly, without Drive or
/// the parser, so the `game_data`-derived tournament/venue columns can be
/// exercised against a value that is unique to this test.
async fn insert_synthetic_search_statsbook(
    pool: &PgPool,
    drive_file_id: &str,
    date: NaiveDate,
    tournament: &str,
    venue_name: &str,
    venue_city: &str,
    venue_state: &str,
) {
    sqlx::query(
        r#"INSERT INTO statsbooks
             (drive_file_id, name, quarter, mime_type, modified_time,
              first_seen, last_seen, parse_status, parser_version)
           VALUES ($1, $2, 'test', NULL, now(), now(), now(), 'parsed', 0)"#,
    )
    .bind(drive_file_id)
    .bind(format!("{drive_file_id}.xlsx"))
    .execute(pool)
    .await
    .unwrap();

    let game_data = serde_json::json!({
        "version": "test",
        "venue": { "name": venue_name, "city": venue_city, "state": venue_state },
        "tournament": tournament,
        "host_league": "Web Render Test League",
        "home": { "league": TEAM_HOME_TITLE, "team": null, "color": null, "skaters": [] },
        "away": { "league": TEAM_AWAY_TITLE, "team": null, "color": null, "skaters": [] },
        "periods": [],
        "penalties": [],
        "game_summary": null,
        "home_score": 0,
        "away_score": 0
    });

    sqlx::query(
        r#"INSERT INTO statsbook_games
             (drive_file_id, date, version, home_score, away_score, game_data)
           VALUES ($1, $2, 'test', 0, 0, $3)"#,
    )
    .bind(drive_file_id)
    .bind(date)
    .bind(game_data)
    .execute(pool)
    .await
    .unwrap();
}

async fn insert_match(
    pool: &PgPool,
    drive_file_id: &str,
    wftda_game_id: Option<i64>,
    method: &str,
) {
    sqlx::query(
        r#"INSERT INTO game_matches (drive_file_id, wftda_game_id, method)
           VALUES ($1, $2, $3)"#,
    )
    .bind(drive_file_id)
    .bind(wftda_game_id)
    .bind(method)
    .execute(pool)
    .await
    .unwrap();
}

async fn get(app: &Router, uri: &str) -> (StatusCode, String) {
    let response = app
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn assert_contains(body: &str, needle: &str, context: &str) {
    assert!(
        body.contains(needle),
        "{context}: expected page to contain {needle:?}"
    );
}

fn assert_not_contains(body: &str, needle: &str, context: &str) {
    assert!(
        !body.contains(needle),
        "{context}: expected page NOT to contain {needle:?}"
    );
}

/// Percent-encodes a query-string component (RFC 3986 unreserved set).
fn encode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[tokio::test]
#[ignore = "requires a migrated statskboo_v2; run with --ignored"]
async fn web_pages_render_against_fixture_seed() {
    let pool = PgPool::connect(&db_url()).await.unwrap();

    cleanup(&pool).await;

    // Snapshot the pre-existing reference tables so we can prove the test puts
    // them back exactly as it found them.
    let teams_before: i64 = sqlx::query_scalar("SELECT count(*)::INT8 FROM wftda_teams")
        .fetch_one(&pool)
        .await
        .unwrap();
    let games_before: i64 = sqlx::query_scalar("SELECT count(*)::INT8 FROM wftda_games")
        .fetch_one(&pool)
        .await
        .unwrap();

    // ---- Stage 4: parse + store the checked-in fixture --------------------
    let bytes = std::fs::read(FIXTURE).unwrap_or_else(|e| panic!("read {FIXTURE}: {e}"));
    insert_statsbook(&pool).await;
    let outcome = store_parsed(&pool, DRIVE_FILE_ID, FIXTURE, &bytes)
        .await
        .unwrap();
    assert!(
        matches!(outcome, ParseOutcome::Parsed { .. }),
        "fixture must parse and store: {outcome:?}"
    );

    let (parsed_date, home_score, away_score): (NaiveDate, i16, i16) = sqlx::query_as(
        "SELECT date, home_score, away_score FROM statsbook_games WHERE drive_file_id = $1",
    )
    .bind(DRIVE_FILE_ID)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        parsed_date,
        NaiveDate::from_ymd_opt(FIXTURE_DATE.0, FIXTURE_DATE.1, FIXTURE_DATE.2).unwrap(),
        "fixture date changed"
    );
    assert_eq!(
        (home_score, away_score),
        FIXTURE_SCORE,
        "fixture score changed"
    );
    println!(
        "store_parsed: date={parsed_date} score={home_score}-{away_score} outcome={outcome:?}"
    );

    // ---- Synthetic WFTDA reference rows -----------------------------------
    insert_team(&pool, TEAM_HOME_ID, TEAM_HOME_TITLE, TEAM_HOME_SLUG).await;
    insert_team(&pool, TEAM_AWAY_ID, TEAM_AWAY_TITLE, TEAM_AWAY_SLUG).await;
    insert_team(
        &pool,
        TEAM_UNMATCHED_1_ID,
        TEAM_UNMATCHED_1_TITLE,
        TEAM_UNMATCHED_1_SLUG,
    )
    .await;
    insert_team(
        &pool,
        TEAM_UNMATCHED_2_ID,
        TEAM_UNMATCHED_2_TITLE,
        TEAM_UNMATCHED_2_SLUG,
    )
    .await;

    // The matched game must line up with the fixture's date and score so the
    // matcher links the statsbook to it via the signature path.
    insert_game(
        &pool,
        GAME_MATCHED_ID,
        parsed_date,
        TEAM_HOME_ID,
        TEAM_AWAY_ID,
        home_score,
        away_score,
    )
    .await;
    // A game with no statsbook exercises the score-only page. Its teams' titles
    // are unrelated to any statsbook side, so it can never match.
    insert_game(
        &pool,
        GAME_UNMATCHED_ID,
        NaiveDate::from_ymd_opt(2001, 1, 1).unwrap(),
        TEAM_UNMATCHED_1_ID,
        TEAM_UNMATCHED_2_ID,
        100,
        80,
    )
    .await;

    // ---- Stages 5-6: match and assign sides -------------------------------
    let stats = resolve_games(&pool).await.unwrap();
    println!("resolve: {}", stats.breakdown());
    assert_eq!(stats.total(), 1, "only the fixture statsbook is resolved");
    assert_eq!(
        stats.pair, 1,
        "the fixture must match the synthetic game via the signature path"
    );
    assert_eq!(stats.none, 0, "the fixture must not stay hidden");

    let (matched_game_id, matched_drive_id): (i64, String) = sqlx::query_as(
        "SELECT wftda_game_id, drive_file_id FROM game_matches WHERE wftda_game_id IS NOT NULL",
    )
    .fetch_one(&pool)
    .await
    .expect("a matched game");
    assert_eq!(matched_game_id, GAME_MATCHED_ID);
    assert_eq!(matched_drive_id, DRIVE_FILE_ID);

    let unmatched_matches: i64 =
        sqlx::query_scalar("SELECT count(*)::INT8 FROM game_matches WHERE wftda_game_id = $1")
            .bind(GAME_UNMATCHED_ID)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(unmatched_matches, 0, "the second game has no statsbook");

    // ---- Directly-seeded tournament/venue search rows ---------------------
    // Two statsbooks share [`SEARCH_TOURNAMENT`]; only the first is matched, so
    // only its game may appear in search results. Both are seeded after
    // `resolve_games` and carry no `statsbook_sides`, so the matcher ignores
    // them (`stats.total()` above stays 1).
    insert_game(
        &pool,
        GAME_SEARCH_ID,
        NaiveDate::from_ymd_opt(2026, 7, 4).unwrap(),
        TEAM_HOME_ID,
        TEAM_AWAY_ID,
        111,
        99,
    )
    .await;
    insert_game(
        &pool,
        GAME_SEARCH_HIDDEN_ID,
        NaiveDate::from_ymd_opt(2026, 7, 4).unwrap(),
        TEAM_HOME_ID,
        TEAM_AWAY_ID,
        50,
        40,
    )
    .await;
    insert_synthetic_search_statsbook(
        &pool,
        SEARCH_DRIVE_ID,
        NaiveDate::from_ymd_opt(2026, 7, 4).unwrap(),
        SEARCH_TOURNAMENT,
        SEARCH_VENUE,
        SEARCH_VENUE_CITY,
        SEARCH_VENUE_STATE,
    )
    .await;
    insert_synthetic_search_statsbook(
        &pool,
        SEARCH_HIDDEN_DRIVE_ID,
        NaiveDate::from_ymd_opt(2026, 7, 4).unwrap(),
        SEARCH_TOURNAMENT,
        SEARCH_HIDDEN_VENUE,
        SEARCH_VENUE_CITY,
        SEARCH_VENUE_STATE,
    )
    .await;
    insert_match(&pool, SEARCH_DRIVE_ID, Some(GAME_SEARCH_ID), "score").await;
    // Hidden: a `game_matches` row with no WFTDA game, exactly the state of an
    // unmatched statsbook (`game_matches.wftda_game_id IS NULL`).
    insert_match(&pool, SEARCH_HIDDEN_DRIVE_ID, None, "none").await;

    // The generated columns must materialize the game_data JSON fields.
    let (tournament, venue_name, venue_city, venue_state, host_league): (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = sqlx::query_as(
        r#"SELECT tournament, venue_name, venue_city, venue_state, host_league
             FROM statsbook_games WHERE drive_file_id = $1"#,
    )
    .bind(SEARCH_DRIVE_ID)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(tournament.as_deref(), Some(SEARCH_TOURNAMENT));
    assert_eq!(venue_name.as_deref(), Some(SEARCH_VENUE));
    assert_eq!(venue_city.as_deref(), Some(SEARCH_VENUE_CITY));
    assert_eq!(venue_state.as_deref(), Some(SEARCH_VENUE_STATE));
    assert_eq!(host_league.as_deref(), Some("Web Render Test League"));
    println!("generated columns: {tournament:?} / {venue_name:?}, {venue_city:?}, {venue_state:?}");

    // ---- Values the pages must render -------------------------------------
    let (player_team_id, player_team_path, player_name, player_number): (
        i64,
        String,
        String,
        String,
    ) = sqlx::query_as(
        r#"SELECT s.wftda_team_id,
                  COALESCE(t.slug, t.id::STRING) AS team_path,
                  sk.name, sk.number
             FROM statsbook_skaters sk
             JOIN statsbook_sides s
               ON s.drive_file_id = sk.drive_file_id AND s.side = sk.side
             JOIN wftda_teams t ON t.id = s.wftda_team_id
            WHERE sk.drive_file_id = $1 AND s.wftda_team_id IS NOT NULL
            ORDER BY sk.number
            LIMIT 1"#,
    )
    .bind(DRIVE_FILE_ID)
    .fetch_one(&pool)
    .await
    .expect("a seeded skater on the matched statsbook");

    let roster_name: String = sqlx::query_scalar(
        "SELECT name FROM statsbook_skaters WHERE drive_file_id = $1 ORDER BY side, number LIMIT 1",
    )
    .bind(DRIVE_FILE_ID)
    .fetch_one(&pool)
    .await
    .unwrap();

    let matched_team_titles: (String, String) = sqlx::query_as(
        r#"SELECT t1.title, t2.title
             FROM wftda_games g
             JOIN wftda_teams t1 ON t1.id = g.team_1_id
             JOIN wftda_teams t2 ON t2.id = g.team_2_id
            WHERE g.id = $1"#,
    )
    .bind(GAME_MATCHED_ID)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        matched_team_titles,
        (TEAM_HOME_TITLE.to_string(), TEAM_AWAY_TITLE.to_string())
    );

    // ---- Render every page through the real router ------------------------
    let state = AppState {
        pool: Arc::new(pool.clone()),
        env: Arc::new(build_template_env()),
        cache: Arc::new(Cache::new()),
    };
    let app = build_router(state);

    // ===== Matched game page ==============================================
    let uri = format!("/game/{GAME_MATCHED_ID}");
    let (status, body) = get(&app, &uri).await;
    assert_eq!(status, StatusCode::OK, "GET {uri}");
    assert_contains(
        &body,
        &format!("https://stats.wftda.com/games/{GAME_MATCHED_ID}"),
        "matched game: WFTDA link",
    );
    assert_contains(
        &body,
        &format!("https://drive.google.com/file/d/{DRIVE_FILE_ID}/view"),
        "matched game: Drive statsbook link",
    );
    assert_contains(&body, "Final Score:", "matched game: score");
    assert_contains(&body, TEAM_HOME_TITLE, "matched game: home title");
    assert_contains(&body, TEAM_AWAY_TITLE, "matched game: away title");
    assert_contains(&body, "<h2>Period 1", "matched game: jam/period detail");
    assert_contains(&body, "Game Summary", "matched game: game summary");
    assert_contains(&body, &roster_name, "matched game: roster content");
    assert_contains(&body, "/player?team=", "matched game: player links");
    assert_contains(
        &body,
        &format!("/team/{TEAM_HOME_SLUG}"),
        "matched game: home team link",
    );
    assert_contains(
        &body,
        &format!("/team/{TEAM_AWAY_SLUG}"),
        "matched game: away team link",
    );
    println!("OK  matched game page {uri} ({} bytes)", body.len());

    // ===== Unmatched WFTDA game page (score only) =========================
    let uri = format!("/game/{GAME_UNMATCHED_ID}");
    let (status, body) = get(&app, &uri).await;
    assert_eq!(status, StatusCode::OK, "GET {uri}");
    assert_contains(
        &body,
        &format!("https://stats.wftda.com/games/{GAME_UNMATCHED_ID}"),
        "unmatched game: WFTDA link",
    );
    assert_contains(&body, "Final Score:", "unmatched game: score");
    assert_contains(
        &body,
        &format!("/team/{TEAM_UNMATCHED_1_SLUG}"),
        "unmatched game: home team link",
    );
    assert_contains(
        &body,
        &format!("/team/{TEAM_UNMATCHED_2_SLUG}"),
        "unmatched game: away team link",
    );
    assert_not_contains(
        &body,
        "drive.google.com",
        "unmatched game: no statsbook link",
    );
    assert_not_contains(&body, "Game Summary", "unmatched game: no summary");
    println!("OK  unmatched game page {uri} ({} bytes)", body.len());

    // ===== 404 for a missing game =========================================
    let (status, _) = get(&app, "/game/999999999").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unknown game id must 404");
    println!("OK  unknown game id -> 404");

    // ===== Team page ======================================================
    let uri = format!("/team/{TEAM_HOME_SLUG}");
    let (status, body) = get(&app, &uri).await;
    assert_eq!(status, StatusCode::OK, "GET {uri}");
    assert_contains(
        &body,
        &format!("https://stats.wftda.com/team/{TEAM_HOME_SLUG}"),
        "team page: WFTDA team link",
    );
    assert_contains(&body, "/league?league=", "team page: league link");
    assert_contains(
        &body,
        derive_league(TEAM_HOME_TITLE),
        "team page: league name",
    );
    assert_contains(&body, "Game History", "team page: game history");
    assert_contains(&body, ">W<", "team page: W/L/T record");
    assert_contains(&body, "/game/", "team page: game rows");
    assert_contains(
        &body,
        "drive.google.com/file/d/",
        "team page: statsbook link for a matched game",
    );
    assert_contains(&body, TEAM_HOME_TITLE, "team page: title");
    println!("OK  team page {uri} ({} bytes)", body.len());

    // Old numeric URLs redirect to the slug.
    let (status, _) = get(&app, &format!("/team/{TEAM_HOME_ID}")).await;
    assert_eq!(
        status,
        StatusCode::PERMANENT_REDIRECT,
        "numeric team URL must redirect to the slug"
    );
    println!("OK  numeric team URL -> 308");

    // ===== Player page ====================================================
    let uri = format!(
        "/player?team={player_team_id}&name={}&number={}",
        encode_component(&player_name),
        encode_component(&player_number)
    );
    let (status, body) = get(&app, &uri).await;
    assert_eq!(status, StatusCode::OK, "GET {uri}");
    assert_contains(&body, "Career Stats", "player page: career stats");
    assert_contains(&body, &format!("#{player_number}"), "player page: number");
    assert_contains(&body, "/game/", "player page: game history links");
    assert_contains(
        &body,
        &format!("/team/{player_team_path}"),
        "player page: team link",
    );
    println!("OK  player page {uri} ({} bytes)", body.len());

    // ===== Index ==========================================================
    let (status, body) = get(&app, "/").await;
    assert_eq!(status, StatusCode::OK, "GET /");
    assert_contains(&body, "Recent Games", "index: recent games");
    assert_contains(&body, "/game/", "index: game links");
    assert_contains(&body, "/team/", "index: team links");
    println!("OK  index / ({} bytes)", body.len());

    // ===== Search =========================================================
    let needle = TEAM_HOME_TITLE
        .split_whitespace()
        .next()
        .filter(|w| w.len() >= 2)
        .unwrap_or(TEAM_HOME_TITLE);
    let uri = format!("/search?q={needle}");
    let (status, body) = get(&app, &uri).await;
    assert_eq!(status, StatusCode::OK, "GET {uri}");
    assert_contains(&body, "<h2>Teams</h2>", "search: teams section");
    assert_contains(
        &body,
        &format!("/team/{TEAM_HOME_SLUG}"),
        "search: team link",
    );
    assert_contains(&body, "/league?league=", "search: league link");
    println!("OK  search {uri} ({} bytes)", body.len());

    // Tournament search: the matched statsbook's game appears under /game/,
    // while the hidden statsbook (same tournament string, no WFTDA match) does
    // not — neither its game link nor its venue.
    let uri = format!("/search?q={}", encode_component(SEARCH_TOURNAMENT));
    let (status, body) = get(&app, &uri).await;
    assert_eq!(status, StatusCode::OK, "GET {uri}");
    assert_contains(&body, "<h2>Games</h2>", "tournament search: games section");
    assert_contains(
        &body,
        &format!("/game/{GAME_SEARCH_ID}"),
        "tournament search: visible game link",
    );
    assert_contains(&body, SEARCH_TOURNAMENT, "tournament search: name shown");
    assert_contains(&body, SEARCH_VENUE, "tournament search: venue shown");
    assert_not_contains(
        &body,
        &format!("/game/{GAME_SEARCH_HIDDEN_ID}"),
        "tournament search: hidden statsbook's game absent",
    );
    assert_not_contains(
        &body,
        SEARCH_HIDDEN_VENUE,
        "tournament search: hidden statsbook's venue absent",
    );
    println!("OK  tournament search {uri} ({} bytes)", body.len());

    // Venue-name search resolves the same visible game.
    let uri = format!("/search?q={}", encode_component(SEARCH_VENUE));
    let (status, body) = get(&app, &uri).await;
    assert_eq!(status, StatusCode::OK, "GET {uri}");
    assert_contains(
        &body,
        &format!("/game/{GAME_SEARCH_ID}"),
        "venue search: game link",
    );
    assert_not_contains(
        &body,
        &format!("/game/{GAME_SEARCH_HIDDEN_ID}"),
        "venue search: hidden game absent",
    );
    println!("OK  venue search {uri} ({} bytes)", body.len());

    // ===== Leagues ========================================================
    let (status, body) = get(&app, "/leagues").await;
    assert_eq!(status, StatusCode::OK, "GET /leagues");
    assert_contains(&body, "Leagues", "leagues: heading");
    assert_contains(&body, "/league?league=", "leagues: league links");
    assert_contains(&body, "/team/", "leagues: team links");
    println!("OK  leagues /leagues ({} bytes)", body.len());

    // ===== Single league ==================================================
    let league_canonical = canonicalize_league(derive_league(TEAM_HOME_TITLE));
    let uri = format!("/league?league={league_canonical}");
    let (status, body) = get(&app, &uri).await;
    assert_eq!(status, StatusCode::OK, "GET {uri}");
    assert_contains(&body, "/team/", "league: team links");
    println!("OK  league {uri} ({} bytes)", body.len());

    // ===== Cleanup + scratch-database invariants ==========================
    cleanup(&pool).await;

    for table in [
        "statsbooks",
        "statsbook_games",
        "statsbook_sides",
        "statsbook_skaters",
        "statsbook_summary",
        "game_matches",
    ] {
        let sql = format!("SELECT count(*)::INT8 FROM {table}");
        let n: i64 = sqlx::query_scalar(&sql).fetch_one(&pool).await.unwrap();
        assert_eq!(n, 0, "scratch {table} must be empty after cleanup");
    }

    let teams_after: i64 = sqlx::query_scalar("SELECT count(*)::INT8 FROM wftda_teams")
        .fetch_one(&pool)
        .await
        .unwrap();
    let games_after: i64 = sqlx::query_scalar("SELECT count(*)::INT8 FROM wftda_games")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(teams_after, teams_before, "wftda_teams left unchanged");
    assert_eq!(games_after, games_before, "wftda_games left unchanged");
    println!(
        "cleanup complete: wftda_teams={teams_after} wftda_games={games_after}; \
         all statsbook tables empty"
    );

    println!("web render verification complete");
}
