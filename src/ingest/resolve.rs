//! Stages 5 and 6 of the WFTDA-first redesign: match parsed statsbooks to
//! WFTDA games, then assign each statsbook side to a WFTDA team.
//!
//! The matcher is deliberately in-process and database-free at its core: the
//! comparison set is every statsbook side against every WFTDA team (and every
//! game in a three-day window), so calling CockroachDB's `pg_trgm`
//! `similarity()` once per pair would mean millions of round trips. Instead the
//! names are canonicalized once, their trigrams are precomputed
//! ([`crate::canon::TrigramSet`]), and the whole resolve pass runs in memory.
//!
//! Paths, in order (see `docs/wftda-first-redesign.md` §3.3):
//!
//! 1. **Signature match** — each side resolved independently by name
//!    (`>= 0.75`); a game between exactly those two teams with the exact
//!    unordered score pair within ±400 days. A team pair plus an exact score is
//!    a near-unique signature, so this recovers statsbooks whose date is wrong
//!    by more than a day.
//! 2. **Date window** over `D-1..D+1` (same day preferred), using the
//!    best-orientation mean name similarity (`team`) and total absolute score
//!    difference (`delta`).
//! 3. Otherwise `method='none'` and the statsbook stays hidden.
//!
//! Every run re-resolves every statsbook and upserts `game_matches`, so a
//! statsbook that had no WFTDA counterpart yet becomes visible the moment the
//! WFTDA sync brings one in.

use crate::canon::{TrigramSet, canonicalize_league};
use anyhow::Result;
use chrono::{Duration, NaiveDate};
use sqlx::PgPool;
use std::cmp::Ordering;
use std::collections::HashMap;
use tracing::{info, warn};

/// A side resolves to a WFTDA team on name alone at this similarity.
pub const SIDE_MATCH_THRESHOLD: f64 = 0.75;
/// Signature-match search radius around the statsbook date, in days.
pub const SIGNATURE_DATE_RADIUS_DAYS: i64 = 400;
/// Same-day drift: at least this name similarity and at most this score delta.
pub const DRIFT_TEAM_MIN: f64 = 0.80;
pub const DRIFT_DELTA_MAX: i32 = 12;
/// Same-day drift, stronger name evidence tolerates a larger score delta.
pub const DRIFT_TEAM_STRONG: f64 = 0.90;
pub const DRIFT_DELTA_STRONG_MAX: i32 = 40;
/// Exact score at offset ±1 needs at least this name similarity.
pub const WINDOW_TEAM_MIN: f64 = 0.60;
/// Zero-total safety net: ignore scores entirely at this name similarity.
pub const ZERO_TOTAL_TEAM_MIN: f64 = 0.85;
/// Required margin over the runner-up at the same date offset.
pub const MARGIN: f64 = 0.05;

/// Statsbooks (and their sides) written per transaction.
const WRITE_BATCH: usize = 500;

/// How a statsbook was matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// Same-day exact-score match.
    Score,
    /// Same-day score drift carried by strong name similarity.
    Drift,
    /// Off-by-one-day exact-score match, or the zero-total safety net.
    Window,
    /// Signature match: independently resolved team pair + exact score.
    Pair,
    /// No confident match; the statsbook stays hidden.
    None,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Score => "score",
            Method::Drift => "drift",
            Method::Window => "window",
            Method::Pair => "pair",
            Method::None => "none",
        }
    }
}

/// Counts by method from one `resolve_games` pass.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ResolveStats {
    pub score: usize,
    pub drift: usize,
    pub window: usize,
    pub pair: usize,
    pub none: usize,
}

impl ResolveStats {
    pub fn matched(&self) -> usize {
        self.score + self.drift + self.window + self.pair
    }

    pub fn total(&self) -> usize {
        self.matched() + self.none
    }

    fn record(&mut self, method: Method) {
        match method {
            Method::Score => self.score += 1,
            Method::Drift => self.drift += 1,
            Method::Window => self.window += 1,
            Method::Pair => self.pair += 1,
            Method::None => self.none += 1,
        }
    }

    /// One-line breakdown for logging.
    pub fn breakdown(&self) -> String {
        format!(
            "matched {} of {} (score {}, drift {}, window {}, pair {}), hidden {}",
            self.matched(),
            self.total(),
            self.score,
            self.drift,
            self.window,
            self.pair,
            self.none
        )
    }
}

/// A WFTDA team as the matcher sees it.
struct Team {
    id: i64,
    trigrams: TrigramSet,
}

/// A WFTDA game with its teams already resolved to `teams` indices.
struct Game {
    id: i64,
    date: NaiveDate,
    team_1: usize,
    team_2: usize,
    score_1: i16,
    score_2: i16,
    /// Unordered score pair, for the signature path.
    pair: (i16, i16),
}

/// One parsed statsbook reduced to the fields the matcher needs.
struct Statsbook {
    drive_file_id: String,
    date: NaiveDate,
    home_score: i16,
    away_score: i16,
    /// Canonical `league + " " + team` for each side, the `side_name` cache key.
    home_name: String,
    away_name: String,
    home: TrigramSet,
    away: TrigramSet,
}

