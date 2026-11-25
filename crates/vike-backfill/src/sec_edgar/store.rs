//! **The SEC import's store half**: every append the three SEC formats make, under its day key, and the
//! reads that classify a day before anything is decoded.
//!
//! One commit per series and UTC day, its key naming both, so the store's commit-key idempotency makes a
//! repeated import write nothing twice, and a day cut short between two of its series (a filing day
//! holding a 10-Q and an 8-K is two series) is finished by importing it again: the series that hold the
//! key skip it, the others take it.
//!
//! - texts: `kind=text/venue=<sec kind>/symbol=<CIK>`, key `sec-filings:{kind}:{cik}:{day}`;
//! - XBRL values: `kind=financial_fact/venue=sec/symbol=<CIK>`, key `sec-companyfacts:{cik}:{day}`;
//! - 13F lines: `kind=institutional_holding/venue=sec/symbol=<manager's CIK>`, key
//!   `sec-form13f:{filer}:{day}`.
//!
//! `{day}` is the UTC day the rows are stored on, `YYYY-MM-DD`. No other producer writes these series,
//! so a day another key holds is not a question here.

use std::collections::{BTreeMap, BTreeSet};

use vike_data::archive_day::{SEC_COMPANY_FACTS_FORMAT, SEC_FILINGS_FORMAT, SEC_FORM_13F_FORMAT};
use vike_data::sec_log::{SEC_AMENDMENT_SUFFIX, SEC_VENUE, sec_text_kind_words};
use vike_data::{
    FinancialFactRow, HistoryStore, HistoryStoreError, InstitutionalHoldingRow, SeriesId, TextRow,
    TimestampRange,
};
use vike_model::MS_PER_DAY;
use vike_model::StoreKindId;
use vike_model::time::epoch_ms_to_utc_date;

/// How far back an amendment's original is looked for: a `/A` corrects a filing of the last few years.
const AMENDS_LOOKBACK_MS: i64 = 3 * 366 * MS_PER_DAY;

/// The key of company `cik`'s `kind` texts on the UTC day holding `day_ms`.
pub fn filing_day_key(kind: &str, cik: &str, day_ms: i64) -> String {
    let day = epoch_ms_to_utc_date(day_ms);
    format!("sec-filings:{kind}:{cik}:{day}")
}

/// The key of company `cik`'s XBRL values on the UTC day holding `day_ms`.
pub fn companyfacts_day_key(cik: &str, day_ms: i64) -> String {
    let day = epoch_ms_to_utc_date(day_ms);
    format!("sec-companyfacts:{cik}:{day}")
}

/// The key of manager `filer`'s 13F lines on the UTC day holding `day_ms`.
pub fn form13f_day_key(filer: &str, day_ms: i64) -> String {
    let day = epoch_ms_to_utc_date(day_ms);
    format!("sec-form13f:{filer}:{day}")
}

/// Store company `cik`'s text rows of the day `day_ms`, one commit per text kind; the rows written.
pub fn append_filing_day<H: HistoryStore + ?Sized>(
    hist: &H,
    cik: &str,
    day_ms: i64,
    rows: &[TextRow],
) -> Result<usize, HistoryStoreError> {
    let mut by_kind: BTreeMap<&str, Vec<TextRow>> = BTreeMap::new();
    for row in rows {
        by_kind.entry(row.kind.as_str()).or_default().push(row.clone());
    }
    let mut written = 0;
    for (kind, rows) in by_kind {
        written += hist.append_texts(kind, cik, &rows, Some(&filing_day_key(kind, cik, day_ms)))?;
    }
    Ok(written)
}

/// Store company `cik`'s XBRL values of the day `day_ms`; the rows written.
pub fn append_facts_day<H: HistoryStore + ?Sized>(
    hist: &H,
    cik: &str,
    day_ms: i64,
    rows: &[FinancialFactRow],
) -> Result<usize, HistoryStoreError> {
    hist.append_financial_facts(SEC_VENUE, cik, rows, Some(&companyfacts_day_key(cik, day_ms)))
}

