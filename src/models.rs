use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GameData {
    pub version: String,
    pub venue: Venue,
    pub tournament: Option<String>,
    pub host_league: Option<String>,
    pub home: TeamData,
    pub away: TeamData,
    pub periods: Vec<Period>,
    pub penalties: Vec<Penalty>,
    pub game_summary: Option<GameSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GameSummary {
    pub home_players: Vec<SummaryPlayer>,
    pub away_players: Vec<SummaryPlayer>,
    pub home_totals: SummaryTotals,
    pub away_totals: SummaryTotals,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SummaryPlayer {
    pub number: String,
    pub name: String,
    pub jams_jammer: Option<u8>,
    pub jams_pivot: Option<u8>,
    pub jams_blocker: Option<u8>,
    pub jams_total: Option<u8>,
    pub jams_pct: Option<f32>,
    pub jammer_points: Option<i16>,
    pub ppj: Option<f32>,
    pub lost: Option<u8>,
    pub lead: Option<u8>,
    pub called: Option<u8>,
    pub no_initial_trip: Option<u8>,
    pub star_passes: Option<u8>,
    pub lead_pct: Option<f32>,
    pub lead_plus_minus: Option<i16>,
    pub avg_lead_plus_minus: Option<f32>,
    pub pts_for: Option<i16>,
    pub pts_against: Option<i16>,
    pub plus_minus: Option<i16>,
    pub jammer_plus_minus: Option<i16>,
    pub avg_jammer_plus_minus: Option<f32>,
    pub pivot_plus_minus: Option<i16>,
    pub avg_pivot_plus_minus: Option<f32>,
    pub block_plus_minus: Option<i16>,
    pub avg_block_plus_minus: Option<f32>,
    pub pack_plus_minus: Option<i16>,
    pub avg_pack_plus_minus: Option<f32>,
    pub avg_plus_minus: Option<f32>,
    pub vtar_pts_for: Option<f32>,
    pub vtar_pts_against: Option<f32>,
    pub vtar_total_plus_minus: Option<f32>,
    pub vtar_jammer_avg_plus_minus: Option<f32>,
    pub vtar_pivot_avg_plus_minus: Option<f32>,
    pub vtar_blocker_avg_plus_minus: Option<f32>,
    pub vtar_pack_avg_plus_minus: Option<f32>,
    pub total_vtar_avg_plus_minus: Option<f32>,
    pub penalty_count: Option<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SummaryTotals {
    pub jams_jammer: Option<u8>,
    pub jams_pivot: Option<u8>,
    pub jams_blocker: Option<u8>,
    pub jams_total: Option<u16>,
    pub jams_pct: Option<f32>,
    pub jammer_points: Option<i16>,
    pub ppj: Option<f32>,
    pub lost: Option<u8>,
    pub lead: Option<u8>,
    pub called: Option<u8>,
    pub no_initial_trip: Option<u8>,
    pub star_passes: Option<u8>,
    pub lead_pct: Option<f32>,
    pub lead_plus_minus: Option<i16>,
    pub avg_lead_plus_minus: Option<f32>,
    pub pts_for: Option<f32>,
    pub pts_against: Option<f32>,
    pub plus_minus: Option<f32>,
    pub jammer_plus_minus: Option<f32>,
    pub avg_jammer_plus_minus: Option<f32>,
    pub pivot_plus_minus: Option<f32>,
    pub avg_pivot_plus_minus: Option<f32>,
    pub block_plus_minus: Option<f32>,
    pub avg_block_plus_minus: Option<f32>,
    pub pack_plus_minus: Option<f32>,
    pub avg_pack_plus_minus: Option<f32>,
    pub avg_plus_minus: Option<f32>,
    pub vtar_pts_for: Option<f32>,
    pub vtar_pts_against: Option<f32>,
    pub vtar_total_plus_minus: Option<f32>,
    pub vtar_jammer_avg_plus_minus: Option<f32>,
    pub vtar_pivot_avg_plus_minus: Option<f32>,
    pub vtar_blocker_avg_plus_minus: Option<f32>,
    pub vtar_pack_avg_plus_minus: Option<f32>,
    pub total_vtar_avg_plus_minus: Option<f32>,
    pub penalty_count: Option<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Venue {
    pub name: Option<String>,
    pub city: Option<String>,
    pub state: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TeamData {
    pub league: Option<String>,
    pub team: Option<String>,
    pub color: Option<String>,
    pub skaters: Vec<Skater>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Skater {
    pub number: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Period {
    pub number: u8,
    pub jams: Vec<Jam>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Jam {
    pub number: u16,
    pub home: JamSide,
    pub away: JamSide,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JamSide {
    pub jammer: Option<String>,
    pub star_pass_jammer: Option<String>,
    pub lead: bool,
    pub lost: bool,
    pub called: bool,
    pub injury: bool,
    pub no_pivot: bool,
    pub score: i16,
    pub lineup: Vec<LineupEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LineupEntry {
    pub number: String,
    pub position: String, // "jammer", "pivot", "blocker"
    pub box_trips: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Penalty {
    pub number: String,
    pub side: String, // "home" or "away"
    pub period: u8,
    pub jam: Option<u16>,
    pub code: String,
    pub foul_out: bool,
    pub expulsion: bool,
}

impl GameData {
    pub fn total_score(&self, side: &str) -> i16 {
        periods_score(&self.periods, side)
    }
}

pub fn periods_score(periods: &[Period], side: &str) -> i16 {
    periods
        .iter()
        .flat_map(|p| &p.jams)
        .map(|j| {
            if side == "home" {
                j.home.score
            } else {
                j.away.score
            }
        })
        .sum()
}

/// Serialization shim for game_summary stats JSONB.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SideStats {
    pub players: Vec<SummaryPlayer>,
    pub totals: SummaryTotals,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_game() -> GameData {
        GameData {
            version: "2024".into(),
            venue: Venue {
                name: Some("Test Arena".into()),
                city: None,
                state: None,
            },
            tournament: None,
            host_league: None,
            home: TeamData {
                league: Some("Gotham Girls Roller Derby".into()),
                team: Some("Gotham Girls All Stars".into()),
                color: Some("Black".into()),
                skaters: vec![Skater {
                    number: "9".into(),
                    name: "Bonnie Thunders".into(),
                }],
            },
            away: TeamData {
                league: Some("Rose City".into()),
                team: Some("Rose City Rollers".into()),
                color: Some("Red".into()),
                skaters: vec![Skater {
                    number: "247".into(),
                    name: "Atomatrix".into(),
                }],
            },
            periods: vec![Period {
                number: 1,
                jams: vec![Jam {
                    number: 1,
                    home: JamSide {
                        jammer: Some("9".into()),
                        star_pass_jammer: None,
                        lead: true,
                        lost: false,
                        called: false,
                        injury: false,
                        no_pivot: false,
                        score: 4,
                        lineup: vec![],
                    },
                    away: JamSide {
                        jammer: Some("247".into()),
                        star_pass_jammer: None,
                        lead: false,
                        lost: true,
                        called: false,
                        injury: false,
                        no_pivot: false,
                        score: 0,
                        lineup: vec![],
                    },
                }],
            }],
            penalties: vec![],
            game_summary: None,
        }
    }

    #[test]
    fn test_serde_roundtrip() {
        let game = sample_game();
        let json = serde_json::to_string(&game).unwrap();
        let back: GameData = serde_json::from_str(&json).unwrap();
        assert_eq!(game, back);
    }

    #[test]
    fn test_total_score() {
        let game = sample_game();
        assert_eq!(game.total_score("home"), 4);
        assert_eq!(game.total_score("away"), 0);
    }
}
