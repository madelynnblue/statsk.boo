#[test]
fn test_parse_testsheet() {
    let bytes = std::fs::read("tests/fixtures/TestSheet.xlsx")
        .expect("download tests/fixtures/TestSheet.xlsx first");
    let game = wsb::ingest::parse::parse_statsbook(&bytes).expect("parse failed");

    // Basic sanity: both sides should have skaters
    assert!(!game.home.skaters.is_empty(), "home roster is empty");
    assert!(!game.away.skaters.is_empty(), "away roster is empty");

    // Both periods should have jams
    assert_eq!(game.periods.len(), 2);
    assert!(!game.periods[0].jams.is_empty(), "period 1 has no jams");

    // Scores should be non-negative
    let home_total = game.total_score("home");
    let away_total = game.total_score("away");
    assert!(home_total >= 0, "negative home score: {home_total}");
    assert!(away_total >= 0, "negative away score: {away_total}");
}
