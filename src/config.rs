use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub struct Config {
    pub database_url: String,
    pub bind_addr: String,
    pub ingest_interval: Duration,
    pub ingest_jitter: Duration,
    pub ingest_enabled: bool,
    pub ingest_dir: Option<String>,
    pub game_data_dir: Option<PathBuf>,
    pub google_drive_folder_id: String,
    pub service_account: Option<Arc<crate::ingest::drive_auth::ServiceAccount>>,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let ingest_dir = std::env::var("INGEST_DIR").ok();
        let game_data_dir = std::env::var("GAME_DATA_DIR").ok().map(PathBuf::from);
        let sa_path = std::env::var("GOOGLE_SERVICE_ACCOUNT_PATH")
            .ok()
            .map(PathBuf::from);
        let ingest_enabled = std::env::var("INGEST_ENABLED")
            .map(|v| !matches!(v.to_ascii_lowercase().as_str(), "false" | "0" | "no"))
            .unwrap_or(true);

        if ingest_enabled {
            anyhow::ensure!(
                ingest_dir.is_some() ^ sa_path.is_some(),
                "exactly one of INGEST_DIR or GOOGLE_SERVICE_ACCOUNT_PATH must be set"
            );
        }

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
            ingest_interval: parse_duration("INGEST_INTERVAL", "24h")?,
            ingest_jitter: parse_duration("INGEST_JITTER", "1h")?,
            ingest_enabled,
            ingest_dir,
            game_data_dir,
            google_drive_folder_id: std::env::var("GOOGLE_DRIVE_FOLDER_ID")
                .unwrap_or_else(|_| "1TC1QUmpIwy9NZX9DBPUPoHjkjFbbzyYr".into()),
            service_account,
        })
    }
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
            std::env::remove_var("INGEST_DIR");
            std::env::remove_var("GOOGLE_SERVICE_ACCOUNT_PATH");
            std::env::remove_var("BIND_ADDR");
            std::env::remove_var("INGEST_INTERVAL");
            std::env::remove_var("INGEST_JITTER");
            std::env::remove_var("INGEST_ENABLED");

            // defaults: neither INGEST_DIR nor SA set -> error
            assert!(Config::from_env().is_err());

            // INGEST_DIR set, no SA
            std::env::set_var("INGEST_DIR", "/tmp/xlsx");
            let cfg = Config::from_env().unwrap();
            assert_eq!(cfg.ingest_dir.as_deref(), Some("/tmp/xlsx"));
            assert!(cfg.service_account.is_none());
            assert!(cfg.ingest_enabled);

            // both INGEST_DIR and SA set -> error
            let sa = write_sa_json(&sa_dir());
            std::env::set_var("GOOGLE_SERVICE_ACCOUNT_PATH", &sa);
            assert!(Config::from_env().is_err());

            // SA set, no INGEST_DIR
            std::env::remove_var("INGEST_DIR");
            let cfg = Config::from_env().unwrap();
            let sa = cfg.service_account.unwrap();
            assert_eq!(sa.client_email, "t@t.iam.gserviceaccount.com");
            assert_eq!(sa.token_uri, "https://oauth2.googleapis.com/token");

            // INGEST_ENABLED parsing
            std::env::set_var("INGEST_ENABLED", "false");
            assert!(!Config::from_env().unwrap().ingest_enabled);
            std::env::set_var("INGEST_ENABLED", "0");
            assert!(!Config::from_env().unwrap().ingest_enabled);
            std::env::set_var("INGEST_ENABLED", "no");
            assert!(!Config::from_env().unwrap().ingest_enabled);
            std::env::set_var("INGEST_ENABLED", "FALSE");
            assert!(!Config::from_env().unwrap().ingest_enabled);
            std::env::set_var("INGEST_ENABLED", "No");
            assert!(!Config::from_env().unwrap().ingest_enabled);
            std::env::set_var("INGEST_ENABLED", "garbage");
            assert!(Config::from_env().unwrap().ingest_enabled);
            std::env::remove_var("INGEST_ENABLED");
            assert!(Config::from_env().unwrap().ingest_enabled);

            std::env::remove_var("GOOGLE_SERVICE_ACCOUNT_PATH");
            std::fs::remove_dir_all(sa_dir()).ok();
        }
    }
}
