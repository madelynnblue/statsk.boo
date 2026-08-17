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

/// Canonical form of a player name for identity matching: lowercase,
/// alphanumeric-only (like league/team canonicals), with captain markers
/// such as "(C)" or "( c )" stripped. Handles case, hyphen/space,
/// apostrophe, and whitespace variants so one player always has one identity.
pub fn canonicalize_name(name: &str) -> String {
    let mut s = name.trim().to_lowercase();
    // Drop captain markers: parenthetical groups whose trimmed content is
    // exactly "c". In real data these appear only as a trailing marker
    // ("Perséfone (C)"), so scanning left-to-right is sufficient.
    loop {
        let Some(open) = s.find('(') else { break };
        let Some(rel_close) = s[open..].find(')') else {
            break;
        };
        let close = open + rel_close;
        if s[open + 1..close].trim() == "c" {
            s.replace_range(open..=close, "");
        } else {
            break;
        }
    }
    s.chars().filter(|c| c.is_alphanumeric()).collect()
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

    #[test]
    fn test_canonicalize_name() {
        let cases = [
            ("Skye the Sk8er", "skyethesk8er"),
            ("Skye The Sk8er", "skyethesk8er"),
            ("SKYE THE SK8ER", "skyethesk8er"),
            ("Cherry Bl'Awesome", "cherryblawesome"),
            ("Cherry Bl’Awesome", "cherryblawesome"),
            ("JESSA-BIT PSYCHO", "jessabitpsycho"),
            ("Jessa Bit Psycho", "jessabitpsycho"),
            ("O'Wheely?", "owheely"),
            ("O'WHEELY?", "owheely"),
            ("Perséfone", "perséfone"),
            ("Perséfone (C)", "perséfone"),
            ("Perséfone ( c )", "perséfone"),
            ("Ziggy Scardust ( C )", "ziggyscardust"),
            ("Circuit Breaker", "circuitbreaker"),
            ("Circuit Breaker ( C )", "circuitbreaker"),
            ("  Foo  Bar  ", "foobar"),
            ("", ""),
            ("(C)", ""),
        ];
        for (input, expected) in cases {
            assert_eq!(canonicalize_name(input), expected, "name: {input:?}");
        }
    }
}
