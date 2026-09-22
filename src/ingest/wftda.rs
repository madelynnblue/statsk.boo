//! WFTDA reference sync (redesign stages 1-2).
//!
//! Stage 1 (`sync_teams`) reads the public `/api/teams` JSON plus the six
//! `/teams?page=N` listing pages and upserts `wftda_teams`. Stage 2
//! (`sync_games`) reads `/api/results` (date-DESC, 64/page) from the top until a
//! page's oldest date falls before the statsbook era, and upserts `wftda_games`,
//! creating `in_teams_api=false` stub team rows for ids the teams API does not
//! know about.
//!
//! The endpoints are undocumented and rate-limited (a 6-way concurrent scan
//! gets HTTP 429), so every request is serialized behind a semaphore, spaced by
//! a small fixed delay, and retried with exponential backoff on 429/5xx —
//! following the polite-client pattern in `drive.rs`.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, NaiveDate, Utc};
use reqwest::Client;
use reqwest::header;
use serde::Deserialize;
use sqlx::PgPool;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::Semaphore;
use tracing::{info, warn};

const BASE_URL: &str = "https://stats.wftda.com";
/// The statsbook era (decision 3): only games from 2023-01-01 onward are in scope.
const DATE_FORMAT: &str = "%d %b %Y";
/// Fixed spacing between requests; the API 429s on concurrent fetches.
const REQUEST_DELAY: Duration = Duration::from_millis(400);
const INITIAL_RETRY_DELAY: Duration = Duration::from_secs(2);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(60);
const MAX_ATTEMPTS: u32 = 5;
/// `/teams?page=N` lists 100 teams/page; there have been 6 pages for a while.
/// An empty page stops the loop early, so this is only an upper bound.
const TEAM_LISTING_PAGES: u32 = 12;

static APP_USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));

/// 2023-01-01, the start of the statsbook era.
fn statsbook_era_start() -> NaiveDate {
    NaiveDate::from_ymd_opt(2023, 1, 1).expect("2023-01-01 is a valid date")
}

pub struct WftdaClient {
    client: Client,
    /// Serializes all requests: one in flight at a time, like `DriveClient`.
    requests: Semaphore,
    delay: Duration,
}

impl WftdaClient {
    pub fn new() -> Self {
        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::USER_AGENT,
            header::HeaderValue::from_static(APP_USER_AGENT),
        );
        headers.insert(
            header::ACCEPT,
            header::HeaderValue::from_static("application/json, text/html;q=0.9"),
        );
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .default_headers(headers)
            .build()
            .expect("reqwest client builder should not fail");
        Self {
            client,
            requests: Semaphore::new(1),
            delay: REQUEST_DELAY,
        }
    }

    /// GET `url` as text, retrying rate limits and transient server/network
    /// failures with exponential backoff. Every request (including retries) is
    /// spaced by `self.delay`.
    async fn get_text(&self, url: &str) -> Result<String> {
        let mut backoff = INITIAL_RETRY_DELAY;
        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            let _permit = self
                .requests
                .acquire()
                .await
                .expect("request semaphore closed");
            tokio::time::sleep(self.delay).await;

            let retryable = match self.client.get(url).send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        return resp
                            .text()
                            .await
                            .with_context(|| format!("reading body of {url}"));
                    }
                    let body = resp.text().await.unwrap_or_default();
                    if is_transient(status) {
                        warn!(
                            "WFTDA {url} returned HTTP {status} (attempt {attempt}/{MAX_ATTEMPTS}): {}",
                            snippet(&body, 200)
                        );
                        true
                    } else {
                        bail!(
                            "WFTDA {url} returned HTTP {status}: {}",
                            snippet(&body, 200)
                        );
                    }
                }
                Err(e) => {
                    warn!("WFTDA {url} request failed (attempt {attempt}/{MAX_ATTEMPTS}): {e}");
                    true
                }
            };

            if !retryable || attempt >= MAX_ATTEMPTS {
                bail!("WFTDA {url} failed after {attempt} attempt(s)");
            }
            tokio::time::sleep(backoff).await;
            backoff = std::cmp::min(backoff * 2, MAX_RETRY_DELAY);
        }
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        let body = self.get_text(url).await?;
        serde_json::from_str(&body).with_context(|| format!("parsing JSON from {url}"))
    }
}

