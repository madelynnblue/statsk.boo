use anyhow::{Context, Result};
use std::time::Duration;

pub struct Config {
    pub database_url: String,
    pub bind_addr: String,
    pub ingest_interval: Duration,
    pub ingest_jitter: Duration,
    pub ingest_dir: Option<String>,
    pub google_drive_folder_id: String,
    pub google_api_key: Option<String>,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let ingest_dir = std::env::var("INGEST_DIR").ok();
        let google_api_key = std::env::var("GOOGLE_API_KEY").ok();

        anyhow::ensure!(
            ingest_dir.is_some() ^ google_api_key.is_some(),
            "exactly one of INGEST_DIR or GOOGLE_API_KEY must be set"
        );

        Ok(Self {
            database_url: std::env::var("DATABASE_URL").context("DATABASE_URL not set")?,
            bind_addr: std::env::var("BIND_ADDR").unwrap_or_else(|_| {
                std::env::var("PORT")
                    .map(|p| format!("0.0.0.0:{p}"))
                    .unwrap_or_else(|_| "0.0.0.0:8080".into())
            }),
            ingest_interval: parse_duration("INGEST_INTERVAL", "24h")?,
            ingest_jitter: parse_duration("INGEST_JITTER", "1h")?,
            ingest_dir,
            google_drive_folder_id: std::env::var("GOOGLE_DRIVE_FOLDER_ID")
                .unwrap_or_else(|_| "1TC1QUmpIwy9NZX9DBPUPoHjkjFbbzyYr".into()),
            google_api_key,
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

    #[test]
    fn test_config_from_env() {
        unsafe {
            // defaults: only GOOGLE_API_KEY set
            std::env::set_var("DATABASE_URL", "postgresql://test");
            std::env::set_var("GOOGLE_API_KEY", "key123");
            std::env::remove_var("INGEST_DIR");
            std::env::remove_var("BIND_ADDR");
            std::env::remove_var("INGEST_INTERVAL");
            std::env::remove_var("INGEST_JITTER");
            let cfg = Config::from_env().unwrap();
            assert_eq!(cfg.bind_addr, "0.0.0.0:8080");
            assert_eq!(cfg.ingest_interval.as_secs(), 86400); // 24h
            assert_eq!(cfg.ingest_jitter.as_secs(), 3600); // 1h
            assert_eq!(cfg.ingest_dir, None);
            assert_eq!(cfg.google_api_key.as_deref(), Some("key123"));

            // INGEST_DIR set, no API key
            std::env::set_var("INGEST_DIR", "/tmp/xlsx");
            std::env::remove_var("GOOGLE_API_KEY");
            let cfg = Config::from_env().unwrap();
            assert_eq!(cfg.ingest_dir.as_deref(), Some("/tmp/xlsx"));
            assert_eq!(cfg.google_api_key, None);

            // neither set → error
            std::env::remove_var("INGEST_DIR");
            std::env::remove_var("GOOGLE_API_KEY");
            assert!(Config::from_env().is_err());

            // both set → error
            std::env::set_var("INGEST_DIR", "/tmp/xlsx");
            std::env::set_var("GOOGLE_API_KEY", "key123");
            assert!(Config::from_env().is_err());
        }
    }
}