/// One candidate for the date-window path.
struct Candidate {
    /// Absolute day distance from the statsbook date (0 or 1).
    off: i16,
    /// Signed day distance (0, -1, or +1), recorded on the match.
    signed_offset: i16,
    team: f64,
    delta: i32,
    game: usize,
}

/// Result of resolving one statsbook, including the stage 6 side assignment.
#[derive(Debug, Clone, PartialEq)]
struct Resolution {
    method: Method,
    wftda_game_id: Option<i64>,
    team_similarity: f64,
    score_delta: i32,
    date_offset: i16,
    home_team_id: Option<i64>,
    away_team_id: Option<i64>,
}

/// Memoized best-team lookup per canonical side name.
type SideCache = HashMap<String, Option<(i64, f64)>>;

/// The in-memory matcher: teams, games, and the two lookup indices.
struct Matcher {
    teams: Vec<Team>,
    games: Vec<Game>,
    by_date: HashMap<NaiveDate, Vec<usize>>,
    /// Unordered team-id pair -> game indices.
    by_pair: HashMap<(i64, i64), Vec<usize>>,
}

impl Matcher {
    fn new(teams: Vec<Team>, games: Vec<Game>) -> Self {
        let mut by_date: HashMap<NaiveDate, Vec<usize>> = HashMap::new();
        let mut by_pair: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
        for (i, g) in games.iter().enumerate() {
            by_date.entry(g.date).or_default().push(i);
            let (id1, id2) = (teams[g.team_1].id, teams[g.team_2].id);
            by_pair
                .entry((id1.min(id2), id1.max(id2)))
                .or_default()
                .push(i);
        }
        Self {
            teams,
            games,
            by_date,
            by_pair,
        }
    }

    /// Best WFTDA team for a canonical side name, if any reaches
    /// [`SIDE_MATCH_THRESHOLD`]. Ties prefer the lowest team id (the order the
    /// teams were loaded in).
    fn resolve_side(
        &self,
        name: &str,
        trigrams: &TrigramSet,
        cache: &mut SideCache,
    ) -> Option<(i64, f64)> {
        if let Some(&cached) = cache.get(name) {
            return cached;
        }
        let mut best: Option<(i64, f64)> = None;
        for team in &self.teams {
            let sim = trigrams.similarity(&team.trigrams);
            if sim >= SIDE_MATCH_THRESHOLD && best.is_none_or(|(_, bs)| sim > bs) {
                best = Some((team.id, sim));
            }
        }
        cache.insert(name.to_string(), best);
        best
    }

    /// Mean per-side similarity and total absolute score difference for the
    /// better of the two home/away orientations.
    ///
    /// The comparison is lexicographic on `(team, delta)` exactly like the
    /// offline analysis this was validated against (`max((team1, delta1),
    /// (team2, delta2))` in Python). That matters when the two side names are
    /// identical — e.g. a statsbook that records the same team on both sides —
    /// because the two orientation means are then exactly equal and the delta
    /// tie-break, not the names, decides which orientation (if either) is
    /// accepted. Reproducing it keeps such duplicate-name games hidden rather
    /// than falsely linked on score alone.
    fn evaluate(&self, sb: &Statsbook, g: &Game) -> (f64, i32) {
        let (t1, t2) = (&self.teams[g.team_1], &self.teams[g.team_2]);
        let h = i32::from(sb.home_score);
        let a = i32::from(sb.away_score);
        let (s1, s2) = (i32::from(g.score_1), i32::from(g.score_2));

        let o1 = (
            (sb.home.similarity(&t1.trigrams) + sb.away.similarity(&t2.trigrams)) / 2.0,
            (h - s1).abs() + (a - s2).abs(),
        );
        let o2 = (
            (sb.home.similarity(&t2.trigrams) + sb.away.similarity(&t1.trigrams)) / 2.0,
            (h - s2).abs() + (a - s1).abs(),
        );
        if o1 >= o2 { o1 } else { o2 }
    }

    /// Stage 6: which statsbook side is which WFTDA team. Score alignment
    /// decides when the two orientations differ; otherwise the higher mean name
    /// similarity wins; an exact tie falls back to WFTDA's `team_1 == home`.
    fn assign_sides(&self, sb: &Statsbook, g: &Game) -> (i64, i64) {
        let (t1, t2) = (&self.teams[g.team_1], &self.teams[g.team_2]);
        let (h, a) = (i32::from(sb.home_score), i32::from(sb.away_score));
        let (s1, s2) = (i32::from(g.score_1), i32::from(g.score_2));
        let delta_a = (h - s1).abs() + (a - s2).abs();
        let delta_b = (h - s2).abs() + (a - s1).abs();
        if delta_a != delta_b {
            return if delta_a < delta_b {
                (t1.id, t2.id)
            } else {
                (t2.id, t1.id)
            };
        }
        let sim_a = (sb.home.similarity(&t1.trigrams) + sb.away.similarity(&t2.trigrams)) / 2.0;
        let sim_b = (sb.home.similarity(&t2.trigrams) + sb.away.similarity(&t1.trigrams)) / 2.0;
        if sim_b > sim_a {
            (t2.id, t1.id)
        } else {
            (t1.id, t2.id)
        }
    }

