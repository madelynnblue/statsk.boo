use crate::ingest::drive_auth::ServiceAccount;
use anyhow::Result;
use reqwest::Client;
use reqwest::header;
use serde::Deserialize;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::Semaphore;

const SHEETS_MIME: &str = "application/vnd.google-apps.spreadsheet";
const XLSX_MIME: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
const INITIAL_RETRY_DELAY: Duration = Duration::from_secs(10);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(60);
const MAX_ATTEMPTS: u32 = 5;

#[derive(Debug, Deserialize)]
pub struct DriveFile {
    pub id: String,
    pub name: String,
    #[serde(rename = "modifiedTime")]
    pub modified_time: String,
    #[serde(rename = "mimeType")]
    pub mime_type: Option<String>,
}

#[derive(Deserialize)]
struct FileList {
    files: Vec<DriveFile>,
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
}

pub struct DriveClient {
    client: Client,
    sa: Arc<ServiceAccount>,
    /// Cached OAuth2 access token; `None` until first fetch / after a 401.
    token: Mutex<Option<String>>,
    /// Serializes all HTTP requests to Google Drive so that only one is in
    /// flight at any time, avoiding rate limits and 403/429 responses.
    request_sem: Semaphore,
}

static APP_USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"),);

/// Distinguishes retryable (transient) HTTP errors from permanent ones so the
/// retry loop can give up immediately on 400/404 etc.
enum FetchError {
    Transient(String),
    Permanent(String),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Transient(s) | FetchError::Permanent(s) => write!(f, "{s}"),
        }
    }
}

fn is_transient_http_status(status: reqwest::StatusCode, body: &str) -> bool {
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
        return true;
    }
    // Google Drive overloads 403 for both rate limits (transient) and
    // permission/access errors (permanent).  Check the JSON error reason.
    if status == reqwest::StatusCode::FORBIDDEN {
        return body.contains("rateLimitExceeded")
            || body.contains("userRateLimitExceeded")
            || body.contains("dailyLimitExceeded");
    }
    false
}

/// Read a snippet of the response body for error diagnostics.  Google error
/// responses are small JSON payloads.
async fn read_error_body(resp: reqwest::Response) -> String {
    resp.text()
        .await
        .unwrap_or_else(|e| format!("(body unreadable: {e})"))
}

/// Retry transient errors (403, 429, 5xx, network failures) with exponential
/// backoff: 10 s → 20 s → 40 s → 60 s (capped).  After `MAX_ATTEMPTS` total
/// attempts the error is returned so the caller can skip the file and move on.
/// Permanent client errors (400, 404, …) are returned immediately.
async fn with_retry<T, F, Fut>(f: F) -> Result<T>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = std::result::Result<T, FetchError>>,
{
    let mut delay = INITIAL_RETRY_DELAY;
    let mut attempts: u32 = 0;

    loop {
        attempts += 1;
        match f().await {
            Ok(val) => return Ok(val),
            Err(FetchError::Permanent(e)) => return Err(anyhow::anyhow!("{e}")),
            Err(FetchError::Transient(e)) => {
                if attempts >= MAX_ATTEMPTS {
                    return Err(anyhow::anyhow!("{e}"));
                }
                tracing::warn!(
                    "transient drive API error (attempt {attempts}/{MAX_ATTEMPTS}), retrying in {delay:?}: {e}",
                );
                tokio::time::sleep(delay).await;
                delay = std::cmp::min(delay * 2, MAX_RETRY_DELAY);
            }
        }
    }
}

