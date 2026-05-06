use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use reqwest::Client;
use reqwest::header;
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct DriveFile {
    pub id: String,
    pub name: String,
    #[serde(rename = "modifiedTime")]
    pub modified_time: String,
}

impl DriveFile {
    pub fn from_local(path: &Path, root: &Path) -> Result<Self> {
        let rel = path.strip_prefix(root).with_context(|| {
            format!("path {} not under root {}", path.display(), root.display())
        })?;
        let id = rel.to_string_lossy().to_string();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| id.clone());
        let meta = std::fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
        let mtime = meta
            .modified()
            .with_context(|| format!("mtime {}", path.display()))?;
        let modified_time =
            DateTime::<Utc>::from(mtime).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        Ok(Self {
            id,
            name,
            modified_time,
        })
    }
}

#[derive(Deserialize)]
struct FileList {
    files: Vec<DriveFile>,
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
}

pub struct DriveClient {
    client: Client,
    api_key: String,
}

static APP_USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"),);

impl DriveClient {
    pub fn new(api_key: String) -> Self {
        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::USER_AGENT,
            header::HeaderValue::from_static(APP_USER_AGENT),
        );
        let client = Client::builder()
            .default_headers(headers)
            .build()
            .expect("reqwest client builder should not fail");
        Self { client, api_key }
    }

    pub async fn list_all_xlsx(&self, folder_id: &str) -> Result<Vec<DriveFile>> {
        let subfolders = self.list_items(folder_id, true, None).await?;
        let mut all = Vec::new();
        for folder in subfolders {
            let files = self.list_items(&folder.id, false, None).await?;
            all.extend(files.into_iter().filter(|f| f.name.ends_with(".xlsx")));
        }
        let root_files = self.list_items(folder_id, false, None).await?;
        all.extend(root_files.into_iter().filter(|f| f.name.ends_with(".xlsx")));
        Ok(all)
    }

    pub async fn list_xlsx_since(&self, folder_id: &str, since: &str) -> Result<Vec<DriveFile>> {
        let mut all = Vec::new();
        let subfolders = self.list_items(folder_id, true, None).await?;
        for folder in subfolders {
            let files = self.list_items(&folder.id, false, Some(since)).await?;
            all.extend(files);
        }
        Ok(all)
    }

    async fn list_items(
        &self,
        folder_id: &str,
        folders_only: bool,
        since: Option<&str>,
    ) -> Result<Vec<DriveFile>> {
        let mime = if folders_only {
            "mimeType = 'application/vnd.google-apps.folder'"
        } else {
            "mimeType != 'application/vnd.google-apps.folder'"
        };
        let since_clause = since
            .map(|s| format!(" and modifiedTime > '{s}'"))
            .unwrap_or_default();
        let q = format!("'{folder_id}' in parents and {mime}{since_clause}");

        let mut all_files = Vec::new();
        let mut page_token: Option<String> = None;

        loop {
            let mut req = self
                .client
                .get("https://www.googleapis.com/drive/v3/files")
                .query(&[
                    ("q", q.as_str()),
                    ("key", &self.api_key),
                    ("fields", "nextPageToken,files(id,name,modifiedTime)"),
                    ("pageSize", "1000"),
                ]);
            if let Some(token) = &page_token {
                req = req.query(&[("pageToken", token.as_str())]);
            }
            let resp: FileList = req
                .send()
                .await?
                .error_for_status()
                .context("Drive API error")?
                .json()
                .await?;
            all_files.extend(resp.files);
            match resp.next_page_token {
                Some(t) => page_token = Some(t),
                None => break,
            }
        }
        Ok(all_files)
    }

    pub async fn download_file(&self, file_id: &str) -> Result<Vec<u8>> {
        let bytes = self
            .client
            .get(format!(
                "https://www.googleapis.com/drive/v3/files/{file_id}"
            ))
            .query(&[("alt", "media"), ("key", &self.api_key)])
            .send()
            .await?
            .error_for_status()
            .context("Drive download error")?
            .bytes()
            .await?;
        Ok(bytes.to_vec())
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn test_since_clause_format() {
        let since = "2024-01-01T00:00:00Z";
        let expected = " and modifiedTime > '2024-01-01T00:00:00Z'";
        let clause = Some(since)
            .map(|s| format!(" and modifiedTime > '{s}'"))
            .unwrap_or_default();
        assert_eq!(clause, expected);
    }

    #[test]
    fn test_no_since_clause() {
        let clause: String = None::<&str>
            .map(|s| format!(" and modifiedTime > '{s}'"))
            .unwrap_or_default();
        assert_eq!(clause, "");
    }
}
