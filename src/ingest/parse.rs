use crate::models::*;
use anyhow::{Context, Result};
use calamine::{Data, DataType, Range, Reader, Xlsx, open_workbook_from_rs};
use chrono::Datelike;
use formualizer_common::LiteralValue;
use formualizer_workbook::{
    LoadStrategy, SpreadsheetReader, Workbook, WorkbookConfig, backends::CalamineAdapter,
};
use std::collections::{HashMap, HashSet};
use std::io::Cursor;

/// Bump this whenever the parsing logic changes, so the ingester can re-parse
/// games that were ingested with an older version of the parser.
pub const PARSER_VERSION: i64 = 18;

pub fn parse_statsbook(bytes: &[u8]) -> Result<GameData> {
    let (game, _) = parse_statsbook_with_date(bytes, None)?;
    Ok(game)
}

/// Parse a statsbook. `file_name`, when provided, is used as a last-resort
/// source for the game date (some uploaders leave the IGRF date cell blank or
/// typed as text; the file name always carries the date per the
/// `[WFTDA]STATS-YYYY-MM-DD_...` convention).
pub fn parse_statsbook_with_date(
    bytes: &[u8],
    file_name: Option<&str>,
) -> Result<(GameData, Option<chrono::NaiveDate>)> {
    let cursor = Cursor::new(bytes);
    let mut wb: Xlsx<_> = open_workbook_from_rs(cursor).context("failed to open xlsx workbook")?;

    let version = parse_version(&mut wb);
    let venue = parse_venue(&mut wb)?;
    let (tournament, host_league, date) = parse_igrf_meta(&mut wb, file_name)?;
    let mut home = parse_team(&mut wb, 1, 2, 13, 20)?;
    let mut away = parse_team(&mut wb, 8, 9, 13, 20)?;
    let mut periods = parse_scores(&mut wb)?;
    merge_lineups(&mut wb, &mut periods)?;
    let home_players = players_in_jams(&periods, Side::Home);
    let away_players = players_in_jams(&periods, Side::Away);
    home.skaters.retain(|s| home_players.contains(&s.number));
    away.skaters.retain(|s| away_players.contains(&s.number));
    let igrf = read_igrf_cells(&mut wb)?;
    let penalties = parse_penalties(&mut wb, &igrf)?;
    let home_jam_counts = compute_jam_counts(&periods, Side::Home);
    let away_jam_counts = compute_jam_counts(&periods, Side::Away);

    // Prefer the statsbook's own cached Game Summary values when present and
    // non-trivial: they are the official calculated numbers for the game.
    // Formualizer's re-evaluation over-counts jammer jams on statsbooks that
    // fill in the SP* (opposing-team star pass) rows with jammer numbers, so
    // only fall back to formula evaluation when the cached sheet is blank
    // (files saved without calculating, e.g. the standbys-flatiron fixture).
    let gs_cached = wb.worksheet_range("Game Summary").ok();

    let game_summary = parse_game_summary(
        &mut wb,
        &igrf,
        &home_players,
        &away_players,
        &home_jam_counts,
        &away_jam_counts,
    )
    .ok()
    .filter(|gs| {
        gs.home_totals.jams_total.unwrap_or(0) > 0 && gs.away_totals.jams_total.unwrap_or(0) > 0
    })
    .or_else(|| {
        parse_game_summary_formualizer(bytes, &home_players, &away_players, gs_cached.as_ref())
    });

    let (home_score, away_score) = parse_igrf_scores(&mut wb).unwrap_or_else(|| {
        (
            periods_score(&periods, Side::Home),
            periods_score(&periods, Side::Away),
        )
    });
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

fn players_in_jams(periods: &[Period], side: Side) -> HashSet<String> {
    let mut numbers = HashSet::new();
    for period in periods {
        for jam in &period.jams {
            let js = jam.side(side);
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

/// Count how many jams each player played as jammer, pivot, and blocker.
fn compute_jam_counts(periods: &[Period], side: Side) -> HashMap<String, JamCounts> {
    let mut counts: HashMap<String, JamCounts> = HashMap::new();
    for period in periods {
        for jam in &period.jams {
            let js = jam.side(side);
            for entry in &js.lineup {
                let e = counts.entry(entry.number.clone()).or_default();
                match entry.position.as_str() {
                    "jammer" => e.jammer += 1,
                    "pivot" => e.pivot += 1,
                    "blocker" => e.blocker += 1,
                    _ => {}
                }
                e.total += 1;
            }
        }
    }
    counts
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
    file_name: Option<&str>,
) -> Result<(Option<String>, Option<String>, Option<chrono::NaiveDate>)> {
    let sheet = wb.worksheet_range("IGRF").context("no IGRF sheet")?;
    let tournament = cell_str(&sheet, 4, 1);
    let host_league = cell_str(&sheet, 4, 8);
    let date = igrf_date(&sheet, file_name);
    Ok((tournament, host_league, date))
}

/// Resolve the game date from the IGRF (6, 1) cell. Uploaders occasionally
/// type the date as text instead of a real Excel date, or leave the cell
/// blank; fall back from the cached DateTime value to text parsing, then to
/// the date embedded in the file name.
fn igrf_date(sheet: &Range<Data>, file_name: Option<&str>) -> Option<chrono::NaiveDate> {
    if let Some(d) = sheet.get_value((6, 1)).and_then(|d| d.as_date()) {
        return Some(d);
    }
    let file_date = file_name.and_then(date_from_file_name);
    cell_str(sheet, 6, 1)
        .as_deref()
        .and_then(|t| parse_text_date(t, file_date))
        .or(file_date)
}

/// Parse a date typed as text into the IGRF date cell (e.g. "2026-06-27",
/// "2025/02/08", "Sept 7 2024"). For the ambiguous `MM/DD/YYYY` vs
/// `DD/MM/YYYY` slash forms, `file_date` (from the file name, which always
/// uses ISO order) disambiguates; without it, month-first is the default.
fn parse_text_date(text: &str, file_date: Option<chrono::NaiveDate>) -> Option<chrono::NaiveDate> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    // WFTDA games never predate the 1990s and a correct date is never more
    // than a day in the future, so anything outside that range (e.g. a 2-digit
    // year parsing as "24" -> AD 24) is a misparse: reject it so the file-name
    // date in igrf_date gets a chance to provide the real date.
    let plausible =
        |d: chrono::NaiveDate| d.year() >= 1990 && d.year() <= chrono::Utc::now().year() + 1;
    for fmt in ["%Y-%m-%d", "%Y/%m/%d"] {
        if let Some(d) = chrono::NaiveDate::parse_from_str(text, fmt).ok()
            && plausible(d)
        {
            return Some(d);
        }
    }
    if let Some(d) = parse_month_name_date(text)
        && plausible(d)
    {
        return Some(d);
    }
    // Ambiguous slash forms: both interpretations may be valid (e.g. "08/06/2024"
    // is Aug 6 or Jun 8). Prefer the interpretation matching the file-name date;
    // otherwise default to month-first. Both interpretations share the parsed
    // year, so the plausibility check applies to either.
    let month_first = chrono::NaiveDate::parse_from_str(text, "%m/%d/%Y").ok();
    let day_first = chrono::NaiveDate::parse_from_str(text, "%d/%m/%Y").ok();
    match (month_first, day_first) {
        (Some(mf), Some(df)) if mf != df && file_date == Some(df) => Some(df),
        (Some(mf), _) if plausible(mf) => Some(mf),
        (None, Some(df)) if plausible(df) => Some(df),
        _ => None,
    }
}

/// Parse a month-name date like "Sept 7 2024" or "7 Sep 2024". chrono's
/// `%b`/`%B` parsing is unsupported, so the month token is mapped to a number
/// by hand and the remaining day/year tokens are parsed numerically.
fn parse_month_name_date(text: &str) -> Option<chrono::NaiveDate> {
    let month_of = |t: &str| -> Option<u32> {
        let lower = t.to_ascii_lowercase();
        match lower.as_str() {
            "january" => Some(1),
            "february" => Some(2),
            "march" => Some(3),
            "april" => Some(4),
            "may" => Some(5),
            "june" => Some(6),
            "july" => Some(7),
            "august" => Some(8),
            "september" => Some(9),
            "october" => Some(10),
            "november" => Some(11),
            "december" => Some(12),
            _ => None,
        }
    };
    let abbr_of = |t: &str| -> Option<u32> {
        // "sept" is a common 4-letter abbreviation of September; every other
        // abbreviation is exactly 3 letters.
        let lower = t.to_ascii_lowercase();
        let stem = match lower.as_str() {
            "sept" => "sep",
            other => other,
        };
        match stem {
            "jan" => Some(1),
            "feb" => Some(2),
            "mar" => Some(3),
            "apr" => Some(4),
            "may" => Some(5),
            "jun" => Some(6),
            "jul" => Some(7),
            "aug" => Some(8),
            "sep" => Some(9),
            "oct" => Some(10),
            "nov" => Some(11),
            "dec" => Some(12),
            _ => None,
        }
    };
    // Tolerate trailing punctuation ("Sept. 7, 2024", "7 Sep., 2024").
    let tokens: Vec<&str> = text
        .split_whitespace()
        .map(|t| t.trim_end_matches(['.', ',']))
        .collect();
    if tokens.len() != 3 {
        return None;
    }
    // Month token is first ("Sept 7 2024") or second ("7 Sep 2024").
    for (idx, tok) in tokens.iter().enumerate() {
        let Some(month) = month_of(tok).or_else(|| abbr_of(tok)) else {
            continue;
        };
        if idx == 0 {
            let day: u32 = tokens[1].parse().ok()?;
            let year: i32 = tokens[2].parse().ok()?;
            return chrono::NaiveDate::from_ymd_opt(year, month, day);
        }
        if idx == 1 {
            let day: u32 = tokens[0].parse().ok()?;
            let year: i32 = tokens[2].parse().ok()?;
            return chrono::NaiveDate::from_ymd_opt(year, month, day);
        }
    }
    None
}

/// Extract the game date from the file name, which follows the
/// `[WFTDA]STATS-YYYY-MM-DD_{league}_{team}_vs_...xlsx` convention.
fn date_from_file_name(file_name: &str) -> Option<chrono::NaiveDate> {
    let marker = "STATS-";
    let start = file_name.find(marker)? + marker.len();
    let date_str = file_name.get(start..start + 10)?;
    chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d").ok()
}

/// Read the official final score from the IGRF sheet's TOTAL POINTS row (row 38 in Excel, row 37 0-indexed).
/// Returns (home_score, away_score) only when both cells hold a usable numeric value
/// (i.e. the formula `IF(COUNT(...)=0,"",SUM(...))` has a cached numeric result).
/// Returns None to signal fallback to jam summing whenever the row is missing, the label is wrong,
/// or either score cell is empty/non-numeric (which happens when a writer saved the file without
/// evaluating formulas — calamine only sees cached values).
fn parse_igrf_scores<R: std::io::Read + std::io::Seek>(wb: &mut Xlsx<R>) -> Option<(i16, i16)> {
    let sheet = wb.worksheet_range("IGRF").ok()?;
    // Require the TOTAL POINTS label so we don't trust a misaligned row.
    let label = cell_str(&sheet, 37, 0)?;
    if !label.contains("TOTAL POINTS") {
        return None;
    }
    // Both score cells must contain a cached numeric value; otherwise fall back to jam summing.
    let home = sheet.get_value((37, 2)).and_then(|d| d.as_i64())? as i16;
    let away = sheet.get_value((37, 9)).and_then(|d| d.as_i64())? as i16;
    Some((home, away))
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
    let mut league = cell_str(&sheet, 9, meta_col);
    let mut team = cell_str(&sheet, 10, meta_col);
    let color = cell_str(&sheet, 11, meta_col);
    // Some uploaders fill in only one of the two header cells (the single-team
    // convention); recover the missing value from the other. If both are blank
    // the side stays anonymous and the game can't be fingerprinted — that's
    // an unrecoverable statsbook, not a case to guess about.
    if team.is_none() {
        team = league.clone();
    }
    if league.is_none() {
        league = team.clone();
    }
    let mut skaters = Vec::new();
    // Some statsbooks list the same skater number twice in the roster (e.g.
    // Copenhagen B vs Rolling Rat Pack 2024-05-25 has #390 twice). A number is
    // the player's identity within a side, and the game_skaters table has a
    // (game_id, side, number) primary key, so keep only the first occurrence.
    let mut seen_numbers = HashSet::new();
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
        if !seen_numbers.insert(raw_number.clone()) {
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

/// Load the statsbook through formualizer to get fully-evaluated Game Summary
/// cells (formualizer evaluates Excel formulas that calamine only sees as cached
/// zeroes). Falls back to `None` on any error so callers can use the calamine path.
fn parse_game_summary_formualizer(
    bytes: &[u8],
    home_numbers: &HashSet<String>,
    away_numbers: &HashSet<String>,
    gs_cached: Option<&Range<Data>>,
) -> Option<GameSummary> {
    let adapter = CalamineAdapter::open_bytes(bytes.to_vec()).ok()?;
    let cfg = WorkbookConfig::ephemeral();
    let mut wb = Workbook::from_reader(adapter, LoadStrategy::EagerAll, cfg).ok()?;
    wb.evaluate_all().ok()?;

    let home_players: Vec<SummaryPlayer> = (6u32..=25)
        .filter_map(|row| read_summary_player(&wb, gs_cached, row))
        .filter(|p| !is_zero_jam_player(&p.number) && home_numbers.contains(&p.number))
        .collect();
    let away_players: Vec<SummaryPlayer> = (28u32..=47)
        .filter_map(|row| read_summary_player(&wb, gs_cached, row))
        .filter(|p| !is_zero_jam_player(&p.number) && away_numbers.contains(&p.number))
        .collect();

    Some(GameSummary {
        home_totals: read_summary_totals(&wb, gs_cached, 26),
        away_totals: read_summary_totals(&wb, gs_cached, 48),
        home_players,
        away_players,
    })
}

/// Read a Game Summary cell from formualizer with calamine-cached fallback for errors.
fn fv_val(wb: &Workbook, cached: Option<&Range<Data>>, row: u32, col: u32) -> Option<LiteralValue> {
    match wb.get_value("Game Summary", row, col) {
        Some(LiteralValue::Error(_)) => {
            // Formualizer couldn't evaluate this formula — fall back to calamine cached.
            let d = cached?.get_value((row - 1, col - 1))?;
            Some(match d {
                Data::String(s) => LiteralValue::Text(s.clone()),
                Data::Float(f) => LiteralValue::Number(*f),
                Data::Int(i) => LiteralValue::Int(*i),
                Data::Bool(b) => LiteralValue::Boolean(*b),
                Data::Empty => return None,
                _ => return None,
            })
        }
        other => other,
    }
}

fn read_summary_player(
    wb: &Workbook,
    cached: Option<&Range<Data>>,
    row: u32,
) -> Option<SummaryPlayer> {
    let number = lv_string(fv_val(wb, cached, row, 1))?;
    let name = lv_string(fv_val(wb, cached, row, 2)).unwrap_or_default();
    let summary = SummaryPlayer {
        number,
        name,
        jams_jammer: lv_u8(fv_val(wb, cached, row, 3)),
        jams_pivot: lv_u8(fv_val(wb, cached, row, 4)),
        jams_blocker: lv_u8(fv_val(wb, cached, row, 5)),
        jams_total: lv_u8(fv_val(wb, cached, row, 6)),
        jams_pct: lv_f32(fv_val(wb, cached, row, 7)),
        jammer_points: lv_i16(fv_val(wb, cached, row, 8)),
        ppj: lv_f32(fv_val(wb, cached, row, 9)),
        lost: lv_u8(fv_val(wb, cached, row, 10)),
        lead: lv_u8(fv_val(wb, cached, row, 11)),
        called: lv_u8(fv_val(wb, cached, row, 12)),
        no_initial_trip: lv_u8(fv_val(wb, cached, row, 13)),
        star_passes: lv_u8(fv_val(wb, cached, row, 14)),
        lead_pct: lv_f32(fv_val(wb, cached, row, 15)),
        lead_plus_minus: lv_i16(fv_val(wb, cached, row, 16)),
        avg_lead_plus_minus: lv_f32(fv_val(wb, cached, row, 17)),
        pts_for: lv_i16(fv_val(wb, cached, row, 18)),
        pts_against: lv_i16(fv_val(wb, cached, row, 19)),
        plus_minus: lv_i16(fv_val(wb, cached, row, 20)),
        jammer_plus_minus: lv_i16(fv_val(wb, cached, row, 21)),
        avg_jammer_plus_minus: lv_f32(fv_val(wb, cached, row, 22)),
        pivot_plus_minus: lv_i16(fv_val(wb, cached, row, 23)),
        avg_pivot_plus_minus: lv_f32(fv_val(wb, cached, row, 24)),
        block_plus_minus: lv_i16(fv_val(wb, cached, row, 25)),
        avg_block_plus_minus: lv_f32(fv_val(wb, cached, row, 26)),
        pack_plus_minus: lv_i16(fv_val(wb, cached, row, 27)),
        avg_pack_plus_minus: lv_f32(fv_val(wb, cached, row, 28)),
        avg_plus_minus: lv_f32(fv_val(wb, cached, row, 29)),
        vtar_pts_for: lv_f32(fv_val(wb, cached, row, 30)),
        vtar_pts_against: lv_f32(fv_val(wb, cached, row, 31)),
        vtar_total_plus_minus: lv_f32(fv_val(wb, cached, row, 32)),
        vtar_jammer_avg_plus_minus: lv_f32(fv_val(wb, cached, row, 33)),
        vtar_pivot_avg_plus_minus: lv_f32(fv_val(wb, cached, row, 34)),
        vtar_blocker_avg_plus_minus: lv_f32(fv_val(wb, cached, row, 35)),
        vtar_pack_avg_plus_minus: lv_f32(fv_val(wb, cached, row, 36)),
        total_vtar_avg_plus_minus: lv_f32(fv_val(wb, cached, row, 37)),
        penalty_count: lv_u8(fv_val(wb, cached, row, 38)),
    };
    if summary.jams_total == Some(0) {
        return None;
    }
    Some(summary)
}

fn read_summary_totals(wb: &Workbook, cached: Option<&Range<Data>>, row: u32) -> SummaryTotals {
    SummaryTotals {
        jams_jammer: lv_u8(fv_val(wb, cached, row, 3)),
        jams_pivot: lv_u8(fv_val(wb, cached, row, 4)),
        jams_blocker: lv_u8(fv_val(wb, cached, row, 5)),
        jams_total: lv_u16(fv_val(wb, cached, row, 6)),
        jams_pct: lv_f32(fv_val(wb, cached, row, 7)),
        jammer_points: lv_i16(fv_val(wb, cached, row, 8)),
        ppj: lv_f32(fv_val(wb, cached, row, 9)),
        lost: lv_u8(fv_val(wb, cached, row, 10)),
        lead: lv_u8(fv_val(wb, cached, row, 11)),
        called: lv_u8(fv_val(wb, cached, row, 12)),
        no_initial_trip: lv_u8(fv_val(wb, cached, row, 13)),
        star_passes: lv_u8(fv_val(wb, cached, row, 14)),
        lead_pct: lv_f32(fv_val(wb, cached, row, 15)),
        lead_plus_minus: lv_i16(fv_val(wb, cached, row, 16)),
        avg_lead_plus_minus: lv_f32(fv_val(wb, cached, row, 17)),
        pts_for: lv_f32(fv_val(wb, cached, row, 18)),
        pts_against: lv_f32(fv_val(wb, cached, row, 19)),
        plus_minus: lv_f32(fv_val(wb, cached, row, 20)),
        jammer_plus_minus: lv_f32(fv_val(wb, cached, row, 21)),
        avg_jammer_plus_minus: lv_f32(fv_val(wb, cached, row, 22)),
        pivot_plus_minus: lv_f32(fv_val(wb, cached, row, 23)),
        avg_pivot_plus_minus: lv_f32(fv_val(wb, cached, row, 24)),
        block_plus_minus: lv_f32(fv_val(wb, cached, row, 25)),
        avg_block_plus_minus: lv_f32(fv_val(wb, cached, row, 26)),
        pack_plus_minus: lv_f32(fv_val(wb, cached, row, 27)),
        avg_pack_plus_minus: lv_f32(fv_val(wb, cached, row, 28)),
        avg_plus_minus: lv_f32(fv_val(wb, cached, row, 29)),
        vtar_pts_for: lv_f32(fv_val(wb, cached, row, 30)),
        vtar_pts_against: lv_f32(fv_val(wb, cached, row, 31)),
        vtar_total_plus_minus: lv_f32(fv_val(wb, cached, row, 32)),
        vtar_jammer_avg_plus_minus: lv_f32(fv_val(wb, cached, row, 33)),
        vtar_pivot_avg_plus_minus: lv_f32(fv_val(wb, cached, row, 34)),
        vtar_blocker_avg_plus_minus: lv_f32(fv_val(wb, cached, row, 35)),
        vtar_pack_avg_plus_minus: lv_f32(fv_val(wb, cached, row, 36)),
        total_vtar_avg_plus_minus: lv_f32(fv_val(wb, cached, row, 37)),
        penalty_count: lv_u8(fv_val(wb, cached, row, 38)),
    }
}

/// Convert a LiteralValue to an optional string.
fn lv_string(v: Option<LiteralValue>) -> Option<String> {
    match v? {
        LiteralValue::Text(s) => {
            let t = s.trim();
            if t.is_empty() || t.eq_ignore_ascii_case("none") {
                None
            } else {
                Some(t.to_string())
            }
        }
        LiteralValue::Number(n) if n.is_finite() => {
            if n.fract() == 0.0 {
                Some((n as i64).to_string())
            } else {
                Some(n.to_string())
            }
        }
        LiteralValue::Int(i) => Some(i.to_string()),
        _ => None,
    }
}

fn lv_u8(v: Option<LiteralValue>) -> Option<u8> {
    match v? {
        LiteralValue::Number(n) if n.is_finite() => Some((n.round() as i64).clamp(0, 255) as u8),
        LiteralValue::Int(i) => Some(i.clamp(0, 255) as u8),
        LiteralValue::Text(s) => {
            if s.trim().is_empty() {
                Some(0)
            } else {
                s.trim().parse().ok()
            }
        }
        _ => None,
    }
}

fn lv_u16(v: Option<LiteralValue>) -> Option<u16> {
    match v? {
        LiteralValue::Number(n) if n.is_finite() => Some((n.round() as i64).clamp(0, 65535) as u16),
        LiteralValue::Int(i) => Some(i.clamp(0, 65535) as u16),
        LiteralValue::Text(s) => {
            if s.trim().is_empty() {
                Some(0)
            } else {
                s.trim().parse().ok()
            }
        }
        _ => None,
    }
}

fn lv_i16(v: Option<LiteralValue>) -> Option<i16> {
    match v? {
        LiteralValue::Number(n) if n.is_finite() => {
            Some((n.round() as i64).clamp(i16::MIN as i64, i16::MAX as i64) as i16)
        }
        LiteralValue::Int(i) => Some(i.clamp(i16::MIN as i64, i16::MAX as i64) as i16),
        LiteralValue::Text(s) => {
            if s.trim().is_empty() {
                Some(0)
            } else {
                s.trim().parse().ok()
            }
        }
        _ => None,
    }
}

fn lv_f32(v: Option<LiteralValue>) -> Option<f32> {
    match v? {
        LiteralValue::Number(n) if n.is_finite() => Some(n as f32),
        LiteralValue::Int(i) => Some(i as f32),
        LiteralValue::Text(s) => {
            if s.trim().is_empty() {
                Some(0.0)
            } else {
                s.trim().parse().ok()
            }
        }
        _ => None,
    }
}

fn parse_game_summary<R: std::io::Read + std::io::Seek>(
    wb: &mut Xlsx<R>,
    igrf: &IgrfCells,
    home_numbers: &HashSet<String>,
    away_numbers: &HashSet<String>,
    home_jam_counts: &HashMap<String, JamCounts>,
    away_jam_counts: &HashMap<String, JamCounts>,
) -> Result<GameSummary> {
    let sheet = wb
        .worksheet_range("Game Summary")
        .context("no Game Summary sheet")?;
    let formulas = wb.worksheet_formula("Game Summary").ok();
    let home_players: Vec<SummaryPlayer> = (5u32..=24)
        .filter_map(|row| parse_summary_player(&sheet, &formulas, row, igrf, home_jam_counts))
        .filter(|p| home_numbers.contains(&p.number))
        .collect();
    let away_players: Vec<SummaryPlayer> = (27u32..=46)
        .filter_map(|row| parse_summary_player(&sheet, &formulas, row, igrf, away_jam_counts))
        .filter(|p| away_numbers.contains(&p.number))
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
    jam_counts: &HashMap<String, JamCounts>,
) -> Option<SummaryPlayer> {
    let number = cell_str_with_formula(sheet, formulas, row, 0, igrf)?;
    if is_zero_jam_player(&number) {
        return None;
    }
    let name = cell_str_with_formula(sheet, formulas, row, 1, igrf).unwrap_or_default();
    let jc = jam_counts.get(&number).copied().unwrap_or_default();
    let sheet_total = cell_opt_u8(sheet, row, 5);
    let use_computed = sheet_total.unwrap_or(0) == 0 && jc.total > 0;
    let summary = SummaryPlayer {
        number,
        name,
        jams_jammer: if use_computed {
            Some(jc.jammer)
        } else {
            cell_opt_u8(sheet, row, 2)
        },
        jams_pivot: if use_computed {
            Some(jc.pivot)
        } else {
            cell_opt_u8(sheet, row, 3)
        },
        jams_blocker: if use_computed {
            Some(jc.blocker)
        } else {
            cell_opt_u8(sheet, row, 4)
        },
        jams_total: if use_computed {
            Some(jc.total)
        } else {
            sheet_total
        },
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

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn test_parse_text_date_iso_and_slashes() {
        assert_eq!(
            parse_text_date("2026-06-27\t\t\t\t", None),
            Some(d(2026, 6, 27))
        );
        assert_eq!(parse_text_date("2026-06-27", None), Some(d(2026, 6, 27)));
        assert_eq!(parse_text_date("2025/02/08", None), Some(d(2025, 2, 8)));
    }

    #[test]
    fn test_parse_text_date_month_names() {
        // "Sept" is a common abbreviation chrono doesn't recognize.
        assert_eq!(parse_text_date("Sept 7 2024", None), Some(d(2024, 9, 7)));
        assert_eq!(
            parse_text_date("September 7 2024", None),
            Some(d(2024, 9, 7))
        );
        assert_eq!(parse_text_date("7 Sep 2024", None), Some(d(2024, 9, 7)));
        assert_eq!(
            parse_text_date("7 September 2024", None),
            Some(d(2024, 9, 7))
        );
        // Trailing punctuation must be tolerated.
        assert_eq!(parse_text_date("Sept. 7, 2024", None), Some(d(2024, 9, 7)));
    }

    #[test]
    fn test_parse_text_date_ambiguous_slash() {
        // 08/06/2024: Aug 6 (month-first) or Jun 8 (day-first).
        assert_eq!(parse_text_date("08/06/2024", None), Some(d(2024, 8, 6)));
        // The file-name date disambiguates: ISO file dates match the day-first
        // reading here.
        assert_eq!(
            parse_text_date("08/06/2024", Some(d(2024, 6, 8))),
            Some(d(2024, 6, 8))
        );
        // ...and the month-first reading when the file name agrees with it.
        assert_eq!(
            parse_text_date("08/06/2024", Some(d(2024, 8, 6))),
            Some(d(2024, 8, 6))
        );
    }

    #[test]
    fn test_parse_text_date_garbage() {
        assert_eq!(parse_text_date("not a date", None), None);
        assert_eq!(parse_text_date("", None), None);
        assert_eq!(parse_text_date("13/13/2024", None), None);
        // 2-digit years would parse as AD 2-99 and future years are impossible;
        // both must be rejected so the file-name date fallback can fire.
        assert_eq!(parse_text_date("08/06/24", None), None);
        assert_eq!(parse_text_date("Sept 7 24", None), None);
        assert_eq!(parse_text_date("08/06/2200", None), None);
    }

    #[test]
    fn test_date_from_file_name() {
        assert_eq!(
            date_from_file_name(
                "[WFTDA]STATS-2024-09-28_AuldReekieRollerDerby_AuldReekieRollerDerbyB_vs_LondonRollerDerby_BatterCPower"
            ),
            Some(d(2024, 9, 28))
        );
        assert_eq!(
            date_from_file_name(
                "[WFTDA]STATS-2026-06-27_ConnecticutRollerDerby_YankeeBrutals_vs_FreeStateRollerDerby_RockVillians"
            ),
            Some(d(2026, 6, 27))
        );
        // No marker, or a truncated/non-date string after the marker.
        assert_eq!(date_from_file_name("2024-09-28.xlsx"), None);
        assert_eq!(date_from_file_name("[WFTDA]STATS-24-09-28_...xlsx"), None);
        assert_eq!(date_from_file_name("[WFTDA]STATS"), None);
    }
}
