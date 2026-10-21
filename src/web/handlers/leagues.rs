use crate::canon::{best_name, canonicalize_league};
use crate::web::{AppState, error::AppError};
use axum::extract::State;
use axum::response::Html;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Serialize, Clone)]
pub struct TeamEntry {
    pub id: i64,
    pub title: String,
    /// WFTDA slug, or the numeric id for the few stub teams that have none.
    pub slug: String,
}

#[derive(Serialize)]
pub struct LeagueEntry {
    pub league: String,
    pub league_canonical: String,
    pub teams: Vec<TeamEntry>,
}

/// Approximate league for a WFTDA team title: the text before the first `:`
/// (WFTDA writes "League: Team"), else the whole title. WFTDA has no league
/// entity of its own, so this grouping is deliberately heuristic.
pub fn derive_league(title: &str) -> &str {
    match title.split_once(':') {
        Some((league, _)) => league.trim(),
        None => title.trim(),
    }
}

pub fn group_leagues(rows: Vec<(i64, String, String)>) -> Vec<LeagueEntry> {
    let mut groups: HashMap<String, (Vec<String>, Vec<TeamEntry>)> = HashMap::new();

    for (id, title, slug) in rows {
        let league = derive_league(&title).to_string();
        let entry = groups.entry(canonicalize_league(&league)).or_default();
        entry.0.push(league);
        entry.1.push(TeamEntry { id, title, slug });
    }

    let mut leagues: Vec<LeagueEntry> = groups
        .into_iter()
        .map(|(league_canonical, (variants, mut teams))| {
            let league = best_name(variants.iter().map(|s| s.as_str()))
                .unwrap_or_else(|| league_canonical.clone());
            teams.sort_by(|a, b| a.title.cmp(&b.title));
            LeagueEntry {
                league,
                league_canonical,
                teams,
            }
        })
        .collect();

    leagues.sort_by(|a, b| a.league_canonical.cmp(&b.league_canonical));
    leagues
}

pub async fn handle(State(state): State<AppState>) -> Result<Html<String>, AppError> {
    let rows = sqlx::query!(
        r#"SELECT id, title, COALESCE(slug, id::STRING) AS "slug!" FROM wftda_teams ORDER BY title"#
    )
    .fetch_all(&*state.pool)
    .await?;

    let leagues = group_leagues(rows.into_iter().map(|r| (r.id, r.title, r.slug)).collect());

    let tmpl = state.env.get_template("leagues.html")?;
    let html = tmpl.render(minijinja::context! { leagues })?;
    Ok(Html(html))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: i64, title: &str) -> (i64, String, String) {
        (id, title.into(), format!("slug-{id}"))
    }

    #[test]
    fn derive_league_splits_on_colon() {
        assert_eq!(
            derive_league("Boulder County Roller Derby: Flat Iron Phoenixes"),
            "Boulder County Roller Derby"
        );
        assert_eq!(derive_league("Bay Area Derby"), "Bay Area Derby");
        assert_eq!(derive_league("  Trimmed  "), "Trimmed");
    }

    #[test]
    fn group_leagues_groups_by_derived_league() {
        let rows = vec![
            row(1, "Alpha League: Team A"),
            row(2, "Alpha League: Team B"),
            row(3, "Beta League"),
            row(4, "Alpha League: Team A"),
        ];
        let leagues = group_leagues(rows);
        assert_eq!(leagues.len(), 2);
        let alpha = leagues
            .iter()
            .find(|l| l.league_canonical == canonicalize_league("Alpha League"))
            .unwrap();
        assert_eq!(alpha.league, "Alpha League");
        assert_eq!(alpha.teams.len(), 3);
        let beta = leagues
            .iter()
            .find(|l| l.league_canonical == canonicalize_league("Beta League"))
            .unwrap();
        assert_eq!(beta.teams.len(), 1);
    }

    #[test]
    fn group_leagues_sorted_alphabetically() {
        let rows = vec![row(1, "Zeta League"), row(2, "Alpha League")];
        let leagues = group_leagues(rows);
        assert_eq!(leagues[0].league_canonical, "alphaleague");
        assert_eq!(leagues[1].league_canonical, "zetaleague");
    }

    #[test]
    fn group_leagues_teams_sorted_within_league() {
        let rows = vec![row(1, "League X: Zebras"), row(2, "League X: Ants")];
        let leagues = group_leagues(rows);
        assert_eq!(leagues[0].teams[0].title, "League X: Ants");
        assert_eq!(leagues[0].teams[1].title, "League X: Zebras");
    }

    #[test]
    fn group_leagues_empty_input() {
        assert!(group_leagues(vec![]).is_empty());
    }

    #[test]
    fn group_leagues_distinct_titles_stay_distinct() {
        // Teams without a colon use the whole title, so they do not collapse
        // into a synthetic league.
        let rows = vec![row(1, "Bay Area Derby"), row(2, "Sacred City Derby")];
        let leagues = group_leagues(rows);
        assert_eq!(leagues.len(), 2);
    }
}
