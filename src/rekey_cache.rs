//! One-off re-key of the legacy on-disk statsbook cache.
//!
//! The pre-WFTDA pipeline cached raw statsbook bytes as
//! `{GAME_DATA_DIR}/{canonical_id}.xlsx`; the WFTDA-first pipeline keys
//! statsbooks by `drive_file_id` and caches them as
//! `{GAME_DATA_DIR}/{drive_file_id}.xlsx`. This tool converts an existing prod
//! `game-data` volume to the new naming so the first `PARSER_VERSION` bump
//! after deploy re-parses from disk instead of re-downloading everything.
//!
//! The mapping comes from either:
//! - `--source-db-url <legacy statskboo URL>`: `SELECT id, canonical_id,
//!   parser_version FROM games` (in the legacy schema `games.id` is the Drive
//!   file id), or
//! - `--mapping-csv <path>`: columns `drive_file_id,canonical_id[,parser_version]`
//!   (an optional `drive_file_id` header row is skipped).
//!
//! For each `drive_file_id` it picks the candidate with the highest
//! `parser_version` whose legacy source file exists, and renames it (or copies
//! with `--copy`). A destination that already exists is left alone and counted
//! as skipped, so re-running is safe. `--dry-run` reports without writing.
//!
//! It is deliberately self-contained: it only reads the three legacy columns
//! above (via a runtime query, not `sqlx::query!`), so it can run before the
//! legacy database is dropped and does not depend on the new schema. It is
//! exposed as a subcommand of the main binary because the production image
//! ships only `target/release/statskboo`:
//!
//! ```text
//! statskboo rekey-cache --cache-dir /data/game-data --source-db-url <legacy-url> [--copy] [--dry-run]
//! statskboo rekey-cache --cache-dir /data/game-data --mapping-csv mapping.csv     [--copy] [--dry-run]
//! ```

use crate::ingest::data_cache::cache_path;
use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

pub const USAGE: &str = "\
usage: statskboo rekey-cache --cache-dir <dir> (--source-db-url <url> | --mapping-csv <path>) [--copy] [--dry-run]

Re-key the legacy statsbook cache ({canonical_id}.xlsx) to the drive_file_id
naming ({drive_file_id}.xlsx). The default is to rename; --copy keeps the
legacy files. --dry-run reports what would happen without touching any file.
--cache-dir may be omitted when GAME_DATA_DIR is set.";

/// One legacy-to-new mapping, from the DB or CSV.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappingRow {
    pub drive_file_id: String,
    pub canonical_id: String,
    pub parser_version: i64,
}

