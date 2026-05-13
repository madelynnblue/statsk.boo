use crate::models::*;
use anyhow::{Context, Result};
use calamine::{Data, DataType, Range, Reader, Xlsx, open_workbook_from_rs};
use std::collections::HashSet;
use std::io::Cursor;

/// Bump this whenever the parsing logic changes, so the ingester can re-parse
/// games that were ingested with an older version of the parser.
pub const PARSER_VERSION: i64 = 11;

pub fn parse_statsbook(bytes: &[u8]) -> Result<GameData> {
    let (game, _) = parse_statsbook_with_date(bytes)?;
    Ok(game)
}

pub fn parse_statsbook_with_date(bytes: &[u8]) -> Result<(GameData, Option<chrono::NaiveDate>)> {
    let cursor = Cursor::new(bytes);
    let mut wb: Xlsx<_> = open_workbook_from_rs(cursor).context("failed to open xlsx workbook")?;

    let version = parse_version(&mut wb);
    let venue = parse_venue(&mut wb)?;
    let (tournament, host_league, date) = parse_igrf_meta(&mut wb)?;
    let mut home = parse_team(&mut wb, 1, 2, 13, 20)?;
    let mut away = parse_team(&mut wb, 8, 9, 13, 20)?;
    let mut periods = parse_scores(&mut wb)?;
    merge_lineups(&mut wb, &mut periods)?;
    let home_players = players_in_jams(&periods, "home");
    let away_players = players_in_jams(&periods, "away");
    home.skaters.retain(|s| home_players.contains(&s.number));
    away.skaters.retain(|s| away_players.contains(&s.number));
    let igrf = read_igrf_cells(&mut wb)?;
    let penalties = parse_penalties(&mut wb, &igrf)?;
    let game_summary = parse_game_summary(&mut wb, &igrf).ok();

    let home_score = periods_score(&periods, "home");
    let away_score = periods_score(&periods, "away");
    let game = GameData {
        version,
        venue,
        tournament,
        host_league,
        home,
        away,
        periods,
        penalties,
        game_summary,
        home_score,
        away_score,
    };
    Ok((game, date))
}

/// A `*` after a player number in the IGRF roster means they played 0 jams.
fn is_zero_jam_player(number: &str) -> bool {
    number.ends_with('*')
}

fn players_in_jams(periods: &[Period], side: &str) -> HashSet<String> {
    let mut numbers = HashSet::new();
    for period in periods {
        for jam in &period.jams {
            let js = if side == "home" { &jam.home } else { &jam.away };
            if let Some(ref n) = js.jammer {
                numbers.insert(n.clone());
            }
            if let Some(ref n) = js.star_pass_jammer {
                numbers.insert(n.clone());
            }
            for entry in &js.lineup {
                numbers.insert(entry.number.clone());
            }
        }
    }
    numbers
}

fn cell_str(r: &Range<Data>, row: u32, col: u32) -> Option<String> {
    let data = r.get_value((row, col))?;
    match data {
        Data::String(s) => {
            let t = s.trim();
            if t.is_empty() || t.eq_ignore_ascii_case("none") {
                None
            } else {
                Some(t.to_string())
            }
        }
        Data::Int(v) => Some(v.to_string()),
        Data::Float(v) => Some(float_to_int_str(*v)),
        _ => None,
    }
}

fn float_to_int_str(v: f64) -> String {
    if v.fract() == 0.0 && v.is_finite() {
        (v as i64).to_string()
    } else {
        v.to_string()
    }
}

/// Maps Excel cell references like "B14" to their string values from the IGRF sheet.
type IgrfCells = std::collections::HashMap<String, String>;

fn read_igrf_cells<R: std::io::Read + std::io::Seek>(wb: &mut Xlsx<R>) -> Result<IgrfCells> {
    let sheet = wb.worksheet_range("IGRF").context("no IGRF sheet")?;
    let (nrows, ncols) = sheet.get_size();
    let mut map = IgrfCells::default();
    for row in 0..nrows {
        for col in 0..ncols {
            if let Some(val) = cell_str(&sheet, row as u32, col as u32) {
                let ref_key = col_row_to_excel(col as u32, row as u32);
                map.insert(ref_key, val);
            }
        }
    }
    Ok(map)
}

