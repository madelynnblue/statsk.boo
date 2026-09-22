//! No-Drive verification for redesign stage 4 (`store_parsed`).
//!
//! `DriveClient` cannot be exercised in CI/sandbox without a Google service
//! account, so `store_parsed` is deliberately Drive-free and verified here
//! against the scratch database `statskboo_v2` with fixture bytes.
//!
//! Ignored by default (it needs a live migrated database). Run with:
//!
//! ```text
//! DATABASE_URL=postgresql://root@cockroachdb:26257/statskboo_v2?sslmode=disable \
//!   cargo test --test statsbook_parse_store_test -- --ignored --nocapture
//! ```
//!
//! The test uses fixed synthetic `drive_file_id`s (`__statsbook_parse_test__*`)
//! and deletes them at the start and end, so it leaves the database unchanged.

use chrono::{DateTime, TimeZone, Utc};
use sqlx::PgPool;
use statskboo::ingest::parse::PARSER_VERSION;
use statskboo::ingest::statsbook_parse::{ParseOutcome, store_parsed};

const ID_SINGLE: &str = "__statsbook_parse_test__single";
const ID_EMPTY: &str = "__statsbook_parse_test__empty";
const ID_OLD: &str = "__statsbook_parse_test__old";
const ID_NEW: &str = "__statsbook_parse_test__new";

const ALL_IDS: [&str; 4] = [ID_SINGLE, ID_EMPTY, ID_OLD, ID_NEW];

const STANDBYS: &str = "tests/fixtures/standbys-flatiron.xlsx";
const EMPTY: &str = "tests/fixtures/hulagans-vendolls-empty.xlsx";

fn db_url() -> String {
    std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        "postgresql://root@cockroachdb:26257/statskboo_v2?sslmode=disable".into()
    })
}

async fn cleanup(pool: &PgPool) {
    // statsbook_sides references statsbooks ON DELETE CASCADE (which cascades
    // to statsbook_skaters); statsbook_games references statsbooks ON DELETE
    // CASCADE (which cascades to statsbook_summary). One delete per id is
    // therefore a full cleanup.
    for id in ALL_IDS {
        sqlx::query("DELETE FROM statsbooks WHERE drive_file_id = $1")
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }
}

async fn insert_statsbook(pool: &PgPool, id: &str, modified: DateTime<Utc>) {
    sqlx::query(
        r#"INSERT INTO statsbooks
             (drive_file_id, name, quarter, mime_type, modified_time,
              first_seen, last_seen, parse_status, parser_version, parse_error)
           VALUES ($1, $2, 'test', NULL, $3, now(), now(), 'pending', NULL, NULL)
           ON CONFLICT (drive_file_id) DO UPDATE SET
             modified_time  = EXCLUDED.modified_time,
             parse_status   = 'pending',
             parser_version = NULL,
             parse_error    = NULL"#,
    )
    .bind(id)
    .bind(format!("{id}.xlsx"))
    .bind(modified)
    .execute(pool)
    .await
    .unwrap();
}