/// Where the mapping comes from. Exactly one is required.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RekeySource {
    /// A legacy statskboo database URL (only `games` is read).
    Db(String),
    /// A CSV with `drive_file_id,canonical_id[,parser_version]`.
    Csv(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RekeyArgs {
    pub cache_dir: PathBuf,
    pub source: RekeySource,
    pub copy: bool,
    pub dry_run: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RekeyCommand {
    Run(RekeyArgs),
    Help,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappedFile {
    pub drive_file_id: String,
    pub canonical_id: String,
    pub parser_version: i64,
}

/// Outcome of a re-key run, including per-item detail for the printed report.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RekeyReport {
    /// Rows loaded from the DB or CSV.
    pub mapping_rows: usize,
    /// Distinct `drive_file_id`s among those rows.
    pub drive_file_ids: usize,
    /// Legacy files renamed/copied to the new name.
    pub mapped: usize,
    /// Destinations that already existed (idempotent re-run).
    pub skipped: usize,
    /// Drive file ids with no existing legacy source file.
    pub missing: usize,
    /// `.xlsx` files in the cache dir referenced by no mapping row.
    pub orphan: usize,
    pub copied: bool,
    pub dry_run: bool,
    pub mapped_files: Vec<MappedFile>,
    pub skipped_ids: Vec<String>,
    pub missing_ids: Vec<String>,
    pub orphan_files: Vec<String>,
    /// Non-fatal problems (currently: failing to carry a source's mtime onto a
    /// `--copy` destination). The run still succeeds.
    pub warnings: Vec<String>,
}

impl RekeyReport {
    pub fn print(&self) {
        let mode = if self.copied { "copy" } else { "rename" };
        let suffix = if self.dry_run {
            " [dry-run: no files written]"
        } else {
            ""
        };
        println!(
            "rekey-cache: {} mapping row(s) for {} drive file id(s), mode {mode}{suffix}",
            self.mapping_rows, self.drive_file_ids
        );
        for m in &self.mapped_files {
            println!(
                "  mapped  {} <- {} (parser_version {})",
                m.drive_file_id, m.canonical_id, m.parser_version
            );
        }
        for id in &self.skipped_ids {
            println!("  skipped {id} (destination already exists)");
        }
        for id in &self.missing_ids {
            println!("  missing {id} (no legacy source file)");
        }
        for name in &self.orphan_files {
            println!("  orphan  {name} (no mapping row)");
        }
        for warning in &self.warnings {
            println!("  warning {warning}");
        }
        println!(
            "rekey-cache: mapped {}, skipped {}, missing {}, orphan {}, {} warning(s)",
            self.mapped,
            self.skipped,
            self.missing,
            self.orphan,
            self.warnings.len()
        );
    }
}

/// Carries `meta`'s mtime (and atime) onto `dst`.
///
/// A `--copy` destination must not look newer than its source: the cache
/// freshness rule compares file mtime to Drive's `modified_time`, so using the
/// copy time would make bytes for a superseded Drive revision look fresh and
/// they would be served instead of re-downloaded.
fn preserve_times(meta: &fs::Metadata, dst: &Path) -> std::io::Result<()> {
    let mut times = fs::FileTimes::new();
    if let Ok(modified) = meta.modified() {
        times = times.set_modified(modified);
    }
    if let Ok(accessed) = meta.accessed() {
        times = times.set_accessed(accessed);
    }
    fs::File::options().write(true).open(dst)?.set_times(times)
}

/// Parses `args` (without the program name or the `rekey-cache` subcommand).
pub fn parse_args<I, S>(args: I) -> Result<RekeyCommand>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let args: Vec<String> = args.into_iter().map(|s| s.as_ref().to_string()).collect();
    let mut cache_dir: Option<PathBuf> = None;
    let mut db_url: Option<String> = None;
    let mut csv: Option<PathBuf> = None;
    let mut copy = false;
    let mut dry_run = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => return Ok(RekeyCommand::Help),
            "--cache-dir" => {
                cache_dir = Some(PathBuf::from(flag_value(&args, &mut i, "--cache-dir")?))
            }
            "--source-db-url" => db_url = Some(flag_value(&args, &mut i, "--source-db-url")?),
            "--mapping-csv" => {
                csv = Some(PathBuf::from(flag_value(&args, &mut i, "--mapping-csv")?))
            }
            "--copy" => copy = true,
            "--dry-run" => dry_run = true,
            other => bail!("unknown argument {other:?}\n\n{USAGE}"),
        }
        i += 1;
    }

    let cache_dir = cache_dir
        .or_else(|| {
            std::env::var_os("GAME_DATA_DIR")
                .map(PathBuf::from)
                .filter(|p| !p.as_os_str().is_empty())
        })
        .with_context(|| format!("--cache-dir is required (or set GAME_DATA_DIR)\n\n{USAGE}"))?;

    let source = match (db_url, csv) {
        (Some(_), Some(_)) => {
            bail!("--source-db-url and --mapping-csv are mutually exclusive\n\n{USAGE}")
        }
        (Some(url), None) => RekeySource::Db(url),
        (None, Some(path)) => RekeySource::Csv(path),
        (None, None) => bail!("one of --source-db-url or --mapping-csv is required\n\n{USAGE}"),
    };

    Ok(RekeyCommand::Run(RekeyArgs {
        cache_dir,
        source,
        copy,
        dry_run,
    }))
}