/// Convert 0-indexed (col, row) to Excel cell reference like "B14".
fn col_row_to_excel(col: u32, row: u32) -> String {
    let mut c = col;
    let mut col_str = String::new();
    loop {
        col_str.insert(0, ((c % 26) as u8 + b'A') as char);
        c /= 26;
        if c == 0 {
            break;
        }
        c -= 1;
    }
    format!("{}{}", col_str, row + 1)
}

/// Try to resolve a cell value from the IGRF map using a formula pattern.
/// Handles `IF(ISBLANK(IGRF!$B14),"",IGRF!$B14)` and `IF(IGRF!B14="","",IGRF!B14)`.
fn resolve_igrf_formula(formula: &str, igrf: &IgrfCells) -> Option<String> {
    // Extract the first IGRF cell reference from the formula.
    // Pattern: IGRF!$B14 or IGRF!B14
    let after_igrf = formula.find("IGRF!")?;
    let ref_start = after_igrf + 5; // skip "IGRF!"
    let ref_str = &formula[ref_start..];
    // Strip leading $ signs, read column letter(s) then row number
    let stripped: String = ref_str
        .chars()
        .filter(|&c| c != '$')
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect();
    if stripped.is_empty() {
        return None;
    }
    igrf.get(&stripped).cloned()
}

/// Read a formula from a sheet's formula range, falling back to the IGRF map.
fn cell_str_with_formula(
    data: &Range<Data>,
    formulas: &Option<Range<String>>,
    row: u32,
    col: u32,
    igrf: &IgrfCells,
) -> Option<String> {
    if let Some(val) = cell_str(data, row, col) {
        return Some(val);
    }
    // Try formula resolution.
    if let Some(fm_range) = formulas
        && let Some(fm) = fm_range.get_value((row, col))
        && !fm.is_empty()
    {
        return resolve_igrf_formula(fm, igrf);
    }
    None
}

fn cell_bool(r: &Range<Data>, row: u32, col: u32) -> bool {
    match r.get_value((row, col)) {
        Some(Data::Bool(b)) => *b,
        Some(Data::String(s)) => !s.trim().is_empty(),
        _ => false,
    }
}

fn cell_i16(r: &Range<Data>, row: u32, col: u32) -> i16 {
    r.get_value((row, col))
        .and_then(|d| d.as_i64())
        .unwrap_or(0) as i16
}

fn parse_version<R: std::io::Read + std::io::Seek>(wb: &mut Xlsx<R>) -> String {
    if let Ok(sheet) = wb.worksheet_range("Read Me")
        && let Some(v) = cell_str(&sheet, 2, 0)
    {
        if let Some(m) = v
            .split_whitespace()
            .find(|s| s.len() == 4 && s.chars().all(|c| c.is_ascii_digit()))
        {
            return m.to_string();
        }
        return v;
    }
    "2018".to_string()
}

fn parse_venue<R: std::io::Read + std::io::Seek>(wb: &mut Xlsx<R>) -> Result<Venue> {
    let sheet = wb.worksheet_range("IGRF").context("no IGRF sheet")?;
    Ok(Venue {
        name: cell_str(&sheet, 2, 1),
        city: cell_str(&sheet, 2, 8),
        state: cell_str(&sheet, 2, 10),
    })
}

fn parse_igrf_meta<R: std::io::Read + std::io::Seek>(
    wb: &mut Xlsx<R>,
) -> Result<(Option<String>, Option<String>, Option<chrono::NaiveDate>)> {
    let sheet = wb.worksheet_range("IGRF").context("no IGRF sheet")?;
    let tournament = cell_str(&sheet, 4, 1);
    let host_league = cell_str(&sheet, 4, 8);
    let date = sheet.get_value((6, 1)).and_then(|d| d.as_date());
    Ok((tournament, host_league, date))
}

