use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub struct Config {
    pub database_url: String,
    pub bind_addr: String,
    /// How often the WFTDA teams + results sync runs (stages 1-2).
    pub wftda_sync_interval: Duration,
    /// How often the Drive statsbook index runs (stage 3). Independent of, and
    /// typically more frequent than, `wftda_sync_interval`.
    pub drive_index_interval: Duration,
    /// Lookback window for the incremental games sync: scan pages until the
    /// oldest date is older than `max(known date) - lookback`
    /// (`WFTDA_GAMES_LOOKBACK`, default 14d).
    pub wftda_games_lookback: Duration,
    /// How often the games sync forces a full scan, which catches retroactive
    /// score corrections and results entered long after the game
    /// (`WFTDA_FULL_REFRESH_INTERVAL`, default 7d).
    pub wftda_full_refresh_interval: Duration,
    /// Master switch for the WFTDA sync, Drive index, parse, and resolve
    /// background tasks, so offline dev can skip them all and just serve the
    /// web layer.
    pub wftda_sync_enabled: bool,
    pub google_drive_folder_id: String,
    /// Optional on-disk raw-bytes cache for downloaded statsbooks, keyed by
    /// `drive_file_id` (`GAME_DATA_DIR`, e.g. `/data/game-data` in prod). Absent
    /// (or empty) disables the cache and restores always-download behaviour.
    pub game_data_dir: Option<PathBuf>,
    pub service_account: Option<Arc<crate::ingest::drive_auth::ServiceAccount>>,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let sa_path = std::env::var("GOOGLE_SERVICE_ACCOUNT_PATH")
            .ok()
            .map(PathBuf::from);

        let service_account = match &sa_path {
            Some(p) => Some(Arc::new(
                crate::ingest::drive_auth::ServiceAccount::from_file(p)
                    .context("loading service account")?,
            )),
            None => None,
        };

        Ok(Self {
            database_url: std::env::var("DATABASE_URL").context("DATABASE_URL not set")?,
            bind_addr: std::env::var("BIND_ADDR").unwrap_or_else(|_| {
                std::env::var("PORT")
                    .map(|p| format!("0.0.0.0:{p}"))
                    .unwrap_or_else(|_| "0.0.0.0:8080".into())
            }),
            wftda_sync_interval: parse_duration("WFTDA_SYNC_INTERVAL", "24h")?,
            drive_index_interval: parse_duration("DRIVE_INDEX_INTERVAL", "24h")?,
            wftda_games_lookback: parse_duration("WFTDA_GAMES_LOOKBACK", "14d")?,
            wftda_full_refresh_interval: parse_duration("WFTDA_FULL_REFRESH_INTERVAL", "7d")?,
            wftda_sync_enabled: parse_bool("WFTDA_SYNC_ENABLED", true),
            google_drive_folder_id: std::env::var("GOOGLE_DRIVE_FOLDER_ID")
                .unwrap_or_else(|_| "1TC1QUmpIwy9NZX9DBPUPoHjkjFbbzyYr".into()),
            // An empty GAME_DATA_DIR is treated as unset: compose leaves the var
            // defined but empty when no cache volume is mounted.
            game_data_dir: std::env::var("GAME_DATA_DIR")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .map(PathBuf::from),
            service_account,
        })
    }
}

/// Parses the boolean env-var convention used across the app: only the exact
/// values `false`, `0`, and `no` (case-insensitive) are false; everything else
/// is true.
fn bool_from_str(v: &str) -> bool {
    !matches!(v.to_ascii_lowercase().as_str(), "false" | "0" | "no")
}

fn parse_bool(var: &str, default: bool) -> bool {
    std::env::var(var)
        .map(|v| bool_from_str(&v))
        .unwrap_or(default)
}

