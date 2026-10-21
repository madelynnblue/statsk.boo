use statskboo::models::GameData;

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
    // Regression test: this statsbook's Game Summary is cached and correct
    // (49/212 home, 43/236 away), but the LU JAMMER SUMPRODUCT formula counts
    // SP* (opposing-team star pass) rows, so formualizer re-evaluation
    // over-counts jammer jams (52/215, 52/245). The parser must prefer the
    // statsbook's own cached values.
    Fixture {
        path: "rocstars-connecticut.xlsx",
        period_count: 2,
        jam_counts: &[20, 20],
        star_pass_counts: &[6, 6],
        penalties: 35,
        summary_players: (15, 15),
        scores: (82, 210),
    },
    // Regression test for formula-evaluated Game Summary (this statsbook's
    // Game Summary formulas were not cached — calamine saw zeros everywhere).
    // Formualizer evaluates the SUMPRODUCT/LU/SK formulas and recovers all
    // 15 players per side with correct jam counts.
    Fixture {
        path: "standbys-flatiron.xlsx",
        period_count: 2,
        jam_counts: &[26, 23],
        star_pass_counts: &[4, 3],
        penalties: 70,
        summary_players: (15, 15),
        scores: (202, 88),
    },
    // Regression test: the IGRF roster in this statsbook lists skater #390
    // "Falke" twice (rows 21 and 30), which used to produce two
    // `statsbook_skaters` rows with the same (side, number) primary key and
    // fail the batch INSERT with a unique violation, dropping the whole game
    // from ingest.
    Fixture {
        path: "copenhagen-dup-roster.xlsx",
        period_count: 2,
        jam_counts: &[22, 22],
        star_pass_counts: &[1, 5],
        penalties: 39,
        summary_players: (13, 13),
        scores: (25, 331),
    },
    // Regression test: this statsbook's IGRF date cell is a text string
    // ("2026-06-27\t\t\t\t") instead of a real Excel date, which used to leave
    // the game undated. The parser must parse text dates.
    Fixture {
        path: "connecticut-yankee-brutals.xlsx",
        period_count: 2,
        jam_counts: &[21, 21],
        star_pass_counts: &[5, 3],
        penalties: 38,
        summary_players: (15, 15),
        scores: (158, 114),
    },
    // Regression test: the away TEAM cell is blank in this statsbook's IGRF
    // (the single-team convention), which used to make the away side
    // anonymous. The team must fall back to the league.
    Fixture {
        path: "brussels-ladrache.xlsx",
        period_count: 2,
        jam_counts: &[17, 12],
        star_pass_counts: &[6, 6],
        penalties: 66,
        summary_players: (15, 14),
        scores: (194, 57),
    },
    // Regression test: the home LEAGUE cell is blank in this statsbook's IGRF.
    // The league must fall back to the team.
    Fixture {
        path: "tulsa-elite.xlsx",
        period_count: 2,
        jam_counts: &[23, 24],
        star_pass_counts: &[4, 6],
        penalties: 35,
        summary_players: (13, 15),
        scores: (50, 233),
    },
    // Regression test: this statsbook's IGRF date cell is completely blank
    // ("ENTER DATE ON IGRF TAB!" in the Game Summary header). The date must be
    // recovered from the `[WFTDA]STATS-YYYY-MM-DD_...` file name.
    Fixture {
        path: "auld-reekie-b.xlsx",
        period_count: 2,
        jam_counts: &[21, 20],
        star_pass_counts: &[6, 3],
        penalties: 46,
        summary_players: (4, 4),
        scores: (93, 156),
    },
    // Regression test: this statsbook's cached IGRF home TOTAL POINTS cell
    // (C38 = SUM(C36:C37)) is stale at 0 even though the Score sheet records
    // real jam scores summing to 146. The parser must use the jam sum for that
    // side while keeping the away side's cached 67.
    Fixture {
        path: "dallas-hidden-tracks-igrf-zero.xlsx",
        period_count: 2,
        jam_counts: &[19, 23],
        star_pass_counts: &[7, 7],
        penalties: 38,
        summary_players: (15, 13),
        scores: (146, 67),
    },
    // Live game 69052 (Arch Rival vs Denver, 2026-10-18): the Lineups sheet
    // has SP rows interspersed with the jam rows, and only 15/14 of the
    // 20-player IGRF charter roster actually played. Regression coverage for
    // blockers being swallowed and for the played-roster filter.
    Fixture {
        path: "arch-rival-mile-high.xlsx",
        period_count: 2,
        jam_counts: &[20, 25],
        star_pass_counts: &[4, 1],
        penalties: 49,
        summary_players: (15, 14),
        scores: (182, 117),
    },
];