impl Default for WftdaClient {
    fn default() -> Self {
        Self::new()
    }
}

fn is_transient(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status.is_server_error()
}

fn snippet(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

#[derive(Debug, Deserialize)]
struct ApiEnvelope<T> {
    success: bool,
    data: T,
}

#[derive(Debug, Deserialize)]
struct ApiPage<T> {
    data: Vec<T>,
    #[serde(default)]
    last_page: u32,
}

#[derive(Debug, Deserialize)]
struct ApiTeam {
    /// CockroachDB `INT` is 64-bit, so WFTDA ids are `i64` all the way through.
    id: i64,
    title: String,
    short_name: Option<String>,
    region: Option<String>,
    region_id: Option<i64>,
    location: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiResult {
    id: i64,
    date: String,
    team_1: ApiResultTeam,
    team_2: ApiResultTeam,
    region: Option<i64>,
    #[serde(default)]
    seeding_game: bool,
}

#[derive(Debug, Deserialize)]
struct ApiResultTeam {
    id: i64,
    title: String,
    score: Option<i32>,
}

/// Stage 1: teams from `/api/teams`, with slugs joined from the `/teams`
/// listing by exact title. Upserts on `id`; existing stubs are upgraded to
/// `in_teams_api=true`.
pub async fn sync_teams(pool: &PgPool, client: &WftdaClient) -> Result<usize> {
    let envelope: ApiEnvelope<ApiPage<ApiTeam>> =
        client.get_json(&format!("{BASE_URL}/api/teams")).await?;
    if !envelope.success {
        bail!("WFTDA /api/teams returned success=false");
    }
    let teams = envelope.data.data;
    info!("WFTDA /api/teams: {} team(s)", teams.len());

    let mut slugs: HashMap<String, String> = HashMap::new();
    for page in 1..=TEAM_LISTING_PAGES {
        let html = client
            .get_text(&format!("{BASE_URL}/teams?page={page}"))
            .await?;
        let pairs = parse_team_listing(&html);
        if pairs.is_empty() {
            info!("WFTDA /teams page {page}: no teams listed, stopping");
            break;
        }
        for (slug, title) in pairs {
            slugs.insert(title, slug);
        }
    }
    info!(
        "WFTDA /teams listing: {} distinct title(s) with a slug",
        slugs.len()
    );

    let mut with_slug = 0usize;
    let mut tx = pool.begin().await?;
    for team in &teams {
        let slug = slugs.get(&team.title);
        if slug.is_some() {
            with_slug += 1;
        }
        sqlx::query!(
            r#"INSERT INTO wftda_teams
                 (id, title, short_name, slug, region, region_id, location, in_teams_api, fetched_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, true, now())
               ON CONFLICT (id) DO UPDATE SET
                 title        = EXCLUDED.title,
                 short_name   = EXCLUDED.short_name,
                 slug         = COALESCE(EXCLUDED.slug, wftda_teams.slug),
                 region       = EXCLUDED.region,
                 region_id    = EXCLUDED.region_id,
                 location     = EXCLUDED.location,
                 in_teams_api = true,
                 fetched_at   = now()"#,
            team.id,
            team.title,
            team.short_name,
            slug.map(|s| s.as_str()),
            team.region,
            team.region_id,
            team.location,
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    info!(
        "wftda_teams: upserted {} team(s), {} with a listing slug",
        teams.len(),
        with_slug
    );
    Ok(teams.len())
}

/// Stage 2: games from `/api/results`, stopping once a page's oldest date is
/// before `stop_before` (the statsbook era for a full scan, or
/// `max(known date) - lookback` for an incremental one). Games older than the
/// statsbook era are never stored. Stub-upserts every referenced team before
/// writing the games so the foreign keys always resolve.
pub async fn sync_games(
    pool: &PgPool,
    client: &WftdaClient,
    stop_before: NaiveDate,
) -> Result<usize> {
    let era = statsbook_era_start();
    info!("WFTDA /api/results: scanning until oldest < {stop_before}");
    let mut games: Vec<GameRow> = Vec::new();
    let mut stubs: HashMap<i64, String> = HashMap::new();
    let mut page: u32 = 1;

    loop {
        let envelope: ApiEnvelope<ApiPage<ApiResult>> = client
            .get_json(&format!("{BASE_URL}/api/results?page={page}"))
            .await?;
        if !envelope.success {
            bail!("WFTDA /api/results?page={page} returned success=false");
        }
        let rows = envelope.data.data;
        if rows.is_empty() {
            info!("WFTDA /api/results page {page}: empty page, stopping");
            break;
        }

        let mut oldest: Option<NaiveDate> = None;
        let mut kept = 0usize;
        for r in &rows {
            let date =
                NaiveDate::parse_from_str(r.date.trim(), DATE_FORMAT).with_context(|| {
                    format!("parsing WFTDA result date {:?} (game {})", r.date, r.id)
                })?;
            oldest = Some(match oldest {
                Some(o) => o.min(date),
                None => date,
            });
            // The API is date-DESC, so only the boundary page can dip below the
            // cutoff; those games are out of scope (decision 3).
            if date < era {
                continue;
            }
            let (Some(score_1), Some(score_2)) = (r.team_1.score, r.team_2.score) else {
                warn!(
                    "skipping WFTDA game {} ({}): missing score ({:?} vs {:?})",
                    r.id, date, r.team_1.score, r.team_2.score
                );
                continue;
            };
            let (Ok(score_1), Ok(score_2)) = (i16::try_from(score_1), i16::try_from(score_2))
            else {
                warn!("skipping WFTDA game {} ({date}): score out of range", r.id);
                continue;
            };
            stubs
                .entry(r.team_1.id)
                .or_insert_with(|| r.team_1.title.clone());
            stubs
                .entry(r.team_2.id)
                .or_insert_with(|| r.team_2.title.clone());
            games.push(GameRow {
                id: r.id,
                date,
                team_1_id: r.team_1.id,
                team_2_id: r.team_2.id,
                team_1_score: score_1,
                team_2_score: score_2,
                region: r.region,
                seeding_game: r.seeding_game,
            });
            kept += 1;
        }

        let last_page = envelope.data.last_page.max(page);
        let reached_cutoff = oldest.is_some_and(|o| o < stop_before);
        info!(
            "WFTDA /api/results page {page}/{last_page}: kept {kept}/{} game(s) (oldest {})",
            rows.len(),
            oldest.map(|d| d.to_string()).unwrap_or_default()
        );
        if reached_cutoff || page >= last_page {
            break;
        }
        page += 1;
    }

    let mut tx = pool.begin().await?;
    for (id, title) in &stubs {
        // DO NOTHING: never downgrade a real /api/teams row (or a richer stub)
        // that is already present.
        sqlx::query!(
            r#"INSERT INTO wftda_teams (id, title, in_teams_api, fetched_at)
               VALUES ($1, $2, false, now())
               ON CONFLICT (id) DO NOTHING"#,
            id,
            title,
        )
        .execute(&mut *tx)
        .await?;
    }
    for g in &games {
        sqlx::query!(
            r#"INSERT INTO wftda_games
                 (id, date, team_1_id, team_2_id, team_1_score, team_2_score,
                  region, seeding_game, fetched_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now())
               ON CONFLICT (id) DO UPDATE SET
                 date         = EXCLUDED.date,
                 team_1_id    = EXCLUDED.team_1_id,
                 team_2_id    = EXCLUDED.team_2_id,
                 team_1_score = EXCLUDED.team_1_score,
                 team_2_score = EXCLUDED.team_2_score,
                 region       = EXCLUDED.region,
                 seeding_game = EXCLUDED.seeding_game,
                 fetched_at   = now()"#,
            g.id,
            g.date,
            g.team_1_id,
            g.team_2_id,
            g.team_1_score,
            g.team_2_score,
            g.region,
            g.seeding_game,
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    info!(
        "wftda_games: upserted {} game(s) and {} stub team(s)",
        games.len(),
        stubs.len()
    );
    Ok(games.len())
}

struct GameRow {
    id: i64,
    date: NaiveDate,
    team_1_id: i64,
    team_2_id: i64,
    team_1_score: i16,
    team_2_score: i16,
    region: Option<i64>,
    seeding_game: bool,
}

/// Runs both WFTDA stages, tolerating failure of either one: a flaky scrape
/// must never take the process (or the other stage) down. A successful game
/// sync is followed by the resolver (redesign stages 5-6), because new WFTDA
/// games can make previously hidden statsbooks matchable.
pub async fn run_sync(pool: &PgPool, games_lookback: Duration, full_refresh_interval: Duration) {
    let client = WftdaClient::new();
    match sync_teams(pool, &client).await {
        Ok(n) => info!("WFTDA team sync complete: {n} team(s)"),
        Err(e) => warn!("WFTDA team sync failed (continuing): {e:#}"),
    }
    let (full, stop_before) =
        match games_sync_scope(pool, games_lookback, full_refresh_interval).await {
            Ok(scope) => scope,
            Err(e) => {
                warn!("WFTDA game sync scope failed (defaulting to full refresh): {e:#}");
                (true, statsbook_era_start())
            }
        };
    info!(
        "WFTDA game sync: {} (stop before {stop_before})",
        if full { "full refresh" } else { "incremental" }
    );
    match sync_games(pool, &client, stop_before).await {
        Ok(n) => {
            info!("WFTDA game sync complete: {n} game(s)");
            if full {
                if let Err(e) = record_full_games_sync(pool).await {
                    warn!("recording the WFTDA full games sync marker failed: {e:#}");
                }
            }
            super::resolve::run_resolve(pool).await;
        }
        Err(e) => warn!("WFTDA game sync failed (continuing): {e:#}"),
    }
}

/// `sync_state` key holding the last successful full games scan.
const FULL_SYNC_KEY: &str = "wftda_games_full_sync";

/// Decides whether this run needs a full scan and where an incremental scan
/// stops. Full when there is no marker, no games are known yet, or the marker is
/// older than `full_interval`; otherwise the watermark is `max_date - lookback`,
/// never before the statsbook era.
fn games_scan_scope(
    now: DateTime<Utc>,
    last_full: Option<DateTime<Utc>>,
    max_date: Option<NaiveDate>,
    lookback: Duration,
    full_interval: Duration,
) -> (bool, NaiveDate) {
    let era = statsbook_era_start();
    let lookback =
        chrono::Duration::from_std(lookback).unwrap_or_else(|_| chrono::Duration::days(14));
    let full_interval =
        chrono::Duration::from_std(full_interval).unwrap_or_else(|_| chrono::Duration::days(7));
    let full = match (max_date, last_full) {
        (None, _) | (_, None) => true,
        (Some(_), Some(last)) => now.signed_duration_since(last) >= full_interval,
    };
    if full {
        return (true, era);
    }
    let watermark = max_date.expect("incremental scope requires a known max date") - lookback;
    (false, watermark.max(era))
}

/// Reads the persisted scope inputs and applies `games_scan_scope`.
async fn games_sync_scope(
    pool: &PgPool,
    lookback: Duration,
    full_interval: Duration,
) -> Result<(bool, NaiveDate)> {
    let max_date: Option<NaiveDate> = sqlx::query_scalar!("SELECT max(date) FROM wftda_games")
        .fetch_one(pool)
        .await?;
    let last_full: Option<DateTime<Utc>> =
        sqlx::query_scalar!("SELECT value FROM sync_state WHERE key = $1", FULL_SYNC_KEY)
            .fetch_optional(pool)
            .await?;
    Ok(games_scan_scope(
        Utc::now(),
        last_full,
        max_date,
        lookback,
        full_interval,
    ))
}

/// Records a successful full games scan so subsequent runs can be incremental.
async fn record_full_games_sync(pool: &PgPool) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO sync_state (key, value) VALUES ($1, now())
           ON CONFLICT (key) DO UPDATE SET value = now()"#,
        FULL_SYNC_KEY
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Extracts `(slug, title)` pairs from a `/teams?page=N` HTML page. The markup
/// is `<a class="teamsTeam--link" href=".../team/{slug}">` followed (after the
/// image tag) by `<h3 class="teamsTeam--title">{title}</h3>`. Parsed by plain
/// string scanning rather than pulling in an HTML parser. Each link's title is
/// searched only up to the next link, so a block missing either half is
/// skipped rather than borrowing the following team's title.
fn parse_team_listing(html: &str) -> Vec<(String, String)> {
    const LINK_PREFIX: &str = "href=\"https://stats.wftda.com/team/";
    const TITLE_OPEN: &str = "<h3 class=\"teamsTeam--title\">";
    const TITLE_CLOSE: &str = "</h3>";

    let mut out = Vec::new();
    let mut cursor = 0usize;
    while let Some(rel) = html[cursor..].find(LINK_PREFIX) {
        let start = cursor + rel;
        let after = &html[start + LINK_PREFIX.len()..];
        let Some(quote) = after.find('"') else { break };
        let slug = &after[..quote];
        // The anchor body runs from just after the href to the next team link.
        let body_start = start + LINK_PREFIX.len() + quote;
        let body_end = html[body_start..]
            .find(LINK_PREFIX)
            .map(|r| body_start + r)
            .unwrap_or(html.len());
        let body = &html[body_start..body_end];
        if let Some(open) = body.find(TITLE_OPEN)
            && let Some(close) = body[open + TITLE_OPEN.len()..].find(TITLE_CLOSE)
        {
            let title = decode_html_entities(body[open + TITLE_OPEN.len()..][..close].trim());
            if !slug.is_empty() && !title.is_empty() {
                out.push((slug.to_string(), title));
            }
        }
        cursor = body_end;
    }
    out
}

/// Minimal HTML entity decoder for the handful of entities the team listing
/// emits (`&amp;`, `&#039;`, …) plus the common named ones.
fn decode_html_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        // Entity names (or numeric codes) are short; a `;` further away is not
        // an entity terminator.
        if let Some(semi) = tail.find(';')
            && semi <= 10
            && let Some(c) = decode_entity(&tail[1..semi])
        {
            out.push(c);
            rest = &tail[semi + 1..];
            continue;
        }
        out.push('&');
        rest = &tail[1..];
    }
    out.push_str(rest);
    out
}

fn decode_entity(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some('\u{a0}'),
        _ => {
            let code = entity.strip_prefix('#')?;
            let n = if let Some(hex) = code.strip_prefix('x').or_else(|| code.strip_prefix('X')) {
                u32::from_str_radix(hex, 16).ok()?
            } else {
                code.parse::<u32>().ok()?
            };
            char::from_u32(n)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_team_listing_pairs() {
        let html = r#"
            <div class="col col-3 teamsTeam">
              <a class="teamsTeam--link" href="https://stats.wftda.com/team/2x4-roller-derby">
                <img class="teamsTeam--image" src="/images/x.png" />
                <h3 class="teamsTeam--title">2 x 4 Roller Derby</h3>
                <span class="teamsTeam--region">Latin America</span>
              </a>
            </div>
            <div class="col col-3 teamsTeam">
              <a class="teamsTeam--link" href="https://stats.wftda.com/team/black-rose-rollers">
                <img class="teamsTeam--image" src="/images/y.png" />
                <h3 class="teamsTeam--title">Black Rose Rollers: Grime &amp; Punishment</h3>
              </a>
            </div>
        "#;
        let pairs = parse_team_listing(html);
        assert_eq!(
            pairs,
            vec![
                (
                    "2x4-roller-derby".to_string(),
                    "2 x 4 Roller Derby".to_string()
                ),
                (
                    "black-rose-rollers".to_string(),
                    "Black Rose Rollers: Grime & Punishment".to_string(),
                ),
            ]
        );
    }

    #[test]
    fn team_listing_skips_blocks_without_a_title() {
        let html = r#"<a href="https://stats.wftda.com/team/orphan">no title here</a>
            <a class="teamsTeam--link" href="https://stats.wftda.com/team/real">
              <h3 class="teamsTeam--title">Real Team</h3></a>"#;
        let pairs = parse_team_listing(html);
        assert_eq!(pairs, vec![("real".to_string(), "Real Team".to_string())]);
    }

    #[test]
    fn decodes_entities() {
        assert_eq!(decode_html_entities("a &amp; b"), "a & b");
        assert_eq!(decode_html_entities("it&#039;s"), "it's");
        assert_eq!(decode_html_entities("&#x27;x&#x27;"), "'x'");
        assert_eq!(decode_html_entities("100% &"), "100% &");
        assert_eq!(decode_html_entities("no entities"), "no entities");
    }

    #[test]
    fn parses_result_dates() {
        assert_eq!(
            NaiveDate::parse_from_str("20 Sep 2026", DATE_FORMAT).unwrap(),
            NaiveDate::from_ymd_opt(2026, 9, 20).unwrap()
        );
        assert_eq!(
            NaiveDate::parse_from_str("01 Jan 2023", DATE_FORMAT).unwrap(),
            statsbook_era_start()
        );
        assert!(NaiveDate::parse_from_str("2026-09-20", DATE_FORMAT).is_err());
    }

    #[test]
    fn transient_statuses() {
        use reqwest::StatusCode;
        assert!(is_transient(StatusCode::TOO_MANY_REQUESTS));
        assert!(is_transient(StatusCode::INTERNAL_SERVER_ERROR));
        assert!(is_transient(StatusCode::SERVICE_UNAVAILABLE));
        assert!(!is_transient(StatusCode::NOT_FOUND));
        assert!(!is_transient(StatusCode::OK));
    }

    #[test]
    fn games_scan_scope_full_when_marker_missing_or_stale() {
        let now = Utc::now();
        let max = NaiveDate::from_ymd_opt(2026, 10, 20).unwrap();
        let lookback = Duration::from_secs(14 * 24 * 3600);
        let full_interval = Duration::from_secs(7 * 24 * 3600);

        let (full, stop) = games_scan_scope(now, None, Some(max), lookback, full_interval);
        assert!(full);
        assert_eq!(stop, statsbook_era_start());

        let stale = now - chrono::Duration::days(8);
        let (full, _) = games_scan_scope(now, Some(stale), Some(max), lookback, full_interval);
        assert!(full);
    }

    #[test]
    fn games_scan_scope_incremental_uses_lookback_and_floors_at_era() {
        let now = Utc::now();
        let max = NaiveDate::from_ymd_opt(2026, 10, 20).unwrap();
        let recent = now - chrono::Duration::days(1);
        let lookback = Duration::from_secs(14 * 24 * 3600);
        let full_interval = Duration::from_secs(7 * 24 * 3600);

        let (full, stop) = games_scan_scope(now, Some(recent), Some(max), lookback, full_interval);
        assert!(!full);
        assert_eq!(stop, NaiveDate::from_ymd_opt(2026, 10, 6).unwrap());

        // A watermark before the statsbook era is floored at the era.
        let early = NaiveDate::from_ymd_opt(2023, 1, 5).unwrap();
        let (full, stop) =
            games_scan_scope(now, Some(recent), Some(early), lookback, full_interval);
        assert!(!full);
        assert_eq!(stop, statsbook_era_start());
    }

    #[test]
    fn games_scan_scope_full_when_no_games() {
        let now = Utc::now();
        let (full, stop) = games_scan_scope(
            now,
            Some(now),
            None,
            Duration::from_secs(14 * 24 * 3600),
            Duration::from_secs(7 * 24 * 3600),
        );
        assert!(full);
        assert_eq!(stop, statsbook_era_start());
    }
}