/// `--flag value` helper: consumes the next argument, erroring if absent/empty.
fn flag_value(args: &[String], i: &mut usize, flag: &str) -> Result<String> {
    *i += 1;
    args.get(*i)
        .cloned()
        .filter(|v| !v.is_empty())
        .with_context(|| format!("{flag} requires a value"))
}

/// Entry point for `statskboo rekey-cache ...` and the `rekey_game_cache` bin.
pub async fn run_cli<I, S>(args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    match parse_args(args)? {
        RekeyCommand::Help => {
            println!("{USAGE}");
            Ok(())
        }
        RekeyCommand::Run(args) => {
            let report = run(args).await?;
            report.print();
            Ok(())
        }
    }
}

/// Loads the mapping and performs the re-key.
pub async fn run(args: RekeyArgs) -> Result<RekeyReport> {
    let rows = match &args.source {
        RekeySource::Db(url) => load_db(url).await?,
        RekeySource::Csv(path) => load_csv(path)?,
    };
    rekey(&args.cache_dir, rows, args.copy, args.dry_run)
}

/// The re-key itself, filesystem-only and testable without a database.
///
/// `cache_dir` need not exist: with no files, every id is reported missing.
pub fn rekey(
    cache_dir: &Path,
    rows: Vec<MappingRow>,
    copy: bool,
    dry_run: bool,
) -> Result<RekeyReport> {
    let mut groups: BTreeMap<String, Vec<MappingRow>> = BTreeMap::new();
    for row in rows {
        groups
            .entry(row.drive_file_id.clone())
            .or_default()
            .push(row);
    }

    let mut report = RekeyReport {
        mapping_rows: groups.values().map(Vec::len).sum(),
        drive_file_ids: groups.len(),
        copied: copy,
        dry_run,
        ..Default::default()
    };

    // Everything a mapping row names is "known", even if its destination is
    // skipped, so it is never reported as an orphan.
    let mut referenced_sources: BTreeSet<String> = BTreeSet::new();
    for candidates in groups.values() {
        for row in candidates {
            referenced_sources.insert(row.canonical_id.clone());
        }
    }

    for (drive_file_id, candidates) in &groups {
        let Some(dst) = cache_path(cache_dir, drive_file_id) else {
            report.missing += 1;
            report.missing_ids.push(drive_file_id.clone());
            continue;
        };

        // Highest parser_version among candidates whose legacy file exists.
        let chosen = candidates
            .iter()
            .filter_map(|row| {
                cache_path(cache_dir, &row.canonical_id)
                    .filter(|p| p.is_file())
                    .map(|p| (p, row))
            })
            .max_by_key(|(_, row)| row.parser_version);

        let Some((src, chosen)) = chosen else {
            report.missing += 1;
            report.missing_ids.push(drive_file_id.clone());
            continue;
        };

        if dst.exists() {
            report.skipped += 1;
            report.skipped_ids.push(drive_file_id.clone());
            continue;
        }

        if !dry_run {
            if copy {
                // Read the source metadata before copying so its original mtime
                // is preserved, not the copy's time.
                let src_meta = fs::metadata(&src)
                    .with_context(|| format!("reading metadata of {}", src.display()))?;
                fs::copy(&src, &dst)
                    .with_context(|| format!("copying {} -> {}", src.display(), dst.display()))?;
                if let Err(e) = preserve_times(&src_meta, &dst) {
                    report.warnings.push(format!(
                        "could not preserve mtime on {}: {e}",
                        dst.display()
                    ));
                }
            } else {
                fs::rename(&src, &dst)
                    .with_context(|| format!("renaming {} -> {}", src.display(), dst.display()))?;
            }
        }
        report.mapped += 1;
        report.mapped_files.push(MappedFile {
            drive_file_id: drive_file_id.clone(),
            canonical_id: chosen.canonical_id.clone(),
            parser_version: chosen.parser_version,
        });
    }

    // Orphans: `.xlsx` files that are neither a destination nor a legacy source
    // named by any mapping row.
    let destinations: BTreeSet<String> = groups
        .keys()
        .filter_map(|id| cache_path(cache_dir, id))
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    if cache_dir.is_dir() {
        for entry in fs::read_dir(cache_dir)
            .with_context(|| format!("reading cache dir {}", cache_dir.display()))?
        {
            let name = entry?.file_name().to_string_lossy().into_owned();
            let Some(stem) = name.strip_suffix(".xlsx") else {
                continue;
            };
            if destinations.contains(&name) || referenced_sources.contains(stem) {
                continue;
            }
            report.orphan += 1;
            report.orphan_files.push(name);
        }
    }
    report.orphan_files.sort();

    Ok(report)
}