fn parse_fixture(path: &str) -> GameData {
    let full = format!("tests/fixtures/{}", path);
    let bytes = std::fs::read(&full).unwrap_or_else(|e| panic!("read {}: {}", full, e));
    statskboo::ingest::parse::parse_statsbook(&bytes).expect("parse failed")
}

#[test]
fn test_fixture_corpus() {
    for f in FIXTURES {
        let game = parse_fixture(f.path);

        // Every real fixture must have game data; the empty-statsbook guard
        // that skips unfilled placeholders must not misfire on a played game.
        assert!(
            !statskboo::ingest::parse::has_no_game_data(&game),
            "{}: real fixture detected as empty",
            f.path
        );

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
        assert_eq!(game.home_score, f.scores.0, "{}: home score", f.path);
        assert_eq!(game.away_score, f.scores.1, "{}: away score", f.path);

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

#[test]
fn test_roster_numbers_unique_per_side() {
    // A skater number uniquely identifies a player within a side; duplicate
    // roster rows (seen in some statsbooks) must be deduplicated by the parser,
    // otherwise the `statsbook_skaters` batch INSERT violates its
    // (drive_file_id, side, number) primary key and the whole game fails to
    // ingest.
    use std::collections::HashSet;
    for f in FIXTURES {
        let game = parse_fixture(f.path);
        for (label, side) in [("home", &game.home), ("away", &game.away)] {
            let mut seen = HashSet::new();
            for skater in &side.skaters {
                assert!(
                    seen.insert(&skater.number),
                    "{}: duplicate {label} roster number {}",
                    f.path,
                    skater.number
                );
            }
        }
    }
}

#[test]
fn test_played_roster_and_jam_blockers() {
    // Only players who actually appeared in a jam are kept (WFTDA fields up to
    // 15; the IGRF charter roster can list 20). Lineups must carry the three
    // blockers as well as jammer/pivot, or the game page's Blockers column is
    // empty and the played-roster filter drops everyone who only blocked.
    let game = parse_fixture("standbys-flatiron.xlsx");
    assert_eq!(
        game.home.skaters.len() + game.away.skaters.len(),
        30,
        "played roster (bench players excluded)"
    );
    let jam = &game.periods[0].jams[0];
    let home_blockers: Vec<&str> = jam
        .home
        .lineup
        .iter()
        .filter(|e| e.position == "blocker")
        .map(|e| e.number.as_str())
        .collect();
    let away_blockers: Vec<&str> = jam
        .away
        .lineup
        .iter()
        .filter(|e| e.position == "blocker")
        .map(|e| e.number.as_str())
        .collect();
    assert_eq!(home_blockers, ["1917", "412", "02"]);
    assert_eq!(away_blockers, ["818", "33", "69"]);
    assert!(
        game.home
            .skaters
            .iter()
            .chain(game.away.skaters.iter())
            .all(|s| !s.number.ends_with('*')),
        "the zero-jam marker must not leak into the player number"
    );
}

#[test]
fn test_arch_rival_lineups_and_played_roster() {
    // Live 69052: a 20-player charter roster where only 15/14 actually played,
    // and the Lineups sheet has SP rows between the jam rows. Every jam's
    // Blockers column must come from the Lineups sheet, not be empty.
    let game = parse_fixture("arch-rival-mile-high.xlsx");
    assert_eq!(game.home.skaters.len(), 15, "home played roster");
    assert_eq!(game.away.skaters.len(), 14, "away played roster");

    let jam = &game.periods[0].jams[0];
    let home_blockers: Vec<&str> = jam
        .home
        .lineup
        .iter()
        .filter(|e| e.position == "blocker")
        .map(|e| e.number.as_str())
        .collect();
    let away_blockers: Vec<&str> = jam
        .away
        .lineup
        .iter()
        .filter(|e| e.position == "blocker")
        .map(|e| e.number.as_str())
        .collect();
    assert_eq!(home_blockers, ["1900", "1853", "810"]);
    assert_eq!(away_blockers, ["505", "8000", "22"]);

    let total_blockers: usize = game
        .periods
        .iter()
        .flat_map(|p| p.jams.iter())
        .map(|j| {
            j.home
                .lineup
                .iter()
                .filter(|e| e.position == "blocker")
                .count()
                + j.away
                    .lineup
                    .iter()
                    .filter(|e| e.position == "blocker")
                    .count()
        })
        .sum();
    assert!(total_blockers > 0, "blockers must not all be empty");
}

#[test]
fn test_fixture_summary_totals() {
    // SP* rows (star passes by the opposing team) must not be counted as jammer
    // jams. Formualizer's re-evaluation of the LU JAMMER formula counts them,
    // inflating the totals; the cached values in the statsbook are correct.
    let game = parse_fixture("rocstars-connecticut.xlsx");
    let gs = game.game_summary.as_ref().unwrap();
    assert_eq!(gs.home_totals.jams_jammer, Some(49));
    assert_eq!(gs.home_totals.jams_total, Some(212));
    assert_eq!(gs.away_totals.jams_jammer, Some(43));
    assert_eq!(gs.away_totals.jams_total, Some(236));
    // Per-player: Demonica (237) jammed 10 jams, not 12; FeFe (2150) 10, not 11.
    let demonica = gs.home_players.iter().find(|p| p.number == "237").unwrap();
    assert_eq!(demonica.jams_jammer, Some(10));
    assert_eq!(demonica.jams_total, Some(10));
    let fefe = gs.home_players.iter().find(|p| p.number == "2150").unwrap();
    assert_eq!(fefe.jams_jammer, Some(10));
    assert_eq!(fefe.jams_total, Some(10));
}

/// Some statsbooks cache a stale Game Summary jam-count denominator
/// (LU!D3/LU!D102 = 0), so the "% Jams Skated" column evaluates to "" for every
/// skater even though the jams are recorded on the Score sheet. The parser must
/// derive the percentage from the parsed jam count instead of leaving it blank
/// (game 69053: Rose City vs Victorian, 2026-10-18).
#[test]
fn test_stale_jams_pct_denominator_is_derived() {
    let game = parse_fixture("rose-city-victorian-2026-10-18.xlsx");
    let gs = game.game_summary.as_ref().unwrap();
    // The Score sheet records 27 jams in period 1 and 24 in period 2.
    let total_jams = 51.0f32;

    let zip = gs.home_players.iter().find(|p| p.number == "0").unwrap();
    assert_eq!(zip.jams_total, Some(4));
    assert!((zip.jams_pct.unwrap() - 4.0 / total_jams).abs() < 0.001);

    let pavey = gs.away_players.iter().find(|p| p.number == "4000").unwrap();
    assert_eq!(pavey.jams_total, Some(15));
    assert!((pavey.jams_pct.unwrap() - 15.0 / total_jams).abs() < 0.001);

    // The totals row mirrors the statsbook's AVERAGE formula over the skaters.
    assert!((gs.home_totals.jams_pct.unwrap() - 1.0 / 3.0).abs() < 0.001);
    assert!((gs.away_totals.jams_pct.unwrap() - 5.0 / 14.0).abs() < 0.001);

    // Cached lead percentages must be left untouched by the fill.
    let darth = gs.home_players.iter().find(|p| p.number == "85").unwrap();
    assert_eq!(darth.lead_pct, Some(0.5));
}

/// Blank team/league IGRF header cells must fall back to the other value so
/// the side keeps its identity (required by the match path).
#[test]
fn test_blank_team_league_fallback() {
    use statskboo::ingest::parse::parse_statsbook;

    // Away TEAM blank -> team becomes the league.
    let game = parse_statsbook(&std::fs::read("tests/fixtures/brussels-ladrache.xlsx").unwrap())
        .expect("parse failed");
    assert_eq!(
        game.away.team.as_deref(),
        Some("Sheffield Steel Roller Derby")
    );
    assert_eq!(
        game.away.league.as_deref(),
        Some("Sheffield Steel Roller Derby")
    );

    // Home LEAGUE blank -> league becomes the team.
    let game = parse_statsbook(&std::fs::read("tests/fixtures/tulsa-elite.xlsx").unwrap())
        .expect("parse failed");
    assert_eq!(game.home.league.as_deref(), Some("TULSA ELITE"));
    assert_eq!(game.home.team.as_deref(), Some("TULSA ELITE"));
}

/// A date typed as text in the IGRF date cell must be parsed (Connecticut
/// stores "2026-06-27\t\t\t\t" as a string).
#[test]
fn test_text_igrf_date() {
    use statskboo::ingest::parse::parse_statsbook_with_date;

    let bytes = std::fs::read("tests/fixtures/connecticut-yankee-brutals.xlsx").unwrap();
    let (_, date) = parse_statsbook_with_date(&bytes, None).expect("parse failed");
    assert_eq!(
        date,
        Some(chrono::NaiveDate::from_ymd_opt(2026, 6, 27).unwrap())
    );
}

/// A blank IGRF date cell must fall back to the date in the file name
/// (Auld Reekie's Game Summary header literally says "ENTER DATE ON IGRF TAB!").
#[test]
fn test_file_name_date_fallback() {
    use statskboo::ingest::parse::parse_statsbook_with_date;

    let bytes = std::fs::read("tests/fixtures/auld-reekie-b.xlsx").unwrap();

    // Without a file name there is nowhere to get the date.
    let (_, date) = parse_statsbook_with_date(&bytes, None).expect("parse failed");
    assert_eq!(date, None);

    // With the statsbook's file name, the date comes from the name.
    let (_, date) = parse_statsbook_with_date(
        &bytes,
        Some(
            "[WFTDA]STATS-2024-09-28_AuldReekieRollerDerby_AuldReekieRollerDerbyB_vs_LondonRollerDerby_BatterCPower",
        ),
    )
    .expect("parse failed");
    assert_eq!(
        date,
        Some(chrono::NaiveDate::from_ymd_opt(2024, 9, 28).unwrap())
    );
}

/// A real Excel date in the IGRF cell is authoritative: it wins even when the
/// file name carries a (wrong) different date, and no fallbacks run.
#[test]
fn test_cached_excel_date_used_first() {
    use statskboo::ingest::parse::parse_statsbook_with_date;

    let bytes = std::fs::read("tests/fixtures/brussels-ladrache.xlsx").unwrap();
    let (_, date) = parse_statsbook_with_date(&bytes, Some("[WFTDA]STATS-1999-01-01_wrong_name"))
        .expect("parse failed");
    assert_eq!(
        date,
        Some(chrono::NaiveDate::from_ymd_opt(2024, 10, 26).unwrap())
    );
}

/// The end-to-end point of the header fallbacks: these statsbooks used to be
/// skipped by the ingester because a blank or text-typed IGRF header left the
/// game without a usable identity. Now they must parse with a date and a
/// complete home/away identity (the fields the match path needs).
#[test]
fn test_recovered_games_parse_with_identity() {
    use statskboo::ingest::parse::parse_statsbook_with_date;

    // (fixture, real Drive file name — Auld Reekie needs it for the date).
    let fixtures = [
        (
            "tests/fixtures/connecticut-yankee-brutals.xlsx",
            "[WFTDA]STATS-2026-06-27_ConnecticutRollerDerby_YankeeBrutals_vs_FreeStateRollerDerby_RockVillians",
        ),
        (
            "tests/fixtures/brussels-ladrache.xlsx",
            "[WFTDA]STATS-2024-10-26_BrusselsRollerDerby_LaDrache_vs_SheffieldSteelRollerDerby",
        ),
        (
            "tests/fixtures/tulsa-elite.xlsx",
            "[WFTDA]STATS-2023-05-06_TulsaElite_vs_CapitalCityCrushers",
        ),
        (
            "tests/fixtures/auld-reekie-b.xlsx",
            "[WFTDA]STATS-2024-09-28_AuldReekieRollerDerby_AuldReekieRollerDerbyB_vs_LondonRollerDerby_BatterCPower",
        ),
    ];
    for (path, name) in fixtures {
        let bytes = std::fs::read(path).unwrap();
        let (game, date) = parse_statsbook_with_date(&bytes, Some(name)).expect("parse failed");
        assert!(date.is_some(), "{path}: no date recovered");
        for (side_name, side) in [("home", &game.home), ("away", &game.away)] {
            assert!(side.league.is_some(), "{path}: missing {side_name} league");
            assert!(side.team.is_some(), "{path}: missing {side_name} team");
        }
    }
}

/// A statsbook whose rosters and metadata are filled in but which recorded no
/// jams, no penalties, and a 0–0 score is an unfilled placeholder, not a game
/// (issue #3). The ingester skips these via `has_no_game_data`.
#[test]
fn test_empty_statsbook_has_no_game_data() {
    use statskboo::ingest::parse::{has_no_game_data, parse_statsbook};

    let bytes = std::fs::read("tests/fixtures/hulagans-vendolls-empty.xlsx").unwrap();
    let game = parse_statsbook(&bytes).expect("parse failed");

    assert!(has_no_game_data(&game), "empty statsbook not detected");
    assert_eq!(game.home_score, 0);
    assert_eq!(game.away_score, 0);
    assert!(game.periods.iter().all(|p| p.jams.is_empty()));
    assert!(game.penalties.is_empty());
    // The Game Summary must not be fabricated from blank Lineups formulas:
    // formualizer used to report 228 pivot / 255 blocker / 912 total jams here.
    assert!(
        game.game_summary.is_none(),
        "empty statsbook produced a game summary"
    );
}

/// A stale cached IGRF TOTAL POINTS cell must not override real jam scores.
/// Here the home side caches 0 — its `SUM(C36:C37)` period subtotals were never
/// refreshed — while the Score sheet's jams sum to 146. The away side's cached
/// 67 is valid and stays authoritative.
#[test]
fn test_stale_zero_igrf_total_uses_jam_sum() {
    use statskboo::ingest::parse::{has_no_game_data, parse_statsbook};

    let bytes = std::fs::read("tests/fixtures/dallas-hidden-tracks-igrf-zero.xlsx").unwrap();
    let game = parse_statsbook(&bytes).expect("parse failed");

    assert_eq!(game.home_score, 146, "home score");
    assert_eq!(game.away_score, 67, "away score");
    assert!(
        !has_no_game_data(&game),
        "played game detected as empty placeholder"
    );
}