async fn count(pool: &PgPool, table: &str, id: &str) -> i64 {
    // `table` is a hard-coded literal at every call site.
    let sql = format!("SELECT count(*)::INT8 FROM {table} WHERE drive_file_id = $1");
    sqlx::query_scalar::<_, i64>(&sql)
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn parse_status(pool: &PgPool, id: &str) -> (String, Option<i64>, Option<String>) {
    sqlx::query_as::<_, (String, Option<i64>, Option<String>)>(
        "SELECT parse_status, parser_version, parse_error FROM statsbooks WHERE drive_file_id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
}

fn read_fixture(path: &str) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

#[tokio::test]
#[ignore = "requires a live migrated statskboo_v2 database; run with --ignored"]
async fn store_parsed_verification_without_drive() {
    let pool = PgPool::connect(&db_url()).await.unwrap();
    cleanup(&pool).await;

    // ---- 1. Standbys/Flatiron: parse, store, idempotent re-run -------------
    let standbys = read_fixture(STANDBYS);
    let (parsed, _) =
        statskboo::ingest::parse::parse_statsbook_with_date(&standbys, Some(STANDBYS)).unwrap();
    let expected_skaters = parsed.home.skaters.len() + parsed.away.skaters.len();
    // Pinned so a parser regression that silently drops roster rows is caught.
    assert_eq!(
        expected_skaters, 30,
        "standbys-flatiron roster size changed"
    );
    println!(
        "standbys-flatiron: parsed home_skaters={} away_skaters={} expected_total={expected_skaters} score={}-{}",
        parsed.home.skaters.len(),
        parsed.away.skaters.len(),
        parsed.home_score,
        parsed.away_score,
    );

    insert_statsbook(
        &pool,
        ID_SINGLE,
        Utc.with_ymd_and_hms(2024, 6, 1, 0, 0, 0).unwrap(),
    )
    .await;
    let outcome = store_parsed(&pool, ID_SINGLE, STANDBYS, &standbys)
        .await
        .unwrap();
    match &outcome {
        ParseOutcome::Parsed {
            sides,
            skaters,
            summaries,
        } => {
            assert_eq!(*sides, 2, "sides");
            assert_eq!(*summaries, 2, "summary rows");
            assert_eq!(*skaters, expected_skaters, "skater count");
        }
        other => panic!("expected Parsed, got {other:?}"),
    }

    let games = count(&pool, "statsbook_games", ID_SINGLE).await;
    let sides = count(&pool, "statsbook_sides", ID_SINGLE).await;
    let skaters = count(&pool, "statsbook_skaters", ID_SINGLE).await;
    let summaries = count(&pool, "statsbook_summary", ID_SINGLE).await;
    let (status, version, error) = parse_status(&pool, ID_SINGLE).await;
    println!(
        "single parse: statsbook_games={games} sides={sides} skaters={skaters} summary={summaries} \
         status={status} parser_version={version:?} parse_error={error:?}"
    );
    assert_eq!(games, 1, "exactly one statsbook_games row");
    assert_eq!(sides, 2, "two statsbook_sides rows");
    assert_eq!(skaters, expected_skaters as i64, "statsbook_skaters rows");
    assert_eq!(summaries, 2, "two statsbook_summary rows");
    assert_eq!(status, "parsed");
    assert_eq!(version, Some(PARSER_VERSION));
    assert_eq!(error, None);

    // Re-running must replace, not duplicate.
    let again = store_parsed(&pool, ID_SINGLE, STANDBYS, &standbys)
        .await
        .unwrap();
    assert!(matches!(again, ParseOutcome::Parsed { .. }));
    assert_eq!(count(&pool, "statsbook_games", ID_SINGLE).await, 1);
    assert_eq!(count(&pool, "statsbook_sides", ID_SINGLE).await, 2);
    assert_eq!(
        count(&pool, "statsbook_skaters", ID_SINGLE).await,
        expected_skaters as i64
    );
    assert_eq!(count(&pool, "statsbook_summary", ID_SINGLE).await, 2);
    println!("re-run idempotent: counts unchanged");
    cleanup(&pool).await;

    // ---- 2. Empty placeholder: skipped, nothing stored ---------------------
    let empty = read_fixture(EMPTY);
    insert_statsbook(
        &pool,
        ID_EMPTY,
        Utc.with_ymd_and_hms(2024, 7, 1, 0, 0, 0).unwrap(),
    )
    .await;
    let outcome = store_parsed(&pool, ID_EMPTY, EMPTY, &empty).await.unwrap();
    let (status, version, _) = parse_status(&pool, ID_EMPTY).await;
    println!(
        "empty placeholder: outcome={outcome:?} status={status} parser_version={version:?} \
         games={} sides={} skaters={} summary={}",
        count(&pool, "statsbook_games", ID_EMPTY).await,
        count(&pool, "statsbook_sides", ID_EMPTY).await,
        count(&pool, "statsbook_skaters", ID_EMPTY).await,
        count(&pool, "statsbook_summary", ID_EMPTY).await,
    );
    assert_eq!(outcome, ParseOutcome::SkippedEmpty);
    assert_eq!(status, "skipped");
    assert_eq!(count(&pool, "statsbook_games", ID_EMPTY).await, 0);
    assert_eq!(count(&pool, "statsbook_sides", ID_EMPTY).await, 0);
    assert_eq!(count(&pool, "statsbook_skaters", ID_EMPTY).await, 0);
    assert_eq!(count(&pool, "statsbook_summary", ID_EMPTY).await, 0);
    cleanup(&pool).await;

    // ---- 3. Duplicate uploads are both stored ------------------------------
    // Dedup happens at the game level in `resolve` (newest `modified_time`
    // wins); `store_parsed` is deliberately fingerprint-free.
    let old_time = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
    let new_time = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
    insert_statsbook(&pool, ID_OLD, old_time).await;
    insert_statsbook(&pool, ID_NEW, new_time).await;

    let old_outcome = store_parsed(&pool, ID_OLD, STANDBYS, &standbys)
        .await
        .unwrap();
    assert!(matches!(old_outcome, ParseOutcome::Parsed { .. }));
    let new_outcome = store_parsed(&pool, ID_NEW, STANDBYS, &standbys)
        .await
        .unwrap();
    assert!(matches!(new_outcome, ParseOutcome::Parsed { .. }));

    let (old_status, _, _) = parse_status(&pool, ID_OLD).await;
    let (new_status, _, _) = parse_status(&pool, ID_NEW).await;
    println!(
        "duplicate uploads: old_status={old_status} old_games={} new_status={new_status} new_games={}",
        count(&pool, "statsbook_games", ID_OLD).await,
        count(&pool, "statsbook_games", ID_NEW).await,
    );
    assert_eq!(old_status, "parsed");
    assert_eq!(new_status, "parsed");
    assert_eq!(count(&pool, "statsbook_games", ID_OLD).await, 1);
    assert_eq!(count(&pool, "statsbook_games", ID_NEW).await, 1);

    // Re-parsing either file stays idempotent for that file.
    let old_again = store_parsed(&pool, ID_OLD, STANDBYS, &standbys)
        .await
        .unwrap();
    assert!(matches!(old_again, ParseOutcome::Parsed { .. }));
    assert_eq!(count(&pool, "statsbook_games", ID_OLD).await, 1);
    assert_eq!(count(&pool, "statsbook_games", ID_NEW).await, 1);
    println!("duplicate re-parse: each file still has exactly one row");

    cleanup(&pool).await;
    let leftover: i64 = sqlx::query_scalar(
        "SELECT count(*)::INT8 FROM statsbooks WHERE drive_file_id = ANY($1::STRING[])",
    )
    .bind(&ALL_IDS[..])
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(leftover, 0, "test rows cleaned up");
    println!("cleanup complete: {leftover} synthetic rows left");
}
