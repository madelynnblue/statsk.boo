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
        self.periods
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

    pub fn player_search_text(&self) -> String {
        let mut names: Vec<&str> = self
            .home
            .skaters
            .iter()
            .chain(self.away.skaters.iter())
            .map(|s| s.name.as_str())
            .collect();
        names.dedup();
        names.join("\n")
    }

    pub fn team_search_text(&self) -> String {
        [
            self.home.league.as_deref(),
            self.home.team.as_deref(),
            self.away.league.as_deref(),
            self.away.team.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n")
    }
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

    #[test]
    fn test_player_search_text() {
        let game = sample_game();
        let text = game.player_search_text();
        assert!(text.contains("Bonnie Thunders"));
        assert!(text.contains("Atomatrix"));
    }

    #[test]
    fn test_team_search_text() {
        let game = sample_game();
        let text = game.team_search_text();
        assert!(text.contains("Gotham Girls Roller Derby"));
        assert!(text.contains("Gotham Girls All Stars"));
        assert!(text.contains("Rose City"));
    }
}