/// Reads `drive_file_id,canonical_id[,parser_version]`; a leading
/// `drive_file_id` header and blank/`#` lines are skipped.
fn load_csv(path: &Path) -> Result<Vec<MappingRow>> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("reading mapping CSV {}", path.display()))?;
    let mut rows = Vec::new();
    let mut first = true;
    for (lineno, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split(',').map(str::trim).collect();
        if first
            && fields
                .first()
                .is_some_and(|f| f.eq_ignore_ascii_case("drive_file_id"))
        {
            first = false;
            continue;
        }
        first = false;
        if fields.len() < 2 || fields[0].is_empty() || fields[1].is_empty() {
            bail!(
                "mapping CSV {}:{}: expected drive_file_id,canonical_id[,parser_version]",
                path.display(),
                lineno + 1
            );
        }
        let parser_version = match fields.get(2) {
            Some(v) if !v.is_empty() => v.parse::<i64>().with_context(|| {
                format!(
                    "mapping CSV {}:{}: invalid parser_version {v:?}",
                    path.display(),
                    lineno + 1
                )
            })?,
            _ => 0,
        };
        rows.push(MappingRow {
            drive_file_id: fields[0].to_string(),
            canonical_id: fields[1].to_string(),
            parser_version,
        });
    }
    Ok(rows)
}