    fn matched(
        &self,
        sb: &Statsbook,
        g: &Game,
        method: Method,
        team_similarity: f64,
        score_delta: i32,
        date_offset: i16,
    ) -> Resolution {
        let (home_team_id, away_team_id) = self.assign_sides(sb, g);
        Resolution {
            method,
            wftda_game_id: Some(g.id),
            team_similarity,
            score_delta,
            date_offset,
            home_team_id: Some(home_team_id),
            away_team_id: Some(away_team_id),
        }
    }

    fn no_match(&self) -> Resolution {
        Resolution {
            method: Method::None,
            wftda_game_id: None,
            team_similarity: 0.0,
            score_delta: 0,
            date_offset: 0,
            home_team_id: None,
            away_team_id: None,
        }
    }

    /// Path 1: independently resolved team pair plus an exact score.
    fn signature_match(&self, sb: &Statsbook, cache: &mut SideCache) -> Option<Resolution> {
        let (home_id, _) = self.resolve_side(&sb.home_name, &sb.home, cache)?;
        let (away_id, _) = self.resolve_side(&sb.away_name, &sb.away, cache)?;
        let pair = (
            sb.home_score.min(sb.away_score),
            sb.home_score.max(sb.away_score),
        );
        let indices = self
            .by_pair
            .get(&(home_id.min(away_id), home_id.max(away_id)))?;

        // Nearest game within the radius; a tie for nearest is not a signature.
        let mut best: Option<(i64, usize)> = None;
        let mut tied = false;
        for &gi in indices {
            let g = &self.games[gi];
            if g.pair != pair {
                continue;
            }
            let days = (g.date - sb.date).num_days().abs();
            if days > SIGNATURE_DATE_RADIUS_DAYS {
                continue;
            }
            match best {
                None => {
                    best = Some((days, gi));
                    tied = false;
                }
                Some((best_days, _)) if days < best_days => {
                    best = Some((days, gi));
                    tied = false;
                }
                Some((best_days, _)) if days == best_days => tied = true,
                _ => {}
            }
        }
        let (_, gi) = best?;
        if tied {
            return None;
        }
        let g = &self.games[gi];
        let (team, delta) = self.evaluate(sb, g);
        let offset = (g.date - sb.date).num_days();
        Some(self.matched(sb, g, Method::Pair, team, delta, offset as i16))
    }

    /// Path 2: date window `D-1..D+1`, same day preferred.
    fn window_match(&self, sb: &Statsbook) -> Resolution {
        // A statsbook whose own total is still zero is treated as score-blind
        // (a real 0–0 game with recorded jams; placeholders never get here).
        let zero_total = i32::from(sb.home_score) + i32::from(sb.away_score) == 0;
        let mut candidates: Vec<Candidate> = Vec::new();

        for offset in [-1i64, 0, 1] {
            let date = sb.date + Duration::days(offset);
            let Some(indices) = self.by_date.get(&date) else {
                continue;
            };
            for &gi in indices {
                let g = &self.games[gi];
                let (team, delta) = self.evaluate(sb, g);
                let off = offset.unsigned_abs() as i16;
                if window_candidate_ok(team, delta, off, zero_total) {
                    candidates.push(Candidate {
                        off,
                        signed_offset: offset as i16,
                        team,
                        delta,
                        game: gi,
                    });
                }
            }
        }
        if candidates.is_empty() {
            return self.no_match();
        }

        // Same day first, then the strongest name match, then the smallest
        // score delta; the game id keeps the order deterministic.
        candidates.sort_by(|a, b| {
            a.off
                .cmp(&b.off)
                .then_with(|| b.team.partial_cmp(&a.team).unwrap_or(Ordering::Equal))
                .then_with(|| a.delta.cmp(&b.delta))
                .then_with(|| self.games[a.game].id.cmp(&self.games[b.game].id))
        });
        let top = &candidates[0];

        // A drift match must beat the runner-up at the same offset clearly; an
        // exact-score candidate does not need the margin.
        if candidates.len() > 1 {
            let runner_up = &candidates[1];
            if runner_up.off == top.off && top.delta != 0 && (top.team - runner_up.team) < MARGIN {
                return self.no_match();
            }
        }

        let method = if zero_total {
            Method::Window
        } else if top.off == 0 && top.delta == 0 {
            Method::Score
        } else if top.off == 0 {
            Method::Drift
        } else {
            Method::Window
        };
        let g = &self.games[top.game];
        self.matched(sb, g, method, top.team, top.delta, top.signed_offset)
    }

    fn resolve(&self, sb: &Statsbook, cache: &mut SideCache) -> Resolution {
        if let Some(resolution) = self.signature_match(sb, cache) {
            return resolution;
        }
        self.window_match(sb)
    }
}

/// The date-window acceptance rule for one candidate. `off` is the absolute
/// day distance (0 or 1), `zero_total` means the statsbook's own score total is
/// zero so scores are ignored.
fn window_candidate_ok(team: f64, delta: i32, off: i16, zero_total: bool) -> bool {
    let ok = if zero_total {
        team >= ZERO_TOTAL_TEAM_MIN
    } else if off > 0 {
        delta == 0
    } else {
        delta == 0
            || (team >= DRIFT_TEAM_MIN && delta <= DRIFT_DELTA_MAX)
            || (team >= DRIFT_TEAM_STRONG && delta <= DRIFT_DELTA_STRONG_MAX)
    };
    ok && (off == 0 || team >= WINDOW_TEAM_MIN)
}