/// Store the day `day_ms`'s 13F lines, one commit per manager; the rows written.
pub fn append_holdings_day<H: HistoryStore + ?Sized>(
    hist: &H,
    day_ms: i64,
    rows: &[InstitutionalHoldingRow],
) -> Result<usize, HistoryStoreError> {
    let mut by_filer: BTreeMap<&str, Vec<InstitutionalHoldingRow>> = BTreeMap::new();
    for row in rows {
        by_filer.entry(row.filer_cik.as_str()).or_default().push(row.clone());
    }
    let mut written = 0;
    for (filer, rows) in by_filer {
        let key = form13f_day_key(filer, day_ms);
        written += hist.append_institutional_holdings(SEC_VENUE, filer, &rows, Some(&key))?;
    }
    Ok(written)
}

/// The UTC days (each `YYYY-MM-DD`) company `cik`'s texts already hold, in any SEC text kind.
pub fn held_filing_days<H: HistoryStore + ?Sized>(
    hist: &H,
    cik: &str,
) -> Result<BTreeSet<String>, HistoryStoreError> {
    let mut days = BTreeSet::new();
    for kind in sec_text_kind_words() {
        let id = SeriesId::per_symbol(StoreKindId::Text, kind, cik, None);
        days.extend(days_of(
            &hist.series_commits(&id)?,
            &format!("{SEC_FILINGS_FORMAT}:{kind}:{cik}:"),
        ));
    }
    Ok(days)
}

/// The UTC days company `cik`'s XBRL values already hold.
pub fn held_facts_days<H: HistoryStore + ?Sized>(
    hist: &H,
    cik: &str,
) -> Result<BTreeSet<String>, HistoryStoreError> {
    let id = SeriesId::per_symbol(StoreKindId::FinancialFact, SEC_VENUE, cik, None);
    Ok(days_of(&hist.series_commits(&id)?, &format!("{SEC_COMPANY_FACTS_FORMAT}:{cik}:")).collect())
}

/// The UTC days manager `filer`'s 13F lines already hold.
pub fn held_holdings_days<H: HistoryStore + ?Sized>(
    hist: &H,
    filer: &str,
) -> Result<BTreeSet<String>, HistoryStoreError> {
    let id = SeriesId::per_symbol(StoreKindId::InstitutionalHolding, SEC_VENUE, filer, None);
    Ok(days_of(&hist.series_commits(&id)?, &format!("{SEC_FORM_13F_FORMAT}:{filer}:")).collect())
}

/// The day labels of the keys that open with `head`.
fn days_of<'k>(keys: &'k [String], head: &'k str) -> impl Iterator<Item = String> + 'k {
    keys.iter().filter_map(move |k| k.strip_prefix(head)).map(str::to_string)
}

/// **The filing an amendment corrects**: the accession of company `cik`'s latest stored filing of
/// `kind` with the same `period`, accepted before `accepted_ms` and not itself an amendment, within the
/// last three years; `None` when it is not stored (imported later, or never). Import a company's
/// days in time order, as the CLI does, and an original is always stored before its `/A`.
pub fn original_of<H: HistoryStore + ?Sized>(
    hist: &H,
    kind: &str,
    cik: &str,
    period: &str,
    accepted_ms: i64,
) -> Result<Option<String>, HistoryStoreError> {
    if period.is_empty() {
        return Ok(None);
    }
    let range = TimestampRange::of(accepted_ms - AMENDS_LOOKBACK_MS, accepted_ms - 1);
    let rows = hist.scan_texts(kind, cik, range)?;
    Ok(rows
        .iter()
        .rev()
        .filter(|r| r.period == period && !r.tags.ends_with(SEC_AMENDMENT_SUFFIX))
        .find_map(|r| r.source_id.split_once('/').map(|(accession, _)| accession.to_string())))
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod store_tests;