fn parse_team<R: std::io::Read + std::io::Seek>(
    wb: &mut Xlsx<R>,
    num_col: u32,
    name_col: u32,
    first_row: u32,
    max_skaters: u32,
) -> Result<TeamData> {
    let sheet = wb.worksheet_range("IGRF").context("no IGRF sheet")?;
    let meta_col = if num_col == 1 { 1u32 } else { 8u32 };
    let league = cell_str(&sheet, 9, meta_col);
    let team = cell_str(&sheet, 10, meta_col);
    let color = cell_str(&sheet, 11, meta_col);
    let mut skaters = Vec::new();
    for i in 0..max_skaters {
        let row = first_row + i;
        let raw_number = match cell_str(&sheet, row, num_col) {
            Some(n) => n,
            None => break,
        };
        if raw_number.is_empty() {
            continue;
        }
        if is_zero_jam_player(&raw_number) {
            continue;
        }
        let name = cell_str(&sheet, row, name_col).unwrap_or_default();
        skaters.push(Skater {
            number: raw_number,
            name,
        });
    }
    Ok(TeamData {
        league,
        team,
        color,
        skaters,
    })
}

fn parse_scores<R: std::io::Read + std::io::Seek>(wb: &mut Xlsx<R>) -> Result<Vec<Period>> {
    let sheet = wb.worksheet_range("Score").context("no Score sheet")?;
    let mut periods = Vec::new();
    let period_defs = [(1u8, 3u32, 0u32, 19u32), (2u8, 45u32, 0u32, 19u32)];
    for (period_num, start_row, home_col, away_col) in period_defs {
        let mut jams = Vec::new();
        let mut i = 0u32;
        while i < 38 {
            let row = start_row + i;
            // SP rows between jams belong to the previous jam — the outer loop
            // should never land on one. If we do, skip past it.
            if matches!(sheet.get_value((row, home_col)), Some(Data::String(_))) {
                i += 1;
                continue;
            }
            let jam_num = match sheet.get_value((row, home_col)) {
                Some(d) => match d.as_i64() {
                    Some(0) | None => break,
                    Some(n) => n as u16,
                },
                None => break,
            };
            let mut home = parse_jam_side(&sheet, row, home_col);
            let mut away = parse_jam_side(&sheet, row, away_col);
            i += 1;
            // The row immediately after a jam may be an SP row carrying a star
            // pass jammer and trip scores for one or both sides.
            let sp_row = start_row + i;
            let home_sp = matches!(sheet.get_value((sp_row, home_col)), Some(Data::String(_)));
            let away_sp = matches!(sheet.get_value((sp_row, away_col)), Some(Data::String(_)));
            if home_sp {
                home.star_pass_jammer = cell_str(&sheet, sp_row, home_col + 1);
                home.score += (7..=15u32)
                    .map(|c| cell_i16(&sheet, sp_row, home_col + c))
                    .sum::<i16>();
            }
            if away_sp {
                away.star_pass_jammer = cell_str(&sheet, sp_row, away_col + 1);
                away.score += (7..=15u32)
                    .map(|c| cell_i16(&sheet, sp_row, away_col + c))
                    .sum::<i16>();
            }
            if home_sp || away_sp {
                i += 1;
            }
            jams.push(Jam {
                number: jam_num,
                home,
                away,
            });
        }
        if !jams.is_empty() {
            periods.push(Period {
                number: period_num,
                jams,
            });
        }
    }
    Ok(periods)
}

fn parse_jam_side(sheet: &Range<Data>, row: u32, base_col: u32) -> JamSide {
    let jammer = cell_str(sheet, row, base_col + 1);
    let lost = cell_bool(sheet, row, base_col + 2);
    let lead = cell_bool(sheet, row, base_col + 3);
    let called = cell_bool(sheet, row, base_col + 4);
    let injury = cell_bool(sheet, row, base_col + 5);
    let no_pivot = cell_bool(sheet, row, base_col + 6);
    let score: i16 = (7..=15u32)
        .map(|c| cell_i16(sheet, row, base_col + c))
        .sum();
    JamSide {
        jammer,
        star_pass_jammer: None,
        lead,
        lost,
        called,
        injury,
        no_pivot,
        score,
        lineup: vec![],
    }
}