/// Reads the legacy `games` table. A runtime query on purpose: the legacy
/// schema is absent from the new database and must not be added to `.sqlx`.
async fn load_db(url: &str) -> Result<Vec<MappingRow>> {
    let pool = sqlx::PgPool::connect(url)
        .await
        .with_context(|| format!("connecting to legacy database {url}"))?;
    // CockroachDB's INTEGER is 64-bit, so `parser_version` decodes as INT8.
    let rows = sqlx::query_as::<_, (String, String, i64)>(
        "SELECT id, canonical_id, parser_version FROM games",
    )
    .fetch_all(&pool)
    .await
    .context("querying legacy games (id, canonical_id, parser_version)")?;
    pool.close().await;
    Ok(rows
        .into_iter()
        .map(|(drive_file_id, canonical_id, parser_version)| MappingRow {
            drive_file_id,
            canonical_id,
            parser_version,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::data_cache::cache_is_fresh;
    use chrono::{DateTime, TimeZone, Utc};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::SystemTime;

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "statskboo-rekey-test-{}-{}-{}",
                tag,
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            TempDir(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn row(drive: &str, canonical: &str, version: i64) -> MappingRow {
        MappingRow {
            drive_file_id: drive.into(),
            canonical_id: canonical.into(),
            parser_version: version,
        }
    }

    fn ls(dir: &Path) -> BTreeSet<String> {
        fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn parse_args_requires_exactly_one_source() {
        assert!(matches!(
            parse_args(["--cache-dir", "/c", "--dry-run"]),
            Err(_)
        ));
        assert!(matches!(
            parse_args(["--cache-dir", "/c", "--mapping-csv", "m.csv"]),
            Ok(RekeyCommand::Run(_))
        ));
        assert!(matches!(
            parse_args([
                "--cache-dir",
                "/c",
                "--mapping-csv",
                "m.csv",
                "--source-db-url",
                "postgres://x"
            ]),
            Err(_)
        ));
        // --copy and --dry-run are independent booleans.
        match parse_args([
            "--cache-dir",
            "/c",
            "--mapping-csv",
            "m.csv",
            "--copy",
            "--dry-run",
        ])
        .unwrap()
        {
            RekeyCommand::Run(a) => {
                assert!(a.copy);
                assert!(a.dry_run);
                assert_eq!(a.cache_dir, PathBuf::from("/c"));
                assert_eq!(a.source, RekeySource::Csv(PathBuf::from("m.csv")));
            }
            RekeyCommand::Help => panic!("expected Run"),
        }
        assert_eq!(parse_args(["--help"]).unwrap(), RekeyCommand::Help);
        assert!(parse_args(["--bogus"]).is_err());
    }

    #[test]
    fn load_csv_skips_header_and_parses_optional_version() {
        let dir = TempDir::new("csv");
        let csv = dir.path().join("mapping.csv");
        fs::write(
            &csv,
            "drive_file_id,canonical_id,parser_version\n\
             idA,canonA,3\n\
             idB,canonB\n\
             # comment\n\
             \n\
             idC, canonC , 7 \n",
        )
        .unwrap();
        let rows = load_csv(&csv).unwrap();
        assert_eq!(
            rows,
            vec![
                row("idA", "canonA", 3),
                row("idB", "canonB", 0),
                row("idC", "canonC", 7)
            ]
        );
    }

    #[test]
    fn picks_highest_parser_version_and_renames() {
        let dir = TempDir::new("rekey");
        // idA: two candidates, only the higher-version file exists.
        fs::write(dir.path().join("canonA_v1.xlsx"), b"v1").unwrap();
        fs::write(dir.path().join("canonA_v5.xlsx"), b"v5").unwrap();
        // idB: only an older candidate exists; must still map.
        fs::write(dir.path().join("canonB_old.xlsx"), b"b").unwrap();
        // idC: no source.
        fs::write(dir.path().join("orphan.xlsx"), b"o").unwrap(); // no mapping row

        let rows = vec![
            row("idA", "canonA_v1", 1),
            row("idA", "canonA_v5", 5),
            row("idA", "canonA_v9", 9), // version 9 has no file
            row("idB", "canonB_old", 2),
            row("idC", "canonC_missing", 1),
        ];
        let report = rekey(dir.path(), rows, false, false).unwrap();

        assert_eq!(report.mapping_rows, 5);
        assert_eq!(report.drive_file_ids, 3);
        assert_eq!(report.mapped, 2);
        assert_eq!(report.skipped, 0);
        assert_eq!(report.missing, 1);
        assert_eq!(report.orphan, 1);
        assert_eq!(report.missing_ids, vec!["idC"]);
        assert_eq!(report.orphan_files, vec!["orphan.xlsx"]);
        assert_eq!(
            report.mapped_files,
            vec![
                MappedFile {
                    drive_file_id: "idA".into(),
                    canonical_id: "canonA_v5".into(),
                    parser_version: 5
                },
                MappedFile {
                    drive_file_id: "idB".into(),
                    canonical_id: "canonB_old".into(),
                    parser_version: 2
                },
            ]
        );

        let files = ls(dir.path());
        assert!(files.contains("idA.xlsx"));
        assert!(files.contains("idB.xlsx"));
        assert!(!files.contains("canonA_v5.xlsx"), "renamed away");
        assert!(files.contains("canonA_v1.xlsx"), "losing candidate kept");
    }

    #[test]
    fn skips_existing_destination_and_reports_it() {
        let dir = TempDir::new("skip");
        fs::write(dir.path().join("canonA.xlsx"), b"legacy").unwrap();
        fs::write(dir.path().join("idA.xlsx"), b"already new").unwrap();

        let report = rekey(dir.path(), vec![row("idA", "canonA", 1)], false, false).unwrap();
        assert_eq!(report.mapped, 0);
        assert_eq!(report.skipped, 1);
        assert_eq!(report.skipped_ids, vec!["idA"]);
        assert_eq!(
            report.orphan, 0,
            "referenced legacy source is not an orphan"
        );
        assert_eq!(
            fs::read(dir.path().join("idA.xlsx")).unwrap(),
            b"already new"
        );
    }

    #[test]
    fn dry_run_writes_nothing_but_reports() {
        let dir = TempDir::new("dry");
        fs::write(dir.path().join("canonA.xlsx"), b"legacy").unwrap();

        let report = rekey(dir.path(), vec![row("idA", "canonA", 4)], false, true).unwrap();
        assert!(report.dry_run);
        assert_eq!(report.mapped, 1);
        assert_eq!(report.orphan, 0);
        let files = ls(dir.path());
        assert!(files.contains("canonA.xlsx"));
        assert!(!files.contains("idA.xlsx"));
    }

    #[test]
    fn copy_leaves_legacy_source_in_place() {
        let dir = TempDir::new("copy");
        fs::write(dir.path().join("canonA.xlsx"), b"legacy").unwrap();

        let report = rekey(dir.path(), vec![row("idA", "canonA", 4)], true, false).unwrap();
        assert!(report.copied);
        assert_eq!(report.mapped, 1);
        assert_eq!(report.orphan, 0);
        assert!(report.warnings.is_empty());
        let files = ls(dir.path());
        assert!(files.contains("canonA.xlsx"));
        assert_eq!(fs::read(dir.path().join("idA.xlsx")).unwrap(), b"legacy");
    }

    #[test]
    fn copy_preserves_source_mtime() {
        let dir = TempDir::new("copy-mtime");
        let src = dir.path().join("canonA.xlsx");
        let dst = dir.path().join("idA.xlsx");
        fs::write(&src, b"legacy").unwrap();

        // An explicitly old mtime, as if the legacy file was downloaded before
        // a later Drive edit.
        let old = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        fs::File::options()
            .write(true)
            .open(&src)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(SystemTime::from(old)))
            .unwrap();

        let report = rekey(dir.path(), vec![row("idA", "canonA", 4)], true, false).unwrap();
        assert!(report.copied);
        assert_eq!(report.mapped, 1);
        assert!(
            report.warnings.is_empty(),
            "unexpected warnings: {:?}",
            report.warnings
        );

        // The copy carries the source mtime, within filesystem granularity.
        let src_mtime = fs::metadata(&src).unwrap().modified().unwrap();
        let dst_mtime = fs::metadata(&dst).unwrap().modified().unwrap();
        let delta = dst_mtime
            .duration_since(src_mtime)
            .or_else(|_| src_mtime.duration_since(dst_mtime))
            .unwrap();
        assert!(
            delta <= std::time::Duration::from_secs(1),
            "destination mtime {dst_mtime:?} differs from source {src_mtime:?} by {delta:?}"
        );

        // ...so the freshness rule reaches the same verdict for source and copy:
        // fresh against an older Drive modified_time, stale against a newer one.
        let src_dt: DateTime<Utc> = src_mtime.into();
        let dst_dt: DateTime<Utc> = dst_mtime.into();
        for (mtime, label) in [(src_dt, "source"), (dst_dt, "copy")] {
            assert!(
                cache_is_fresh(mtime, Some(old - chrono::Duration::seconds(60))),
                "{label} must be fresh for an older modified_time"
            );
            assert!(
                !cache_is_fresh(mtime, Some(old + chrono::Duration::seconds(60))),
                "{label} must be stale for a newer modified_time"
            );
        }
    }

    #[test]
    fn missing_cache_dir_reports_everything_missing() {
        let parent = TempDir::new("missing");
        let absent = parent.path().join("nope");
        let report = rekey(&absent, vec![row("idA", "canonA", 1)], false, false).unwrap();
        assert_eq!(report.mapped, 0);
        assert_eq!(report.missing, 1);
        assert_eq!(report.orphan, 0);
    }
}