fn parse_duration(var: &str, default: &str) -> Result<Duration> {
    let s = std::env::var(var).unwrap_or_else(|_| default.into());
    humantime::parse_duration(&s).with_context(|| format!("invalid duration in {var}: {s}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_sa_json(dir: &std::path::Path) -> std::path::PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join("sa.json");
        std::fs::write(
            &p,
            r#"{"client_email":"t@t.iam.gserviceaccount.com","private_key":"-----BEGIN PRIVATE KEY-----\nZmFrZQ==\n-----END PRIVATE KEY-----\n","token_uri":"https://oauth2.googleapis.com/token"}"#,
        )
        .unwrap();
        p
    }

    fn sa_dir() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("statskboo-config-test-{}", std::process::id()))
    }

    #[test]
    fn test_config_from_env() {
        unsafe {
            std::env::set_var("DATABASE_URL", "postgresql://test");
            std::env::remove_var("GOOGLE_SERVICE_ACCOUNT_PATH");
            std::env::remove_var("BIND_ADDR");
            std::env::remove_var("WFTDA_SYNC_INTERVAL");
            std::env::remove_var("DRIVE_INDEX_INTERVAL");
            std::env::remove_var("WFTDA_GAMES_LOOKBACK");
            std::env::remove_var("WFTDA_FULL_REFRESH_INTERVAL");
            std::env::remove_var("WFTDA_SYNC_ENABLED");
            std::env::remove_var("GAME_DATA_DIR");

            // defaults: no service account is fine (the Drive loop no-ops with
            // a warning), and both sync intervals default to daily.
            let cfg = Config::from_env().unwrap();
            assert!(cfg.service_account.is_none());
            assert!(cfg.wftda_sync_enabled);
            assert_eq!(cfg.wftda_sync_interval, Duration::from_secs(24 * 3600));
            assert_eq!(cfg.drive_index_interval, Duration::from_secs(24 * 3600));
            assert_eq!(
                cfg.wftda_games_lookback,
                Duration::from_secs(14 * 24 * 3600)
            );
            assert_eq!(
                cfg.wftda_full_refresh_interval,
                Duration::from_secs(7 * 24 * 3600)
            );
            // No GAME_DATA_DIR -> the on-disk byte cache is disabled.
            assert!(cfg.game_data_dir.is_none());

            // GAME_DATA_DIR enables the cache; the legacy compose var name is
            // kept so a prod volume keeps working.
            std::env::set_var("GAME_DATA_DIR", "/data/game-data");
            assert_eq!(
                Config::from_env().unwrap().game_data_dir,
                Some(PathBuf::from("/data/game-data"))
            );
            // An empty value is treated as unset (compose may define it empty).
            std::env::set_var("GAME_DATA_DIR", "   ");
            assert!(Config::from_env().unwrap().game_data_dir.is_none());
            std::env::remove_var("GAME_DATA_DIR");

            // SA configured -> loaded from the JSON file
            let sa = write_sa_json(&sa_dir());
            std::env::set_var("GOOGLE_SERVICE_ACCOUNT_PATH", &sa);
            let cfg = Config::from_env().unwrap();
            let sa = cfg.service_account.unwrap();
            assert_eq!(sa.client_email, "t@t.iam.gserviceaccount.com");
            assert_eq!(sa.token_uri, "https://oauth2.googleapis.com/token");

            // WFTDA sync / Drive index scheduling (decision 8)

            std::env::set_var("WFTDA_SYNC_INTERVAL", "6h");
            std::env::set_var("DRIVE_INDEX_INTERVAL", "30m");
            std::env::set_var("WFTDA_GAMES_LOOKBACK", "3d");
            std::env::set_var("WFTDA_FULL_REFRESH_INTERVAL", "2d");
            std::env::set_var("WFTDA_SYNC_ENABLED", "false");
            let cfg = Config::from_env().unwrap();
            assert_eq!(cfg.wftda_sync_interval, Duration::from_secs(6 * 3600));
            assert_eq!(cfg.drive_index_interval, Duration::from_secs(30 * 60));
            assert_eq!(cfg.wftda_games_lookback, Duration::from_secs(3 * 24 * 3600));
            assert_eq!(
                cfg.wftda_full_refresh_interval,
                Duration::from_secs(2 * 24 * 3600)
            );
            assert!(!cfg.wftda_sync_enabled);

            std::env::set_var("WFTDA_SYNC_ENABLED", "yes");
            assert!(Config::from_env().unwrap().wftda_sync_enabled);

            std::env::set_var("WFTDA_SYNC_INTERVAL", "not-a-duration");
            assert!(Config::from_env().is_err());
            std::env::remove_var("WFTDA_SYNC_INTERVAL");
            std::env::remove_var("DRIVE_INDEX_INTERVAL");
            std::env::remove_var("WFTDA_GAMES_LOOKBACK");
            std::env::remove_var("WFTDA_FULL_REFRESH_INTERVAL");
            std::env::remove_var("WFTDA_SYNC_ENABLED");

            std::env::remove_var("GOOGLE_SERVICE_ACCOUNT_PATH");
            std::fs::remove_dir_all(sa_dir()).ok();
        }
    }

    #[test]
    fn test_bool_from_str() {
        for v in ["false", "FALSE", "False", "0", "no", "No", "NO"] {
            assert!(!bool_from_str(v), "{v:?} should parse as false");
        }
        for v in ["true", "1", "yes", "garbage", ""] {
            assert!(bool_from_str(v), "{v:?} should parse as true");
        }
    }
}