impl DriveClient {
    pub fn new(sa: Arc<ServiceAccount>) -> Self {
        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::USER_AGENT,
            header::HeaderValue::from_static(APP_USER_AGENT),
        );
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .default_headers(headers)
            .build()
            .expect("reqwest client builder should not fail");
        Self {
            client,
            sa,
            token: Mutex::new(None),
            request_sem: Semaphore::new(1),
        }
    }

    async fn ensure_token(&self) -> Result<String, FetchError> {
        if let Some(t) = self.token.lock().unwrap().clone() {
            return Ok(t);
        }
        let t = self
            .sa
            .fetch_access_token(&self.client)
            .await
            .map_err(|e| FetchError::Transient(format!("token fetch failed: {e:#}")))?;
        *self.token.lock().unwrap() = Some(t.clone());
        Ok(t)
    }

    fn invalidate_token(&self) {
        *self.token.lock().unwrap() = None;
    }

    /// Lists files in the two-level folder tree under `folder_id` with paths
    /// relative to it, e.g. "2023-Q1/[WFTDA]STATS-....xlsx". One list query
    /// per subfolder (plus one for the subfolder listing; pagination handled
    /// internally by list_items). Files sitting directly in the root folder
    /// are not listed. Inherits the request serialization and retry logic.
    /// Used by the statsbook index (redesign stage 3).
    pub async fn list_tree_with_paths(&self, folder_id: &str) -> Result<Vec<(String, DriveFile)>> {
        let subfolders = self.list_items(folder_id, true).await?;
        let mut out = Vec::new();
        for folder in subfolders {
            let files = self.list_items(&folder.id, false).await?;
            for f in files {
                out.push((format!("{}/{}", folder.name, f.name), f));
            }
        }
        Ok(out)
    }

    async fn list_items(&self, folder_id: &str, folders_only: bool) -> Result<Vec<DriveFile>> {
        let mime = if folders_only {
            "mimeType = 'application/vnd.google-apps.folder'"
        } else {
            "mimeType != 'application/vnd.google-apps.folder'"
        };
        let q = format!("'{folder_id}' in parents and {mime}");

        let mut all_files = Vec::new();
        let mut page_token: Option<String> = None;

        loop {
            let resp: FileList = self.send_list_request(&q, &page_token).await?;
            all_files.extend(resp.files);
            match resp.next_page_token {
                Some(t) => page_token = Some(t),
                None => break,
            }
        }
        Ok(all_files)
    }

    async fn send_list_request(&self, q: &str, page_token: &Option<String>) -> Result<FileList> {
        with_retry(|| async {
            // Serialize all HTTP requests — only one drive API call at a time.
            let _permit = self
                .request_sem
                .acquire()
                .await
                .expect("request semaphore closed");

            let token = self.ensure_token().await?;
            let mut req = self
                .client
                .get("https://www.googleapis.com/drive/v3/files")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .query(&[
                    ("q", q),
                    (
                        "fields",
                        "nextPageToken,files(id,name,modifiedTime,mimeType)",
                    ),
                    ("pageSize", "1000"),
                ]);
            if let Some(token) = page_token {
                req = req.query(&[("pageToken", token.as_str())]);
            }

            let resp = req
                .send()
                .await
                .map_err(|e| FetchError::Transient(format!("network error: {e}")))?;

            let status = resp.status();
            if status == reqwest::StatusCode::UNAUTHORIZED {
                // Token expired or invalid: drop it and let the retry loop
                // fetch a fresh one on the next attempt.
                self.invalidate_token();
                let body = read_error_body(resp).await;
                return Err(FetchError::Transient(format!(
                    "HTTP 401 (token expired): {body}"
                )));
            }
            if !status.is_success() {
                let body = read_error_body(resp).await;
                let msg = format!("HTTP {status}: {body}");
                if is_transient_http_status(status, &body) {
                    return Err(FetchError::Transient(msg));
                }
                return Err(FetchError::Permanent(msg));
            }

            resp.json::<FileList>()
                .await
                .map_err(|e| FetchError::Transient(format!("parse error: {e}")))
        })
        .await
    }

    pub async fn download_file(&self, file_id: &str, mime_type: Option<&str>) -> Result<Vec<u8>> {
        let file_id = file_id.to_string();
        let is_sheets = mime_type == Some(SHEETS_MIME);

        with_retry(|| async {
            // Serialize all HTTP requests — only one drive API call at a time.
            let _permit = self
                .request_sem
                .acquire()
                .await
                .expect("request semaphore closed");

            let token = self.ensure_token().await?;
            let req = if is_sheets {
                self.client
                    .get(format!(
                        "https://www.googleapis.com/drive/v3/files/{file_id}/export"
                    ))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .query(&[("mimeType", XLSX_MIME)])
            } else {
                self.client
                    .get(format!(
                        "https://www.googleapis.com/drive/v3/files/{file_id}"
                    ))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    // alt=media is required: without it the endpoint returns file metadata JSON, not raw bytes.
                    .query(&[("alt", "media")])
            };

            let resp = req
                .send()
                .await
                .map_err(|e| FetchError::Transient(format!("network error: {e}")))?;

            let status = resp.status();
            if status == reqwest::StatusCode::UNAUTHORIZED {
                // Token expired or invalid: drop it and let the retry loop
                // fetch a fresh one on the next attempt.
                self.invalidate_token();
                let body = read_error_body(resp).await;
                return Err(FetchError::Transient(format!(
                    "HTTP 401 (token expired): {body}"
                )));
            }
            if !status.is_success() {
                let body = read_error_body(resp).await;
                let msg = format!("HTTP {status}: {body}");
                if is_transient_http_status(status, &body) {
                    return Err(FetchError::Transient(msg));
                }
                return Err(FetchError::Permanent(msg));
            }

            resp.bytes()
                .await
                .map(|b| b.to_vec())
                .map_err(|e| FetchError::Transient(format!("body read error: {e}")))
        })
        .await
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn test_unauthorized_is_transient() {
        use reqwest::StatusCode;

        // 401 is handled explicitly in the request paths (token refresh),
        // so it never reaches the generic classifier; the classifier itself
        // still treats it as permanent if it somehow does.
        assert!(!super::is_transient_http_status(
            StatusCode::UNAUTHORIZED,
            ""
        ));
    }

    #[test]
    fn test_is_transient_http_status() {
        use reqwest::StatusCode;

        // Transient: 429, 5xx
        assert!(super::is_transient_http_status(
            StatusCode::TOO_MANY_REQUESTS,
            ""
        ));
        assert!(super::is_transient_http_status(
            StatusCode::INTERNAL_SERVER_ERROR,
            ""
        ));
        assert!(super::is_transient_http_status(StatusCode::BAD_GATEWAY, ""));
        assert!(super::is_transient_http_status(
            StatusCode::SERVICE_UNAVAILABLE,
            ""
        ));
        assert!(super::is_transient_http_status(
            StatusCode::GATEWAY_TIMEOUT,
            ""
        ));

        // Permanent: 400, 401, 404
        assert!(!super::is_transient_http_status(
            StatusCode::BAD_REQUEST,
            ""
        ));
        assert!(!super::is_transient_http_status(
            StatusCode::UNAUTHORIZED,
            ""
        ));
        assert!(!super::is_transient_http_status(StatusCode::NOT_FOUND, ""));

        // 403: depends on body content
        assert!(super::is_transient_http_status(
            StatusCode::FORBIDDEN,
            r#"{"error":{"errors":[{"reason":"rateLimitExceeded"}]}}"#
        ));
        assert!(super::is_transient_http_status(
            StatusCode::FORBIDDEN,
            r#"{"error":{"errors":[{"reason":"userRateLimitExceeded"}]}}"#
        ));
        assert!(super::is_transient_http_status(
            StatusCode::FORBIDDEN,
            r#"{"error":{"errors":[{"reason":"dailyLimitExceeded"}]}}"#
        ));
        assert!(!super::is_transient_http_status(
            StatusCode::FORBIDDEN,
            r#"{"error":{"errors":[{"reason":"forbidden"}]}}"#
        ));
        assert!(!super::is_transient_http_status(StatusCode::FORBIDDEN, ""));
        assert!(!super::is_transient_http_status(StatusCode::OK, ""));
    }
}
