use wsb::models::GameData;

/// A statsbook fixture with expected parse results.
///
/// To add a new fixture: drop the `.xlsx` file in `tests/fixtures/`,
/// then add a `Fixture` entry with the expected values.
struct Fixture {
    path: &'static str,
    period_count: usize,
    /// Expected jam counts per period (in order)
    jam_counts: &'static [usize],
    /// Expected star-pass jam counts per period (in order)
    star_pass_counts: &'static [usize],
    /// Expected penalty count (exact)
    penalties: usize,
    /// Expected home/away summary player counts
    summary_players: (usize, usize),
    /// Expected (home, away) final scores
    scores: (i16, i16),
}

const FIXTURES: &[Fixture] = &[
    Fixture {
        path: "boise-boulder.xlsx",
        period_count: 2,
        jam_counts: &[22, 25],
        star_pass_counts: &[8, 6],
        penalties: 31,
        summary_players: (13, 13),
        // Score sheet sums to 175 due to a known data-entry error in an SP trip cell;
        // IGRF TOTAL POINTS (174) is authoritative.
        scores: (174, 164),
    },
    Fixture {
        path: "TestSheet.xlsx",
        period_count: 2,
        jam_counts: &[23, 23],
        star_pass_counts: &[3, 1],
        penalties: 61,
        summary_players: (15, 15),
        scores: (117, 199),
    },
    Fixture {
        path: "CascadianClash2026_Palouse_vs_RatCity.xlsx",
        period_count: 2,
        jam_counts: &[28, 25],
        star_pass_counts: &[5, 7],
        penalties: 26,
        summary_players: (15, 15),
        scores: (133, 149),
    },
    Fixture {
        path: "BoulderCounty2026_vs_RockyMountain.xlsx",
        period_count: 2,
        jam_counts: &[24, 20],
        star_pass_counts: &[4, 3],
        penalties: 52,
        summary_players: (15, 14),
        scores: (137, 270),
    },
    Fixture {
        path: "boulder-dames.xlsx",
        period_count: 2,
        jam_counts: &[23, 25],
        star_pass_counts: &[2, 3],
        penalties: 25,
        summary_players: (15, 10),
        scores: (198, 97),
    },
];

fn parse_fixture(path: &str) -> GameData {
    let full = format!("tests/fixtures/{}", path);
    let bytes = std::fs::read(&full).unwrap_or_else(|e| panic!("read {}: {}", full, e));
    wsb::ingest::parse::parse_statsbook(&bytes).expect("parse failed")
}

#[test]
fn test_fixture_corpus() {
    for f in FIXTURES {
        let game = parse_fixture(f.path);

        // Rosters
        assert!(
            !game.home.skaters.is_empty(),
            "{}: home roster empty",
            f.path
        );
        assert!(
            !game.away.skaters.is_empty(),
            "{}: away roster empty",
            f.path
        );

        // Scores
        assert_eq!(
            game.total_score("home"),
            f.scores.0,
            "{}: home score",
            f.path
        );
        assert_eq!(
            game.total_score("away"),
            f.scores.1,
            "{}: away score",
            f.path
        );

        // Periods and jams (these catch regressions in star pass / SP row parsing)
        assert_eq!(
            game.periods.len(),
            f.period_count,
            "{}: period count",
            f.path
        );
        for (pi, period) in game.periods.iter().enumerate() {
            assert_eq!(
                period.number,
                (pi + 1) as u8,
                "{}: period {} number",
                f.path,
                pi
            );
            assert_eq!(
                period.jams.len(),
                f.jam_counts[pi],
                "{}: period {} jam count",
                f.path,
                pi
            );
            let sp_count = period
                .jams
                .iter()
                .filter(|j| j.home.star_pass_jammer.is_some() || j.away.star_pass_jammer.is_some())
                .count();
            assert_eq!(
                sp_count, f.star_pass_counts[pi],
                "{}: period {} star pass count",
                f.path, pi
            );
        }

        // Penalties (exact count to catch formula-resolution regressions)
        assert_eq!(
            game.penalties.len(),
            f.penalties,
            "{}: penalty count",
            f.path
        );

        // Game summary (catches IGRF formula-resolution regressions)
        let gs = game
            .game_summary
            .as_ref()
            .unwrap_or_else(|| panic!("{}: game summary missing", f.path));
        assert_eq!(
            gs.home_players.len(),
            f.summary_players.0,
            "{}: home summary players",
            f.path
        );
        assert_eq!(
            gs.away_players.len(),
            f.summary_players.1,
            "{}: away summary players",
            f.path
        );
    }
}
