use crate::canon::best_name;
use crate::web::{AppState, error::AppError};
use axum::extract::State;
use axum::response::Html;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Serialize)]
struct TeamEntry {
    team: String,
    team_canonical: String,
}

#[derive(Serialize)]
struct LeagueEntry {
    league: String,
    league_canonical: String,
    teams: Vec<TeamEntry>,
}

struct RawRow {
    league: Option<String>,
    league_canonical: String,
    team: Option<String>,
    team_canonical: String,
}

fn group_leagues(rows: Vec<RawRow>) -> Vec<LeagueEntry> {
    let mut groups: HashMap<String, (Vec<String>, HashMap<String, Vec<String>>)> = HashMap::new();

    for row in rows {
        let (league_variants, team_groups) =
            groups.entry(row.league_canonical.clone()).or_default();
        if let Some(l) = row.league {
            league_variants.push(l);
        }
        let team_variants = team_groups.entry(row.team_canonical.clone()).or_default();
        if let Some(t) = row.team {
            team_variants.push(t);
        }
    }

    let mut leagues: Vec<LeagueEntry> = groups
        .into_iter()
        .map(|(league_canonical, (league_variants, team_groups))| {
            let league = best_name(league_variants.iter().map(|s| s.as_str()))
                .unwrap_or_else(|| league_canonical.clone());

            let mut teams: Vec<TeamEntry> = team_groups
                .into_iter()
                .map(|(team_canonical, variants)| TeamEntry {
                    team: best_name(variants.iter().map(|s| s.as_str()))
                        .unwrap_or_else(|| team_canonical.clone()),
                    team_canonical,
                })
                .collect();
            teams.sort_by(|a, b| a.team_canonical.cmp(&b.team_canonical));

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
    let rows =
        sqlx::query!("SELECT league, league_canonical, team, team_canonical FROM game_sides")
            .fetch_all(&*state.pool)
            .await?;

    let raw: Vec<RawRow> = rows
        .into_iter()
        .map(|r| RawRow {
            league: r.league,
            league_canonical: r.league_canonical,
            team: r.team,
            team_canonical: r.team_canonical,
        })
        .collect();

    let leagues = group_leagues(raw);

    let tmpl = state.env.get_template("leagues.html")?;
    let html = tmpl.render(minijinja::context! { leagues })?;
    Ok(Html(html))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(league: &str, lc: &str, team: &str, tc: &str) -> RawRow {
        RawRow {
            league: Some(league.into()),
            league_canonical: lc.into(),
            team: Some(team.into()),
            team_canonical: tc.into(),
        }
    }

    #[test]
    fn group_leagues_groups_by_league_canonical() {
        let rows = vec![
            row("Alpha League", "alphaleague", "Team A", "teama"),
            row("Alpha League", "alphaleague", "Team B", "teamb"),
            row("Beta League", "betaleague", "Team C", "teamc"),
            row("Alpha League", "alphaleague", "Team A", "teama"),
        ];
        let leagues = group_leagues(rows);
        assert_eq!(leagues.len(), 2);
        let alpha = leagues
            .iter()
            .find(|l| l.league_canonical == "alphaleague")
            .unwrap();
        assert_eq!(alpha.teams.len(), 2);
        let beta = leagues
            .iter()
            .find(|l| l.league_canonical == "betaleague")
            .unwrap();
        assert_eq!(beta.teams.len(), 1);
    }

    #[test]
    fn group_leagues_sorted_alphabetically() {
        let rows = vec![
            row("Zeta League", "zetaleague", "Team Z", "teamz"),
            row("Alpha League", "alphaleague", "Team A", "teama"),
        ];
        let leagues = group_leagues(rows);
        assert_eq!(leagues[0].league_canonical, "alphaleague");
        assert_eq!(leagues[1].league_canonical, "zetaleague");
    }

    #[test]
    fn group_leagues_teams_sorted_within_league() {
        let rows = vec![
            row("League X", "leaguex", "Zebras", "zebras"),
            row("League X", "leaguex", "Ants", "ants"),
        ];
        let leagues = group_leagues(rows);
        assert_eq!(leagues[0].teams[0].team_canonical, "ants");
        assert_eq!(leagues[0].teams[1].team_canonical, "zebras");
    }

    #[test]
    fn group_leagues_best_name_picked() {
        // "Alpha League" is shorter than "ALPHA LEAGUE" — best_name prefers it
        let rows = vec![
            row("ALPHA LEAGUE", "alphaleague", "Team A", "teama"),
            row("Alpha League", "alphaleague", "Team A", "teama"),
        ];
        let leagues = group_leagues(rows);
        assert_eq!(leagues[0].league, "Alpha League");
    }

    #[test]
    fn group_leagues_empty_input() {
        let leagues = group_leagues(vec![]);
        assert!(leagues.is_empty());
    }

    #[test]
    fn group_leagues_null_league_falls_back_to_canonical() {
        let rows = vec![RawRow {
            league: None,
            league_canonical: "leaguecanon".into(),
            team: Some("Team A".into()),
            team_canonical: "teama".into(),
        }];
        let leagues = group_leagues(rows);
        assert_eq!(leagues[0].league, "leaguecanon");
    }

    #[test]
    fn group_leagues_null_team_falls_back_to_canonical() {
        let rows = vec![RawRow {
            league: Some("League X".into()),
            league_canonical: "leaguex".into(),
            team: None,
            team_canonical: "teamcanon".into(),
        }];
        let leagues = group_leagues(rows);
        assert_eq!(leagues[0].teams[0].team, "teamcanon");
    }
}