fn merge_lineups<R: std::io::Read + std::io::Seek>(
    wb: &mut Xlsx<R>,
    periods: &mut [Period],
) -> Result<()> {
    let sheet = match wb.worksheet_range("Lineups") {
        Ok(s) => s,
        Err(_) => return Ok(()),
    };
    let defs = [(0usize, 3u32, 1u32, 27u32), (1usize, 45u32, 1u32, 27u32)];
    for (pi, start_row, home_np_col, away_np_col) in defs {
        let Some(period) = periods.get_mut(pi) else {
            continue;
        };
        let mut row = start_row;
        for jam in period.jams.iter_mut() {
            // Skip SP (star pass) rows in the Lineups sheet. An empty
            // string in column A means the cell exists but is blank, not SP.
            while let Some(Data::String(s)) = sheet.get_value((row, 0u32)) {
                if s.trim().is_empty() {
                    break;
                }
                row += 1;
            }
            jam.home.lineup = parse_lineup_side(&sheet, row, home_np_col);
            jam.away.lineup = parse_lineup_side(&sheet, row, away_np_col);
            row += 1;
        }
    }
    Ok(())
}

fn parse_lineup_side(sheet: &Range<Data>, row: u32, no_pivot_col: u32) -> Vec<LineupEntry> {
    let jammer_col = no_pivot_col + 1;
    let positions = [
        (jammer_col, "jammer"),
        (jammer_col + 4, "pivot"),
        (jammer_col + 8, "blocker"),
        (jammer_col + 12, "blocker"),
        (jammer_col + 16, "blocker"),
    ];
    positions
        .iter()
        .filter_map(|(col, pos)| {
            let number = cell_str(sheet, row, *col)?;
            if is_zero_jam_player(&number) {
                return None;
            }
            let box_trips = (1..=3u32)
                .filter(|&b| cell_str(sheet, row, col + b).is_some())
                .count() as u8;
            Some(LineupEntry {
                number,
                position: pos.to_string(),
                box_trips,
            })
        })
        .collect()
}

fn parse_penalties<R: std::io::Read + std::io::Seek>(
    wb: &mut Xlsx<R>,
    igrf: &IgrfCells,
) -> Result<Vec<Penalty>> {
    match parse_penalties_sheet(wb, "Penalties", igrf) {
        Ok(p) if !p.is_empty() => Ok(p),
        _ => parse_penalties_sheet(wb, "Penalties-Lineups", igrf),
    }
}

fn parse_penalties_sheet<R: std::io::Read + std::io::Seek>(
    wb: &mut Xlsx<R>,
    sheet_name: &str,
    igrf: &IgrfCells,
) -> Result<Vec<Penalty>> {
    let sheet = match wb.worksheet_range(sheet_name) {
        Ok(s) => s,
        Err(_) => return Ok(vec![]),
    };
    let formulas = wb.worksheet_formula(sheet_name).ok();

    let mut penalties = Vec::new();
    let sections: &[(u8, &str, u32, u32, u32)] = &[
        (1, "home", 0, 1, 10),
        (1, "away", 15, 16, 25),
        (2, "home", 28, 29, 38),
        (2, "away", 43, 44, 53),
    ];
    for &(period, side, skater_col, pen_col, fo_col) in sections {
        for i in 0..20u32 {
            let code_row = 3 + i * 2;
            let jam_row = code_row + 1;
            let skater_num =
                match cell_str_with_formula(&sheet, &formulas, code_row, skater_col, igrf) {
                    Some(n) => n,
                    None => break,
                };
            if is_zero_jam_player(&skater_num) {
                continue;
            }
            for c in pen_col..(pen_col + 9) {
                if let Some(code) = cell_str(&sheet, code_row, c) {
                    let jam = cell_i16(&sheet, jam_row, c);
                    penalties.push(Penalty {
                        number: skater_num.clone(),
                        side: side.to_string(),
                        period,
                        jam: if jam > 0 { Some(jam as u16) } else { None },
                        code,
                        foul_out: false,
                        expulsion: false,
                    });
                }
            }
            if let Some(fo_code) = cell_str(&sheet, code_row, fo_col) {
                let fo_jam = cell_i16(&sheet, jam_row, fo_col);
                penalties.push(Penalty {
                    number: skater_num.clone(),
                    side: side.to_string(),
                    period,
                    jam: if fo_jam > 0 {
                        Some(fo_jam as u16)
                    } else {
                        None
                    },
                    code: fo_code,
                    foul_out: true,
                    expulsion: false,
                });
            }
        }
    }
    Ok(penalties)
}

