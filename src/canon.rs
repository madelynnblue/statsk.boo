use std::collections::HashSet;

/// Picks the best display name from a group of equivalent names.
/// Prefers shorter names, then fewer uppercase letters, then alphabetical.
pub fn best_name<'a>(names: impl IntoIterator<Item = &'a str>) -> Option<String> {
    names
        .into_iter()
        .min_by(|a, b| {
            (
                a.len(),
                a.chars().filter(|c| c.is_uppercase()).count(),
                a.to_lowercase(),
            )
                .cmp(&(
                    b.len(),
                    b.chars().filter(|c| c.is_uppercase()).count(),
                    b.to_lowercase(),
                ))
        })
        .map(|s| s.to_string())
}

pub fn canonicalize_league(league: &str) -> String {
    league
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

pub fn canonicalize_team(league: Option<&str>, team: &str) -> String {
    let league_words: HashSet<String> = league
        .unwrap_or("")
        .split_whitespace()
        .map(|w| w.to_lowercase())
        .collect();

    let filtered = team
        .split_whitespace()
        .filter(|w| !league_words.contains(&w.to_lowercase()))
        .collect::<Vec<_>>()
        .join(" ");

    let result = filtered
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect::<String>();

    if result.is_empty() {
        team.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(|c| c.to_lowercase())
            .collect()
    } else {
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_canons() {
        let league_cases = [
            ("Boulder County Roller Derby", "bouldercountyrollerderby"),
            ("BOULDER COUNTY ROLLER DERBY", "bouldercountyrollerderby"),
            ("Böblingen Roller Derby", "böblingenrollerderby"),
        ];
        for (input, expected) in league_cases {
            assert_eq!(canonicalize_league(input), expected, "league: {input}");
        }

        let team_cases = [
            (
                Some("Boulder County Roller Derby"),
                "Boulder County Roller Derby Flatiron Phoenixes",
                "flatironphoenixes",
            ),
            (None, "Flatiron Phoenixes", "flatironphoenixes"),
            (Some("Böblingen"), "Böblingen Blitz", "blitz"),
            (
                Some("Boulder County Roller Derby"),
                "Boulder County Roller Derby",
                "bouldercountyrollerderby",
            ),
            (Some("Boulder County Roller Derby"), "Boulter", "boulter"),
        ];
        for (league, team, expected) in team_cases {
            assert_eq!(
                canonicalize_team(league, team),
                expected,
                "team: {team} / league: {league:?}"
            );
        }
    }
}
