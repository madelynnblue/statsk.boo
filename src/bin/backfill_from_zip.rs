//! One-off backfill: populate the GAME_DATA_DIR cache from a zip of all
//! game files, and re-key source='file' rows to their Drive file IDs so
//! web Drive links work. Requires GOOGLE_SERVICE_ACCOUNT_PATH for a one-time
//! paced Drive listing (path -> file id); the cache fill itself is offline.
//!
//! Usage: cargo run --release --bin backfill_from_zip -- <zip> [--dry-run]
//! Env:   DATABASE_URL, GAME_DATA_DIR, GOOGLE_SERVICE_ACCOUNT_PATH, GOOGLE_DRIVE_FOLDER_ID

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use tracing::info;
use wsb::config::Config;
use wsb::ingest::data_cache::write_game_data;
use wsb::ingest::drive::{DriveClient, DriveFile};
use wsb::ingest::parse::parse_statsbook_with_date;
use wsb::ingest::{build_fingerprint, compute_canonical_id};

const ZIP_ROOT: &str = "Public Stats Repository";

struct Row {
    id: String,
}

async fn rekey_row(
    pool: &PgPool,
    old_id: &str,
    drive_id: &str,
    drive_time: DateTime<Utc>,
) -> Result<()> {
    // Children (sides/skaters/summary) reference games(id) with ON DELETE
    // CASCADE; CockroachDB refuses UPDATE of the referenced PK (23503), and
    // the new row shares the unique canonical_id with the old one. Also, a
    // DELETE followed by INSERT...SELECT from the same table inside one tx
    // would see the deleted row as absent. So: fetch the old row's base
    // columns plus all children into Rust first, then one tx: DELETE old
    // row (cascade), INSERT new games row with explicit VALUES (base
    // columns only), re-INSERT children under drive_id.
    let sides: Vec<(String, Option<String>, Option<String>, Option<String>, String, String)> =
        sqlx::query!(
            "SELECT side, league, team, color, league_canonical, team_canonical FROM game_sides WHERE game_id = $1",
            old_id
        )
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(|r| (r.side, r.league, r.team, r.color, r.league_canonical, r.team_canonical))
        .collect();

    let skaters: Vec<(String, String, String)> = sqlx::query!(
        "SELECT side, number, name FROM game_skaters WHERE game_id = $1",
        old_id
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|r| (r.side, r.number, r.name))
    .collect();

    let summaries: Vec<(String, serde_json::Value)> = sqlx::query!(
        "SELECT side, stats FROM game_summary WHERE game_id = $1",
        old_id
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|r| (r.side, r.stats))
    .collect();

    let old = sqlx::query!(
        "SELECT date, parser_version, version, fingerprint, canonical_id, game_data FROM games WHERE id = $1",
        old_id
    )
    .fetch_one(pool)
    .await?;

    let mut tx = pool.begin().await?;
    sqlx::query!("DELETE FROM games WHERE id = $1", old_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query!(
        r#"INSERT INTO games
           (id, source, date, parser_version, version, modified_time, fingerprint, canonical_id, game_data)
           VALUES ($1, 'drive', $2, $3, $4, $5, $6, $7, $8)"#,
        drive_id,
        old.date,
        old.parser_version,
        old.version,
        drive_time,
        old.fingerprint,
        old.canonical_id,
        old.game_data,
    )
    .execute(&mut *tx)
    .await?;
    for (side, league, team, color, league_canonical, team_canonical) in sides {
        sqlx::query!(
            "INSERT INTO game_sides (game_id, side, league, team, color, league_canonical, team_canonical) VALUES ($1, $2, $3, $4, $5, $6, $7)",
            drive_id,
            side,
            league,
            team,
            color,
            league_canonical,
            team_canonical,
        )
        .execute(&mut *tx)
        .await?;
    }
    if !skaters.is_empty() {
        let mut qb =
            sqlx::QueryBuilder::new("INSERT INTO game_skaters (game_id, side, number, name) ");
        qb.push_values(skaters.iter(), |mut b, (side, number, name)| {
            b.push_bind(drive_id)
                .push_bind(side)
                .push_bind(number)
                .push_bind(name);
        });
        qb.build().execute(&mut *tx).await?;
    }
    for (side, stats) in &summaries {
        sqlx::query!(
            "INSERT INTO game_summary (game_id, side, stats) VALUES ($1, $2, $3)",
            drive_id,
            side,
            stats,
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let mut args = std::env::args().skip(1);
    let zip_path = args
        .next()
        .context("usage: backfill_from_zip <zip> [--dry-run]")?;
    let dry_run = args.any(|a| a == "--dry-run");
    let cfg = Config::from_env()?;
    let cache_dir = cfg
        .game_data_dir
        .as_deref()
        .context("GAME_DATA_DIR must be set")?;
    let sa = cfg
        .service_account
        .clone()
        .context("GOOGLE_SERVICE_ACCOUNT_PATH must be set")?;
    let pool = PgPool::connect(&cfg.database_url).await?;

    // File-mode rows keyed by canonical_id (unique index -> one row).
    let rows: HashMap<String, Row> =
        sqlx::query!("SELECT id, canonical_id FROM games WHERE source = 'file'")
            .fetch_all(&pool)
            .await?
            .into_iter()
            .map(|r| (r.canonical_id, Row { id: r.id }))
            .collect();
    info!("loaded {} file-mode row(s)", rows.len());

    // Pass 1 (offline): parse zip entries in parallel, fill the cache,
    // remember canonical_id + relative path for the re-key pass.
    let file = std::fs::File::open(&zip_path).with_context(|| format!("opening {zip_path}"))?;
    let archive = Mutex::new(zip::ZipArchive::new(file)?);

    // Central-directory scan (fast): collect (zip index, entry name,
    // rel_path) for every xlsx entry before any parsing.
    let mut entries: Vec<(usize, String, String)> = Vec::new();
    {
        let mut archive = archive.lock().unwrap();
        for i in 0..archive.len() {
            let entry = archive.by_index(i)?;
            if entry.is_dir() {
                continue;
            }
            let name = entry.name().to_string();
            if !name.ends_with(".xlsx") {
                continue;
            }
            let rel_path = name
                .strip_prefix(&format!("{ZIP_ROOT}/"))
                .unwrap_or(&name)
                .to_string();
            entries.push((i, name, rel_path));
        }
    }
    let scanned = entries.len();

    // Worker pool: parse outside the archive lock (ZipArchive is not Sync,
    // so workers lock briefly to read an entry's bytes, then parse).
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(16);
    let next = AtomicUsize::new(0);
    let completed = AtomicUsize::new(0);
    let parse_errors = AtomicUsize::new(0);
    let parsed: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new()); // (canonical_id, rel_path)
    let first_error: Mutex<Option<String>> = Mutex::new(None);

    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= entries.len() || first_error.lock().unwrap().is_some() {
                        break;
                    }
                    let (idx, name, rel_path) = &entries[i];
                    let mut bytes = Vec::new();
                    {
                        let mut archive = archive.lock().unwrap();
                        let mut entry = match archive.by_index(*idx) {
                            Ok(e) => e,
                            Err(e) => {
                                *first_error.lock().unwrap() =
                                    Some(format!("reading zip entry {idx} ({name}): {e}"));
                                break;
                            }
                        };
                        if let Err(e) = entry.read_to_end(&mut bytes) {
                            *first_error.lock().unwrap() =
                                Some(format!("reading zip entry {idx} ({name}): {e}"));
                            break;
                        }
                    }
                    match parse_statsbook_with_date(&bytes).and_then(|(game, date)| {
                        build_fingerprint(&game, date).map(|fp| compute_canonical_id(&fp))
                    }) {
                        Ok(canonical_id) => {
                            if !dry_run
                                && let Err(e) = write_game_data(cache_dir, &canonical_id, &bytes)
                            {
                                *first_error.lock().unwrap() =
                                    Some(format!("writing cache for {rel_path}: {e}"));
                                break;
                            }
                            parsed
                                .lock()
                                .unwrap()
                                .push((canonical_id, rel_path.clone()));
                        }
                        Err(e) => {
                            parse_errors.fetch_add(1, Ordering::Relaxed);
                            tracing::warn!("parse failed for {}: {e:#}", name);
                        }
                    }
                    let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                    if done.is_multiple_of(500) {
                        eprintln!("parsed {done} files...");
                    }
                }
            });
        }
    });

    let parse_errors = parse_errors.load(Ordering::Relaxed);
    if let Some(err) = first_error.into_inner().unwrap() {
        return Err(anyhow::anyhow!(err));
    }
    let parsed = parsed.into_inner().unwrap();
    info!(
        "scanned {scanned} xlsx, parsed {}, parse errors {parse_errors}",
        parsed.len()
    );

    // Pass 2: one paced Drive listing: path -> (id, modified_time).
    let client = DriveClient::new(sa);
    let tree = client
        .list_tree_with_paths(&cfg.google_drive_folder_id)
        .await?;
    let by_path: HashMap<String, DriveFile> = tree.into_iter().collect();
    info!("drive listing: {} file(s)", by_path.len());

    // Pass 3: re-key matching rows.
    let mut rekeyed = 0usize;
    let mut no_db_row = 0usize;
    let mut no_path_match = 0usize;
    let mut already = HashSet::new();
    for (canonical_id, rel_path) in &parsed {
        let Some(row) = rows.get(canonical_id) else {
            no_db_row += 1;
            continue;
        };
        // Drive file names usually lack the .xlsx suffix that the zip
        // export adds, so fall back to the extension-stripped path. The
        // exact match wins when both a "foo" and "foo.xlsx" exist.
        let Some(drive) = by_path.get(rel_path).or_else(|| {
            rel_path
                .strip_suffix(".xlsx")
                .and_then(|stem| by_path.get(stem))
        }) else {
            no_path_match += 1;
            continue;
        };
        if !already.insert(canonical_id.clone()) {
            continue; // duplicate zip entry for the same game
        }
        let drive_time: DateTime<Utc> = drive
            .modified_time
            .parse()
            .with_context(|| format!("parsing modified_time for {}", rel_path))?;
        if dry_run {
            eprintln!("would re-key {} -> {} ({})", row.id, drive.id, rel_path);
        } else {
            rekey_row(&pool, &row.id, &drive.id, drive_time).await?;
        }
        rekeyed += 1;
    }

    println!("=== summary ===");
    println!("zip xlsx scanned:          {scanned}");
    println!(
        "parsed (cache written):    {} ({})",
        parsed.len(),
        if dry_run { "dry-run" } else { "written" }
    );
    println!("parse/fingerprint errors:  {parse_errors}");
    println!("re-keyed to drive:         {rekeyed}");
    println!("zip entry, no file row:    {no_db_row}");
    println!("file row, no path match:   {no_path_match}");
    Ok(())
}
