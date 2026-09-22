use std::cmp::Ordering;
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
    while let Some(open) = s.find('(') {
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

/// Sorted, deduplicated `pg_trgm`-style trigrams for one string.
///
/// This is the in-process equivalent of `pg_trgm`'s `show_trgm()`: the string
/// is lowercased, split into `[a-z0-9]+` words, each word is padded to
/// `"  " + word + " "`, and every 3-character window of the padded word becomes
/// a trigram. Building the set once lets the game matcher compare thousands of
/// names without a database round trip per pair (the naive comparison set is
/// ~4k statsbook sides against ~4k WFTDA games).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrigramSet(Vec<[u8; 3]>);

impl TrigramSet {
    pub fn new(s: &str) -> Self {
        Self(trigrams(s))
    }

    /// `|intersection| / |union|`, matching `pg_trgm`'s `similarity()`.
    pub fn similarity(&self, other: &TrigramSet) -> f64 {
        trigram_similarity(&self.0, &other.0)
    }
}

/// The trigrams of `s` as a sorted, deduplicated vector (see [`TrigramSet`]).
pub fn trigrams(s: &str) -> Vec<[u8; 3]> {
    let lowered = s.to_lowercase();
    let mut grams: Vec<[u8; 3]> = Vec::new();
    for word in lowered.split(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit())) {
        if word.is_empty() {
            continue;
        }
        let mut padded = String::with_capacity(word.len() + 3);
        padded.push_str("  ");
        padded.push_str(word);
        padded.push(' ');
        let bytes = padded.as_bytes();
        // A non-empty word pads to at least 4 bytes, so the window is in range.
        for i in 0..bytes.len() - 2 {
            grams.push([bytes[i], bytes[i + 1], bytes[i + 2]]);
        }
    }
    grams.sort_unstable();
    grams.dedup();
    grams
}

/// Jaccard similarity of two *sorted, deduplicated* trigram lists.
pub fn trigram_similarity(a: &[[u8; 3]], b: &[[u8; 3]]) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let (mut i, mut j, mut intersection) = (0usize, 0usize, 0usize);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            Ordering::Equal => {
                intersection += 1;
                i += 1;
                j += 1;
            }
            Ordering::Less => i += 1,
            Ordering::Greater => j += 1,
        }
    }
    let union = a.len() + b.len() - intersection;
    intersection as f64 / union as f64
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

    #[test]
    fn trigrams_match_pg_trgm_padding() {
        // Verified against CockroachDB's `show_trgm` (pg_trgm keeps every
        // 3-character window of the padded word, including ones that span the
        // padding). `show_trgm('word')` = {"  w"," wo",ord,"rd ",wor}.
        assert_eq!(
            trigrams("word"),
            vec![*b"  w", *b" wo", *b"ord", *b"rd ", *b"wor"]
        );
        // `show_trgm('hello')` = {"  h"," he",ell,hel,llo,"lo "}.
        assert_eq!(
            trigrams("hello"),
            vec![*b"  h", *b" he", *b"ell", *b"hel", *b"llo", *b"lo "]
        );
    }

    #[test]
    fn trigrams_lowercase_and_split_words() {
        // Every word is padded independently and punctuation separates words.
        // `show_trgm('a b')` = {"  a","  b"," a "," b "}.
        assert_eq!(trigrams("a b"), vec![*b"  a", *b"  b", *b" a ", *b" b "]);
        assert_eq!(trigrams("Hi!"), trigrams("hi"));
        assert_eq!(trigrams(""), Vec::<[u8; 3]>::new());
        assert_eq!(trigrams("!!!"), Vec::<[u8; 3]>::new());
        // Digits are word characters. `show_trgm('2x4')` = {"  2"," 2x",2x4,"x4 "}.
        assert_eq!(trigrams("2x4"), vec![*b"  2", *b" 2x", *b"2x4", *b"x4 "]);
    }

    #[test]
    fn trigram_similarity_is_jaccard() {
        let hello = TrigramSet::new("hello");
        let hallo = TrigramSet::new("hallo");
        // common {"  h","llo","lo "} = 3, union 9 -> 3/9, matching
        // `similarity('hello','hallo')` = 0.3333333333333333.
        assert!((hello.similarity(&hallo) - 1.0 / 3.0).abs() < 1e-9);
        assert!((hello.similarity(&hello) - 1.0).abs() < 1e-9);
        assert_eq!(hello.similarity(&TrigramSet::new("zzzzz")), 0.0);
        assert_eq!(hello.similarity(&TrigramSet::new("")), 0.0);
    }

    #[test]
    fn trigram_similarity_handles_unicode_canon() {
        // `canonicalize_league` keeps Unicode letters but drops separators, so a
        // canon string is one word; the Unicode letter itself is not a trigram
        // character (matching `pg_trgm`'s `[a-z0-9]` word class).
        let a = TrigramSet::new(&canonicalize_league("Böblingen Blitz"));
        let b = TrigramSet::new("boblingenblitz");
        assert!(a.similarity(&b) > 0.4);
    }
}
