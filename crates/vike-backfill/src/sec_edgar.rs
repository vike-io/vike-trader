//! **SEC EDGAR into the history store**: the decoders and the store half of the three SEC archive
//! formats the datahub's import lane reads (`vike-cli data history import`), and the grammar of the
//! files the datahub's SEC download writes. Pure parsing plus [`store`]'s appends; no network here
//! (the download is `crates/vike-datahub/src/import/sec_download.rs`).
//!
//! | format (`vike_data::archive_day`'s `SEC_*_FORMAT`) | a dataset is | its files | lands in |
//! |---|---|---|---|
//! | `sec-filings` | a company's CIK | one file per UTC day of EDGAR acceptance, `YYYY-MM-DD.txt`: the day's EDGAR complete submission texts, one after another ([`submission`]) | `kind=text`, `venue=<sec kind>`, `symbol=<CIK>`, one row per standard section ([`sections`]) |
//! | `sec-companyfacts` | a company's CIK | the SEC's `companyfacts` JSON, `CIK##########.json` ([`companyfacts`]) | `kind=financial_fact`, `venue=sec`, `symbol=<CIK>` |
//! | `sec-form13f` | one quarterly data set (`2024Q1`) | the SEC's Form 13F data-set zip ([`form13f`]) | `kind=institutional_holding`, `venue=sec`, `symbol=<manager's CIK>` |
//!
//! # Time: when a row became knowable, in UTC
//!
//! - **A filing text** is knowable at its EDGAR ACCEPTANCE time, which the submission header states
//!   in New York wall-clock time (`<ACCEPTANCE-DATETIME>20231102180827`); [`new_york_to_utc_ms`]
//!   converts it. ⚠ The submissions JSON states the same instant in UTC (`2023-11-02T22:08:27.000Z`),
//!   and [`submissions_index`] reads it as UTC: two clocks for one instant, measured against SEC
//!   (that module's doc). Never the filing DATE, which a filing accepted after 17:30 takes from the NEXT
//!   business day.
//! - **A table row** (an XBRL value, a 13F line) states only its filing DATE, so it is knowable at the
//!   end of that date's EDGAR day ([`end_of_filing_day_ms`]): 03:00 UTC the next day, which is 22:00
//!   New York standard time and 23:00 daylight time, never before EDGAR's 22:00 close under either
//!   clock. Seen a few hours, or a weekend, late; never early.
//!
//! New York's daylight rule is `vike_venues::session::new_york_utc_offset_min`, the US rule since 2007.
//! Before 2007 the rule was another one, and no second table is kept: an earlier wall-clock time is
//! read as standard time, which is the LATER of the two readings, so it may be an hour late in summer
//! but never early (a point-in-time read must never see a filing before it was accepted).
//!
//! # A company's identity
//!
//! A CIK is ten digits, zero-padded, exactly as EDGAR's paths and JSON names spell it
//! ([`cik_of_dataset`]). A text row's `subject` is the CIK, then, where the filing itself states them,
//! the trading symbols its inline XBRL cover page tags (`dei:TradingSymbol`): the symbol AT the filing,
//! never today's.

use vike_model::MS_PER_DAY;
use vike_utility::calendar::{days_from_civil, is_real_day};

pub mod companyfacts;
pub mod form13f;
pub mod html_text;
pub mod sections;
pub mod store;
pub mod submission;
pub mod submissions_index;
pub mod texts;
pub mod zip_members;

/// The first year New York's daylight rule is the one `vike_venues::session` knows.
const NEW_YORK_RULE_SINCE: i64 = 2007;

/// New York's standard-time offset behind UTC, in milliseconds (UTC-5).
const NEW_YORK_STANDARD_OFFSET_MS: i64 = 5 * 3_600_000;

/// **A New York wall-clock time as epoch milliseconds UTC**, or `None` for a time that is not a real
/// calendar instant. From 2007 on, by the US daylight rule; before, as standard time (the module
/// doc: never early). EDGAR accepts filings from 06:00 to 22:00, far from the 02:00 changeovers, so the
/// hour a changeover skips or repeats never meets a filing.
pub fn new_york_to_utc_ms(
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> Option<i64> {
    if !is_real_day(year, month, day) || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let wall = days_from_civil(year, month, day) * MS_PER_DAY
        + i64::from(hour * 3600 + minute * 60 + second) * 1000;
    let as_standard = wall + NEW_YORK_STANDARD_OFFSET_MS;
    if year < NEW_YORK_RULE_SINCE {
        return Some(as_standard);
    }
    // The offset in force at the standard-time reading: equal to the true one except inside the
    // changeover hours, which EDGAR's opening hours never meet.
    Some(wall + vike_venues::session::new_york_utc_offset_min(as_standard) * 60_000)
}

/// **When a row that states only its filing DATE became knowable**: 03:00 UTC on the day after
/// `(year, month, day)`, the end of that date's EDGAR day under either New York clock (the module
/// doc). `None` for a date that is not a real calendar day.
pub fn end_of_filing_day_ms(year: i64, month: u32, day: u32) -> Option<i64> {
    is_real_day(year, month, day).then(|| {
        days_from_civil(year, month, day) * MS_PER_DAY
            + EDGAR_CLOSE_MS
            + NEW_YORK_STANDARD_OFFSET_MS
    })
}

/// EDGAR's daily close, 22:00, as milliseconds into a New York day.
const EDGAR_CLOSE_MS: i64 = 22 * 3_600_000;

/// `YYYY-MM-DD` or `YYYYMMDD` as `(year, month, day)`, only for a real calendar day.
pub fn parse_date(text: &str) -> Option<(i64, u32, u32)> {
    let digits: String = text.trim().chars().filter(|c| *c != '-').collect();
    if digits.len() != 8 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (year, month, day) =
        (digits[..4].parse().ok()?, digits[4..6].parse().ok()?, digits[6..].parse().ok()?);
    is_real_day(year, month, day).then_some((year, month, day))
}

/// `(year, month, day)` as `YYYY-MM-DD`.
pub fn date_label((year, month, day): (i64, u32, u32)) -> String {
    format!("{year:04}-{month:02}-{day:02}")
}

/// A dataset that names a company: its CIK, ten ASCII digits as EDGAR spells it
/// (`0000320193`), or the whole-request refusal.
pub fn cik_of_dataset(source: &str, dataset: &str) -> Result<String, String> {
    if dataset.len() == 10 && dataset.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(dataset.to_string());
    }
    Err(format!(
        "{source} reads one company's folder, named by its CIK as EDGAR spells it: ten digits, \
         zero-padded (`0000320193`). {dataset:?} is not one"
    ))
}

/// An EDGAR accession number, `0000320193-23-000106`: ten, two and six digits.
pub fn is_accession(text: &str) -> bool {
    let parts: Vec<&str> = text.split('-').collect();
    parts.len() == 3
        && [10, 2, 6].iter().zip(&parts).all(|(n, p)| p.len() == *n)
        && parts.iter().all(|p| p.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
#[path = "sec_edgar/sec_edgar_tests.rs"]
mod sec_edgar_tests;