fn parse_game_summary<R: std::io::Read + std::io::Seek>(
    wb: &mut Xlsx<R>,
    igrf: &IgrfCells,
) -> Result<GameSummary> {
    let sheet = wb
        .worksheet_range("Game Summary")
        .context("no Game Summary sheet")?;
    let formulas = wb.worksheet_formula("Game Summary").ok();
    let home_players = (5u32..=24)
        .filter_map(|row| parse_summary_player(&sheet, &formulas, row, igrf))
        .collect();
    let away_players = (27u32..=46)
        .filter_map(|row| parse_summary_player(&sheet, &formulas, row, igrf))
        .collect();
    Ok(GameSummary {
        home_totals: parse_summary_totals(&sheet, 25),
        away_totals: parse_summary_totals(&sheet, 47),
        home_players,
        away_players,
    })
}

fn parse_summary_player(
    sheet: &Range<Data>,
    formulas: &Option<Range<String>>,
    row: u32,
    igrf: &IgrfCells,
) -> Option<SummaryPlayer> {
    let number = cell_str_with_formula(sheet, formulas, row, 0, igrf)?;
    if is_zero_jam_player(&number) {
        return None;
    }
    let name = cell_str_with_formula(sheet, formulas, row, 1, igrf).unwrap_or_default();
    let summary = SummaryPlayer {
        number,
        name,
        jams_jammer: cell_opt_u8(sheet, row, 2),
        jams_pivot: cell_opt_u8(sheet, row, 3),
        jams_blocker: cell_opt_u8(sheet, row, 4),
        jams_total: cell_opt_u8(sheet, row, 5),
        jams_pct: cell_opt_f32(sheet, row, 6),
        jammer_points: cell_opt_i16(sheet, row, 7),
        ppj: cell_opt_f32(sheet, row, 8),
        lost: cell_opt_u8(sheet, row, 9),
        lead: cell_opt_u8(sheet, row, 10),
        called: cell_opt_u8(sheet, row, 11),
        no_initial_trip: cell_opt_u8(sheet, row, 12),
        star_passes: cell_opt_u8(sheet, row, 13),
        lead_pct: cell_opt_f32(sheet, row, 14),
        lead_plus_minus: cell_opt_i16(sheet, row, 15),
        avg_lead_plus_minus: cell_opt_f32(sheet, row, 16),
        pts_for: cell_opt_i16(sheet, row, 17),
        pts_against: cell_opt_i16(sheet, row, 18),
        plus_minus: cell_opt_i16(sheet, row, 19),
        jammer_plus_minus: cell_opt_i16(sheet, row, 20),
        avg_jammer_plus_minus: cell_opt_f32(sheet, row, 21),
        pivot_plus_minus: cell_opt_i16(sheet, row, 22),
        avg_pivot_plus_minus: cell_opt_f32(sheet, row, 23),
        block_plus_minus: cell_opt_i16(sheet, row, 24),
        avg_block_plus_minus: cell_opt_f32(sheet, row, 25),
        pack_plus_minus: cell_opt_i16(sheet, row, 26),
        avg_pack_plus_minus: cell_opt_f32(sheet, row, 27),
        avg_plus_minus: cell_opt_f32(sheet, row, 28),
        vtar_pts_for: cell_opt_f32(sheet, row, 29),
        vtar_pts_against: cell_opt_f32(sheet, row, 30),
        vtar_total_plus_minus: cell_opt_f32(sheet, row, 31),
        vtar_jammer_avg_plus_minus: cell_opt_f32(sheet, row, 32),
        vtar_pivot_avg_plus_minus: cell_opt_f32(sheet, row, 33),
        vtar_blocker_avg_plus_minus: cell_opt_f32(sheet, row, 34),
        vtar_pack_avg_plus_minus: cell_opt_f32(sheet, row, 35),
        total_vtar_avg_plus_minus: cell_opt_f32(sheet, row, 36),
        penalty_count: cell_opt_u8(sheet, row, 37),
    };
    if summary.jams_total == Some(0) {
        return None;
    }
    Some(summary)
}

