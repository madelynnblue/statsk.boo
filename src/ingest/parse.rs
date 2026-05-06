use crate::models::*;
use anyhow::{Context, Result};
use calamine::{Data, DataType, Range, Reader, Xlsx, open_workbook_from_rs};
use std::io::Cursor;

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
    let home = parse_team(&mut wb, 1, 2, 13, 20)?;
    let away = parse_team(&mut wb, 8, 9, 13, 20)?;
    let mut periods = parse_scores(&mut wb)?;
    merge_lineups(&mut wb, &mut periods)?;
    let penalties = parse_penalties(&mut wb)?;

    let game = GameData {
        version,
        venue,
        tournament,
        host_league,
        home,
        away,
        periods,
        penalties,
    };
    Ok((game, date))
}

fn cell_str(r: &Range<Data>, row: u32, col: u32) -> Option<String> {
    let data = r.get_value((row, col))?;
    match data {
        Data::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        }
        Data::Int(v) => {
            eprintln!("DEBUG cell_str Int at ({}, {}): {}", row, col, v);
            Some(v.to_string())
        }
        Data::Float(v) => {
            eprintln!("DEBUG cell_str Float at ({}, {}): {}", row, col, v);
            Some(float_to_int_str(*v))
        }
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
    if let Ok(sheet) = wb.worksheet_range("Read Me") {
        if let Some(v) = cell_str(&sheet, 2, 0) {
            if let Some(m) = v
                .split_whitespace()
                .find(|s| s.len() == 4 && s.chars().all(|c| c.is_ascii_digit()))
            {
                return m.to_string();
            }
            return v;
        }
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
        let number = match cell_str(&sheet, row, num_col) {
            Some(n) => n,
            None => break,
        };
        let name = cell_str(&sheet, row, name_col).unwrap_or_default();
        if !number.is_empty() {
            skaters.push(Skater { number, name });
        }
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
        for i in 0..38u32 {
            let row = start_row + i;
            let jam_num = match cell_i16(&sheet, row, home_col) {
                0 => break,
                n => n as u16,
            };
            let home = parse_jam_side(&sheet, row, home_col);
            let away = parse_jam_side(&sheet, row, away_col);
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
    periods: &mut Vec<Period>,
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
        for (ji, jam) in period.jams.iter_mut().enumerate() {
            let row = start_row + ji as u32;
            jam.home.lineup = parse_lineup_side(&sheet, row, home_np_col);
            jam.away.lineup = parse_lineup_side(&sheet, row, away_np_col);
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

fn parse_penalties<R: std::io::Read + std::io::Seek>(wb: &mut Xlsx<R>) -> Result<Vec<Penalty>> {
    let sheet = match wb.worksheet_range("Penalties") {
        Ok(s) => s,
        Err(_) => return Ok(vec![]),
    };
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
            let skater_num = match cell_str(&sheet, code_row, skater_col) {
                Some(n) => n,
                None => break,
            };
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