/// Canonical `league + " " + team` name for a statsbook side, falling back to
/// the stored canonicals when the raw values are missing.
fn side_name(
    league: Option<&str>,
    team: Option<&str>,
    league_canonical: &str,
    team_canonical: &str,
) -> String {
    let canon = canonicalize_league(&format!("{} {}", league.unwrap_or(""), team.unwrap_or("")));
    if canon.is_empty() {
        format!("{league_canonical}{team_canonical}")
    } else {
        canon
    }
}

/// Stage 5 driver: resolve every parsed statsbook against `wftda_games`,
/// upsert `game_matches`, and write the stage 6 side assignment.
pub async fn resolve_games(pool: &PgPool) -> Result<ResolveStats> {
    let team_rows = sqlx::query!("SELECT id, title FROM wftda_teams ORDER BY id")
        .fetch_all(pool)
        .await?;
    let game_rows = sqlx::query!(
        r#"SELECT g.id,
                  g.date,
                  g.team_1_id,
                  g.team_2_id,
                  g.team_1_score,
                  g.team_2_score
             FROM wftda_games g
             ORDER BY g.id"#,
    )
    .fetch_all(pool)
    .await?;
    let book_rows = sqlx::query!(
        r#"SELECT g.drive_file_id,
                  g.date,
                  g.home_score,
                  g.away_score,
                  h.league            AS "home_league?",
                  h.team              AS "home_team?",
                  h.league_canonical  AS "home_league_canonical!",
                  h.team_canonical    AS "home_team_canonical!",
                  a.league            AS "away_league?",
                  a.team              AS "away_team?",
                  a.league_canonical  AS "away_league_canonical!",
                  a.team_canonical    AS "away_team_canonical!"
             FROM statsbook_games g
             JOIN statsbook_sides h
               ON h.drive_file_id = g.drive_file_id AND h.side = 'home'
             JOIN statsbook_sides a
               ON a.drive_file_id = g.drive_file_id AND a.side = 'away'
             ORDER BY g.drive_file_id"#,
    )
    .fetch_all(pool)
    .await?;

    let teams: Vec<Team> = team_rows
        .iter()
        .map(|row| Team {
            id: row.id,
            // The statsbook side is `canon(league + " " + team)`, so the WFTDA
            // title must be canonicalized the same way before comparing; the
            // canonical form is one alphanumeric blob, not word-split text.
            trigrams: TrigramSet::new(&canonicalize_league(&row.title)),
        })
        .collect();
    let mut team_index: HashMap<i64, usize> = HashMap::with_capacity(teams.len());
    for (i, team) in teams.iter().enumerate() {
        team_index.insert(team.id, i);
    }

    let mut games = Vec::with_capacity(game_rows.len());
    for row in &game_rows {
        let (Some(&team_1), Some(&team_2)) = (
            team_index.get(&row.team_1_id),
            team_index.get(&row.team_2_id),
        ) else {
            warn!(
                "wftda game {} references a team outside wftda_teams; skipping",
                row.id
            );
            continue;
        };
        games.push(Game {
            id: row.id,
            date: row.date,
            team_1,
            team_2,
            score_1: row.team_1_score,
            score_2: row.team_2_score,
            pair: (
                row.team_1_score.min(row.team_2_score),
                row.team_1_score.max(row.team_2_score),
            ),
        });
    }

    let matcher = Matcher::new(teams, games);
    let mut cache: SideCache = HashMap::new();
    let mut stats = ResolveStats::default();

    for chunk in book_rows.chunks(WRITE_BATCH) {
        let mut tx = pool.begin().await?;
        for row in chunk {
            let home_name = side_name(
                row.home_league.as_deref(),
                row.home_team.as_deref(),
                &row.home_league_canonical,
                &row.home_team_canonical,
            );
            let away_name = side_name(
                row.away_league.as_deref(),
                row.away_team.as_deref(),
                &row.away_league_canonical,
                &row.away_team_canonical,
            );
            let statsbook = Statsbook {
                drive_file_id: row.drive_file_id.clone(),
                date: row.date,
                home_score: row.home_score,
                away_score: row.away_score,
                home: TrigramSet::new(&home_name),
                away: TrigramSet::new(&away_name),
                home_name,
                away_name,
            };
            let resolution = matcher.resolve(&statsbook, &mut cache);
            stats.record(resolution.method);

            sqlx::query!(
                r#"INSERT INTO game_matches
                     (drive_file_id, wftda_game_id, method, team_similarity,
                      score_delta, date_offset, resolved_at)
                   VALUES ($1, $2, $3, $4, $5, $6, now())
                   ON CONFLICT (drive_file_id) DO UPDATE SET
                     wftda_game_id   = EXCLUDED.wftda_game_id,
                     method          = EXCLUDED.method,
                     team_similarity = EXCLUDED.team_similarity,
                     score_delta     = EXCLUDED.score_delta,
                     date_offset     = EXCLUDED.date_offset,
                     resolved_at     = now()"#,
                statsbook.drive_file_id,
                resolution.wftda_game_id,
                resolution.method.as_str(),
                resolution.team_similarity as f32,
                i16::try_from(resolution.score_delta).unwrap_or(i16::MAX),
                resolution.date_offset,
            )
            .execute(&mut *tx)
            .await?;

            // One statement writes both sides; NULL for an unmatched statsbook
            // clears any assignment left by a previous run.
            sqlx::query!(
                r#"UPDATE statsbook_sides
                   SET wftda_team_id = CASE side
                       WHEN 'home' THEN $2
                       WHEN 'away' THEN $3
                   END
                   WHERE drive_file_id = $1"#,
                statsbook.drive_file_id,
                resolution.home_team_id,
                resolution.away_team_id,
            )
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
    }

    let duplicates = dedup_matched_games(pool).await?;
    info!("resolve stage complete: {}", stats.breakdown());
    if duplicates > 0 {
        info!("resolve: hid {duplicates} duplicate statsbook(s) behind the newest file");
    }
    Ok(stats)
}

/// Enforce one visible statsbook per WFTDA game. Identity is `wftda_game_id`, so
/// when duplicate uploads (or a corrected re-upload with different scores) both
/// match the same game, the newest `statsbooks.modified_time` wins. The rest are
/// hidden (`wftda_game_id = NULL`, `method = 'duplicate'`) with their side
/// assignments cleared. Idempotent: it also restores a previously hidden file
/// when it becomes the newest.
async fn dedup_matched_games(pool: &PgPool) -> Result<usize> {
    let losers = sqlx::query!(
        r#"SELECT drive_file_id
           FROM (
               SELECT m.drive_file_id,
                      row_number() OVER (
                          PARTITION BY m.wftda_game_id
                          ORDER BY s.modified_time DESC NULLS LAST, m.drive_file_id
                      ) AS rn
               FROM game_matches m
               JOIN statsbooks s ON s.drive_file_id = m.drive_file_id
               WHERE m.wftda_game_id IS NOT NULL
           ) ranked
           WHERE rn > 1"#
    )
    .fetch_all(pool)
    .await?;

    if losers.is_empty() {
        return Ok(0);
    }

    let mut tx = pool.begin().await?;
    for row in &losers {
        sqlx::query!(
            r#"UPDATE game_matches
               SET wftda_game_id = NULL, method = 'duplicate', resolved_at = now()
               WHERE drive_file_id = $1"#,
            row.drive_file_id,
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query!(
            "UPDATE statsbook_sides SET wftda_team_id = NULL WHERE drive_file_id = $1",
            row.drive_file_id,
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(losers.len())
}

/// Non-fatal wrapper for the ingest loops: `resolve_games` already logs its
/// method breakdown on success, so only failures are logged here. A resolver
/// bug never takes down a sync cycle.
pub async fn run_resolve(pool: &PgPool) {
    if let Err(e) = resolve_games(pool).await {
        warn!("resolve stage failed (continuing): {e:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    /// Build a matcher from `(id, title)` teams and games described as
    /// `(id, date, home_team_id, away_team_id, home_score, away_score)`.
    fn matcher(teams: &[(i64, &str)], games: &[(i64, NaiveDate, i64, i64, i16, i16)]) -> Matcher {
        let teams: Vec<Team> = teams
            .iter()
            .map(|(id, title)| Team {
                id: *id,
                trigrams: TrigramSet::new(&canonicalize_league(title)),
            })
            .collect();
        let index: HashMap<i64, usize> = teams.iter().enumerate().map(|(i, t)| (t.id, i)).collect();
        let games: Vec<Game> = games
            .iter()
            .map(|(id, d, t1, t2, s1, s2)| Game {
                id: *id,
                date: *d,
                team_1: index[t1],
                team_2: index[t2],
                score_1: *s1,
                score_2: *s2,
                pair: ((*s1).min(*s2), (*s1).max(*s2)),
            })
            .collect();
        Matcher::new(teams, games)
    }

    /// A statsbook side name is `canon(league + " " + team)`.
    fn statsbook(
        date: NaiveDate,
        home: (&str, &str),
        away: (&str, &str),
        home_score: i16,
        away_score: i16,
    ) -> Statsbook {
        let home_name = canonicalize_league(&format!("{} {}", home.0, home.1));
        let away_name = canonicalize_league(&format!("{} {}", away.0, away.1));
        Statsbook {
            drive_file_id: "book".into(),
            date,
            home_score,
            away_score,
            home: TrigramSet::new(&home_name),
            away: TrigramSet::new(&away_name),
            home_name,
            away_name,
        }
    }

    fn resolve(m: &Matcher, sb: &Statsbook) -> Resolution {
        m.resolve(sb, &mut SideCache::new())
    }

    #[test]
    fn window_rule_table() {
        // Zero total ignores scores but still needs a strong name.
        assert!(window_candidate_ok(0.85, 500, 0, true));
        assert!(!window_candidate_ok(0.84, 0, 0, true));
        // Same-day exact score has no name floor and no delta cap.
        assert!(window_candidate_ok(0.0, 0, 0, false));
        // Same-day drift: 0.80..0.90 needs delta <= 12.
        assert!(window_candidate_ok(0.80, 12, 0, false));
        assert!(!window_candidate_ok(0.80, 13, 0, false));
        assert!(!window_candidate_ok(0.79, 12, 0, false));
        // Same-day strong drift: 0.90+ tolerates delta <= 40.
        assert!(window_candidate_ok(0.90, 40, 0, false));
        assert!(!window_candidate_ok(0.90, 41, 0, false));
        assert!(!window_candidate_ok(0.89, 40, 0, false));
        // Offset ±1: exact score only, with a 0.60 name floor.
        assert!(window_candidate_ok(0.60, 0, 1, false));
        assert!(!window_candidate_ok(0.59, 0, 1, false));
        assert!(!window_candidate_ok(1.0, 1, 1, false));
        // Zero total is permissive on scores but still honours the name floor.
        assert!(window_candidate_ok(0.85, 99, 1, true));
        assert!(!window_candidate_ok(0.70, 0, 1, true));
    }

    #[test]
    fn signature_pair_uses_exact_names_and_exact_score() {
        let m = matcher(
            &[(1, "Alpha Roller Derby"), (2, "Beta Roller Derby")],
            &[(10, date(2026, 6, 1), 1, 2, 100, 90)],
        );
        let r = resolve(
            &m,
            &statsbook(
                date(2026, 6, 1),
                ("Alpha", "Roller Derby"),
                ("Beta", "Roller Derby"),
                100,
                90,
            ),
        );
        assert_eq!(r.method, Method::Pair);
        assert_eq!(r.wftda_game_id, Some(10));
        assert_eq!(r.date_offset, 0);
        assert_eq!(r.score_delta, 0);
        assert_eq!((r.home_team_id, r.away_team_id), (Some(1), Some(2)));
    }

    #[test]
    fn same_day_score_matches_when_one_side_is_unresolvable() {
        // The home name pins team_1, but the away name is too weak to resolve
        // on its own (path 1 needs both sides). The same-day exact score still
        // matches, and the name evidence picks orientation A.
        let m = matcher(
            &[(1, "Alpha Roller Derby"), (2, "Beta Roller Derby")],
            &[(11, date(2026, 6, 1), 1, 2, 100, 90)],
        );
        let r = resolve(
            &m,
            &statsbook(
                date(2026, 6, 1),
                ("Alpha", "Roller Derby"),
                ("Zenith League", "Comets"),
                100,
                90,
            ),
        );
        assert_eq!(r.method, Method::Score);
        assert_eq!(r.wftda_game_id, Some(11));
        assert_eq!((r.home_team_id, r.away_team_id), (Some(1), Some(2)));
    }

    #[test]
    fn score_alignment_decides_a_swapped_game() {
        // Local home scored 100; the WFTDA row records that team as team_2.
        // Names resolve to the swapped ids, so path 1 finds the pair and the
        // score alignment alone decides the sides.
        let m = matcher(
            &[(1, "Alpha Roller Derby"), (2, "Beta Roller Derby")],
            &[(12, date(2026, 6, 1), 1, 2, 90, 100)],
        );
        let r = resolve(
            &m,
            &statsbook(
                date(2026, 6, 1),
                ("Beta", "Roller Derby"),
                ("Alpha", "Roller Derby"),
                100,
                90,
            ),
        );
        assert_eq!(r.method, Method::Pair);
        assert_eq!((r.home_team_id, r.away_team_id), (Some(2), Some(1)));
    }

    #[test]
    fn signature_match_recovers_a_wrong_date() {
        // Same teams and exact score a year off: only path 1 can find it.
        let m = matcher(
            &[(1, "Alpha Roller Derby"), (2, "Beta Roller Derby")],
            &[(20, date(2024, 5, 25), 1, 2, 287, 87)],
        );
        let r = resolve(
            &m,
            &statsbook(
                date(2025, 5, 25),
                ("Alpha", "Roller Derby"),
                ("Beta", "Roller Derby"),
                287,
                87,
            ),
        );
        assert_eq!(r.method, Method::Pair);
        assert_eq!(r.wftda_game_id, Some(20));
        assert_eq!(r.date_offset, -365);
    }

    #[test]
    fn tied_nearest_signature_falls_through_to_the_window() {
        let m = matcher(
            &[(1, "Alpha Roller Derby"), (2, "Beta Roller Derby")],
            &[
                (20, date(2024, 5, 24), 1, 2, 100, 90),
                (21, date(2024, 5, 26), 1, 2, 100, 90),
            ],
        );
        let r = resolve(
            &m,
            &statsbook(
                date(2024, 5, 25),
                ("Alpha", "Roller Derby"),
                ("Beta", "Roller Derby"),
                100,
                90,
            ),
        );
        // Neither nearest game wins the signature tie; the ±1 window does.
        assert_eq!(r.method, Method::Window);
        assert_eq!(r.wftda_game_id, Some(20));
        assert_eq!(r.date_offset, -1);
    }

    #[test]
    fn same_day_score_drift_is_drift() {
        let m = matcher(
            &[(1, "Alpha Roller Derby"), (2, "Beta Roller Derby")],
            &[(30, date(2026, 6, 1), 1, 2, 104, 90)],
        );
        let r = resolve(
            &m,
            &statsbook(
                date(2026, 6, 1),
                ("Alpha", "Roller Derby"),
                ("Beta", "Roller Derby"),
                100,
                90,
            ),
        );
        assert_eq!(r.method, Method::Drift);
        assert_eq!(r.wftda_game_id, Some(30));
        assert_eq!(r.score_delta, 4);
        assert_eq!(r.date_offset, 0);
    }

    #[test]
    fn large_score_gap_is_hidden() {
        let m = matcher(
            &[(1, "Alpha Roller Derby"), (2, "Beta Roller Derby")],
            &[(31, date(2026, 6, 1), 1, 2, 145, 90)],
        );
        let r = resolve(
            &m,
            &statsbook(
                date(2026, 6, 1),
                ("Alpha", "Roller Derby"),
                ("Beta", "Roller Derby"),
                100,
                90,
            ),
        );
        assert_eq!(r.method, Method::None);
        assert_eq!(r.wftda_game_id, None);
        assert_eq!((r.home_team_id, r.away_team_id), (None, None));
    }

    #[test]
    fn strong_names_tolerate_a_larger_drift() {
        let m = matcher(
            &[(1, "Alpha Roller Derby"), (2, "Beta Roller Derby")],
            &[(32, date(2026, 6, 1), 1, 2, 136, 90)],
        );
        let sb = statsbook(
            date(2026, 6, 1),
            ("Alpha", "Roller Derby"),
            ("Beta", "Roller Derby"),
            100,
            90,
        );
        let (team, delta) = m.evaluate(&sb, &m.games[0]);
        assert_eq!(team, 1.0);
        assert_eq!(delta, 36);
        let r = resolve(&m, &sb);
        assert_eq!(r.method, Method::Drift);
        assert_eq!(r.score_delta, 36);
    }

    #[test]
    fn off_by_one_exact_score_is_a_window_match() {
        // The away side resolves to team 2, but the only nearby game is 1v3;
        // path 1 finds no pair, so the ±1 window carries the match.
        let m = matcher(
            &[
                (1, "Alpha Roller Derby"),
                (2, "Beta Roller Derby"),
                (3, "Beta Roller Derby B"),
            ],
            &[(40, date(2026, 8, 8), 1, 3, 100, 90)],
        );
        let r = resolve(
            &m,
            &statsbook(
                date(2026, 8, 9),
                ("Alpha", "Roller Derby"),
                ("Beta", "Roller Derby"),
                100,
                90,
            ),
        );
        assert_eq!(r.method, Method::Window);
        assert_eq!(r.wftda_game_id, Some(40));
        assert_eq!(r.date_offset, -1);
        assert_eq!(r.score_delta, 0);
    }

    #[test]
    fn zero_total_ignores_scores_with_strong_names() {
        let m = matcher(
            &[(1, "Alpha Roller Derby"), (2, "Beta Roller Derby")],
            &[(60, date(2026, 2, 21), 1, 2, 88, 76)],
        );
        let r = resolve(
            &m,
            &statsbook(
                date(2026, 2, 21),
                ("Alpha", "Roller Derby"),
                ("Beta", "Roller Derby"),
                0,
                0,
            ),
        );
        assert_eq!(r.method, Method::Window);
        assert_eq!(r.wftda_game_id, Some(60));
    }

    #[test]
    fn no_plausible_game_is_none() {
        let m = matcher(
            &[(1, "Alpha Roller Derby"), (2, "Beta Roller Derby")],
            &[(70, date(2026, 6, 1), 1, 2, 100, 90)],
        );
        let r = resolve(
            &m,
            &statsbook(
                date(2026, 6, 1),
                ("Totally Different League", "Nobody"),
                ("Another League", "Someone"),
                50,
                40,
            ),
        );
        assert_eq!(r.method, Method::None);
        assert_eq!(r.wftda_game_id, None);
    }

    #[test]
    fn evaluate_prefers_the_better_orientation() {
        let m = matcher(
            &[(1, "Alpha Roller Derby"), (2, "Beta Roller Derby")],
            &[(90, date(2026, 6, 1), 1, 2, 104, 90)],
        );
        // Local home is Beta (which WFTDA lists as team_2).
        let sb = statsbook(
            date(2026, 6, 1),
            ("Beta", "Roller Derby"),
            ("Alpha", "Roller Derby"),
            100,
            90,
        );
        let (team, delta) = m.evaluate(&sb, &m.games[0]);
        // Orientation B (home = team_2) has both names at 1.0, so it wins even
        // though its score delta (24) is larger than orientation A's (4).
        assert_eq!(team, 1.0);
        assert_eq!(delta, 24);
    }

    #[test]
    fn equal_score_deltas_use_name_similarity() {
        // 95-95 makes both orientations differ by 10, so names decide.
        let m = matcher(
            &[(1, "Alpha Roller Derby"), (2, "Beta Roller Derby")],
            &[(80, date(2026, 6, 1), 1, 2, 95, 95)],
        );
        let straight = statsbook(
            date(2026, 6, 1),
            ("Alpha", "Roller Derby"),
            ("Beta", "Roller Derby"),
            100,
            90,
        );
        let r = resolve(&m, &straight);
        assert_eq!(r.method, Method::Drift);
        assert_eq!((r.home_team_id, r.away_team_id), (Some(1), Some(2)));

        let swapped = statsbook(
            date(2026, 6, 1),
            ("Beta", "Roller Derby"),
            ("Alpha", "Roller Derby"),
            100,
            90,
        );
        let r = resolve(&m, &swapped);
        assert_eq!((r.home_team_id, r.away_team_id), (Some(2), Some(1)));
    }

    #[test]
    fn margin_check_rejects_a_close_runner_up() {
        let m = matcher(
            &[(1, "Alpha Roller Derby"), (2, "Beta Roller Derby")],
            &[
                (50, date(2026, 6, 1), 1, 2, 104, 90),
                (51, date(2026, 6, 1), 1, 2, 106, 90),
            ],
        );
        let r = resolve(
            &m,
            &statsbook(
                date(2026, 6, 1),
                ("Alpha", "Roller Derby"),
                ("Beta", "Roller Derby"),
                100,
                90,
            ),
        );
        // Both games have identical names and a 4/6 delta: neither is confident.
        assert_eq!(r.method, Method::None);
    }

    #[test]
    fn stats_breakdown_counts_every_method() {
        let mut stats = ResolveStats::default();
        for m in [
            Method::Score,
            Method::Drift,
            Method::Window,
            Method::Pair,
            Method::None,
        ] {
            stats.record(m);
        }
        assert_eq!(Method::Score.as_str(), "score");
        assert_eq!(Method::Drift.as_str(), "drift");
        assert_eq!(Method::Window.as_str(), "window");
        assert_eq!(Method::Pair.as_str(), "pair");
        assert_eq!(Method::None.as_str(), "none");
        assert_eq!(stats.matched(), 4);
        assert_eq!(stats.total(), 5);
        assert_eq!(stats.none, 1);
        assert_eq!(
            stats.breakdown(),
            "matched 4 of 5 (score 1, drift 1, window 1, pair 1), hidden 1"
        );
    }

    #[tokio::test]
    #[ignore = "requires a live migrated statskboo_v2 database; run with --ignored"]
    async fn dedup_keeps_newest_statsbook_per_game() {
        use crate::ingest::parse::PARSER_VERSION;

        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgresql://root@cockroachdb:26257/statskboo_v2?sslmode=disable".into()
        });
        let pool = PgPool::connect(&url).await.unwrap();

        let cleanup = |pool: PgPool| async move {
            sqlx::query(
                "DELETE FROM statsbooks WHERE drive_file_id LIKE '__resolve_dedup_test__%'",
            )
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query("DELETE FROM wftda_games WHERE id = -900001")
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query("DELETE FROM wftda_teams WHERE id IN (-900001, -900002)")
                .execute(&pool)
                .await
                .unwrap();
        };
        cleanup(pool.clone()).await;

        sqlx::query(
            r#"INSERT INTO wftda_teams (id, title, in_teams_api, fetched_at)
               VALUES (-900001, 'Dedup Home', false, now()),
                      (-900002, 'Dedup Away', false, now())"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO wftda_games (id, date, team_1_id, team_2_id, team_1_score, team_2_score)
               VALUES (-900001, DATE '2026-06-01', -900001, -900002, 100, 90)"#,
        )
        .execute(&pool)
        .await
        .unwrap();

        for (id, modified) in [
            ("__resolve_dedup_test__old", "2024-01-01 00:00:00+00"),
            ("__resolve_dedup_test__new", "2025-01-01 00:00:00+00"),
        ] {
            sqlx::query(
                r#"INSERT INTO statsbooks
                     (drive_file_id, name, quarter, modified_time, parse_status, parser_version)
                   VALUES ($1, $1, 'test', $2::timestamptz, 'parsed', $3)"#,
            )
            .bind(id)
            .bind(modified)
            .bind(PARSER_VERSION)
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO game_matches (drive_file_id, wftda_game_id, method) VALUES ($1, -900001, 'pair')",
            )
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query(
                r#"INSERT INTO statsbook_sides
                     (drive_file_id, side, wftda_team_id, league_canonical, team_canonical)
                   VALUES ($1, 'home', -900001, 'dedup home', 'dedup home')"#,
            )
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        }

        assert_eq!(dedup_matched_games(&pool).await.unwrap(), 1);

        let old: (Option<i64>, String) = sqlx::query_as(
            "SELECT wftda_game_id, method FROM game_matches WHERE drive_file_id = '__resolve_dedup_test__old'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let new: (Option<i64>, String) = sqlx::query_as(
            "SELECT wftda_game_id, method FROM game_matches WHERE drive_file_id = '__resolve_dedup_test__new'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(old, (None, "duplicate".to_string()), "older file is hidden");
        assert_eq!(new, (Some(-900001), "pair".to_string()), "newer file wins");

        let old_side: Option<i64> = sqlx::query_scalar(
            "SELECT wftda_team_id FROM statsbook_sides WHERE drive_file_id = '__resolve_dedup_test__old'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(old_side, None, "hidden duplicate's sides are unassigned");

        // Re-running changes nothing: the loser is already out of the partition.
        assert_eq!(dedup_matched_games(&pool).await.unwrap(), 0);

        cleanup(pool).await;
    }
}