fn parse_summary_totals(sheet: &Range<Data>, row: u32) -> SummaryTotals {
    SummaryTotals {
        jams_jammer: cell_opt_u8(sheet, row, 2),
        jams_pivot: cell_opt_u8(sheet, row, 3),
        jams_blocker: cell_opt_u8(sheet, row, 4),
        jams_total: cell_opt_u16(sheet, row, 5),
        jams_pct: cell_opt_f32(sheet, row, 6),
        jammer_points: cell_opt_i16(sheet, row, 7),
        ppj: cell_opt_f32(sheet, row, 8),
        lost: cell_opt_u8(sheet, row, 9),
        lead: cell_opt_u8(sheet, row, 10),
        called: cell_opt_u8(sheet, row, 11),
        no_initial_trip: cell_opt_u8(sheet, row, 12),
        star_passes: cell_opt_u8(sheet, row, 13),
        lead_pct: cell_opt_f32(sheet, row, 14),
        lead_plus_minus: cell_opt_i16(sheet, row, 15),
        avg_lead_plus_minus: cell_opt_f32(sheet, row, 16),
        pts_for: cell_opt_f32(sheet, row, 17),
        pts_against: cell_opt_f32(sheet, row, 18),
        plus_minus: cell_opt_f32(sheet, row, 19),
        jammer_plus_minus: cell_opt_f32(sheet, row, 20),
        avg_jammer_plus_minus: cell_opt_f32(sheet, row, 21),
        pivot_plus_minus: cell_opt_f32(sheet, row, 22),
        avg_pivot_plus_minus: cell_opt_f32(sheet, row, 23),
        block_plus_minus: cell_opt_f32(sheet, row, 24),
        avg_block_plus_minus: cell_opt_f32(sheet, row, 25),
        pack_plus_minus: cell_opt_f32(sheet, row, 26),
        avg_pack_plus_minus: cell_opt_f32(sheet, row, 27),
        avg_plus_minus: cell_opt_f32(sheet, row, 28),
        vtar_pts_for: cell_opt_f32(sheet, row, 29),
        vtar_pts_against: cell_opt_f32(sheet, row, 30),
        vtar_total_plus_minus: cell_opt_f32(sheet, row, 31),
        vtar_jammer_avg_plus_minus: cell_opt_f32(sheet, row, 32),
        vtar_pivot_avg_plus_minus: cell_opt_f32(sheet, row, 33),
        vtar_blocker_avg_plus_minus: cell_opt_f32(sheet, row, 34),
        vtar_pack_avg_plus_minus: cell_opt_f32(sheet, row, 35),
        total_vtar_avg_plus_minus: cell_opt_f32(sheet, row, 36),
        penalty_count: cell_opt_u8(sheet, row, 37),
    }
}

fn cell_opt_i16(sheet: &Range<Data>, row: u32, col: u32) -> Option<i16> {
    match sheet.get_value((row, col))? {
        Data::String(s) if s.trim().eq_ignore_ascii_case("none") => None,
        Data::String(s) => s.trim().parse().ok(),
        Data::Float(f) => Some(f.round() as i16),
        Data::Int(i) => Some(*i as i16),
        _ => None,
    }
}

fn cell_opt_u8(sheet: &Range<Data>, row: u32, col: u32) -> Option<u8> {
    match sheet.get_value((row, col))? {
        Data::String(s) if s.trim().eq_ignore_ascii_case("none") => None,
        Data::String(s) => s.trim().parse().ok(),
        Data::Float(f) => Some(f.round() as u8),
        Data::Int(i) => Some(*i as u8),
        _ => None,
    }
}

fn cell_opt_u16(sheet: &Range<Data>, row: u32, col: u32) -> Option<u16> {
    match sheet.get_value((row, col))? {
        Data::String(s) if s.trim().eq_ignore_ascii_case("none") => None,
        Data::String(s) => s.trim().parse().ok(),
        Data::Float(f) => Some(f.round() as u16),
        Data::Int(i) => Some(*i as u16),
        _ => None,
    }
}

fn cell_opt_f32(sheet: &Range<Data>, row: u32, col: u32) -> Option<f32> {
    match sheet.get_value((row, col))? {
        Data::String(s) if s.trim().eq_ignore_ascii_case("none") => None,
        Data::String(s) => s.trim().parse().ok(),
        Data::Float(f) => Some(*f as f32),
        Data::Int(i) => Some(*i as f32),
        _ => None,
    }
}
