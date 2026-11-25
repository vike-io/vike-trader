//! SEC EDGAR history rows: the vocabulary of the filing TEXTS (`kind=text`, [`crate::TextRow`]) and the
//! two TABLE kinds, each its own series kind, never text:
//!
//! - `kind=institutional_holding` ([`InstitutionalHoldingRow`]): one line of a Form 13F information table
//!   (a manager's position in one security at a quarter's end), from the SEC's quarterly 13F data sets.
//!   Keyed `venue=sec`/`symbol=<the filer's CIK>`: a 13F report is one manager's, so "what did this
//!   manager hold as of a time" is one small series read.
//! - `kind=financial_fact` ([`FinancialFactRow`]): one reported XBRL value (a company, a concept, a
//!   period, a value) from the SEC's `companyfacts` API. Keyed `venue=sec`/`symbol=<the company's CIK>`.
//!
//! # The two clocks, and the one a study reads
//!
//! A row's store `ts` is the instant it became KNOWABLE: what a point-in-time read selects on
//! ([`crate::HistoryStore::institutional_holdings_as_of`], [`crate::HistoryStore::financial_facts_as_of`]).
//! Both sources state only the filing DATE, never the acceptance time, so the knowable instant is the
//! LATEST moment EDGAR accepts a filing that carries that date: 22:00 New York time on it (EDGAR's
//! window closes at 22:00 Eastern, and a filing accepted after its 17:30 cut-off is DATED the next
//! business day, never the same one). So a row is never seen early; it may be seen a few hours, or a
//! weekend, late. A filing TEXT carries its exact acceptance time ([`crate::TextRow::published_ms`]),
//! and a study that needs a fact the moment it was accepted joins the two on the accession number.
//!
//! # Amendments are rows, never edits
//!
//! A 13F amendment (`13F-HR/A`) is its own rows with its [`InstitutionalHoldingRow::amendment_type`]:
//! `RESTATEMENT` replaces the manager's report for that period, `NEW HOLDINGS` adds to it. An XBRL value
//! restated by a later filing is a later row with the same concept and period. The last row known at a
//! time is the value known then.
//!
//! Pure data, no I/O. The Parquet codecs are `vike-store`'s, beside the other kinds'; the import that
//! writes them is `crates/vike-backfill/src/sec_edgar/store.rs`.

use serde::{Deserialize, Serialize};

/// The `venue` of both SEC table kinds.
pub const SEC_VENUE: &str = "sec";

/// The source word of every SEC text row ([`crate::TextRow::source`]).
pub const SEC_TEXT_SOURCE: &str = "EDGAR";

/// **The SEC text kinds**: a filing's base form type (its `/A` amendment shares the kind) and the text
/// kind word its rows land under, the series' `venue`. The vocabulary is [`crate::TextRow`]'s open
/// one; a new form is a new row here, never a schema change. Schedules 13D and 13G kept their
/// `SC 13D`/`SC 13G` form names until EDGAR renamed them `SCHEDULE 13D`/`SCHEDULE 13G` (December
/// 2024); both spellings land in one kind.
pub const SEC_TEXT_KINDS: [(&str, &str); 12] = [
    ("10-K", "sec_10k"),
    ("10-Q", "sec_10q"),
    ("8-K", "sec_8k"),
    ("20-F", "sec_20f"),
    ("6-K", "sec_6k"),
    ("S-1", "sec_s1"),
    ("F-1", "sec_f1"),
    ("DEF 14A", "sec_def14a"),
    ("SC 13D", "sec_13d"),
    ("SC 13G", "sec_13g"),
    ("SCHEDULE 13D", "sec_13d"),
    ("SCHEDULE 13G", "sec_13g"),
];

/// The amendment suffix of an EDGAR form type (`10-K/A`).
pub const SEC_AMENDMENT_SUFFIX: &str = "/A";

/// The text kind of the EDGAR form type `form` (`10-K`, `10-K/A`, `SC 13G/A`), or `None` for a form
/// the SEC text history does not keep. Matched exactly, upper case, as EDGAR spells it.
pub fn sec_text_kind(form: &str) -> Option<&'static str> {
    let base = form.strip_suffix(SEC_AMENDMENT_SUFFIX).unwrap_or(form);
    SEC_TEXT_KINDS.iter().find(|(f, _)| *f == base).map(|(_, kind)| *kind)
}

/// Every distinct SEC text kind word, in [`SEC_TEXT_KINDS`] order.
pub fn sec_text_kind_words() -> Vec<&'static str> {
    let mut words: Vec<&'static str> = Vec::new();
    for (_, kind) in SEC_TEXT_KINDS {
        if !words.contains(&kind) {
            words.push(kind);
        }
    }
    words
}

/// One line of a Form 13F information table, persisted as the `kind=institutional_holding` series.
/// Every string is EMPTY where the data set gives nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstitutionalHoldingRow {
    /// When the line became knowable, epoch milliseconds UTC: 22:00 New York time on the filing's
    /// date (the module doc). The store's `ts`.
    pub filed_ms: i64,
    /// The filing's accession number (`0001067983-24-000006`).
    pub accession: String,
    /// The filing manager's CIK, ten digits: the series' `symbol` too.
    pub filer_cik: String,
    /// The filing manager's name, as its cover page states it.
    pub filer_name: String,
    /// The quarter's end the report is as of, `YYYY-MM-DD`.
    pub period: String,
    /// The submission type: `13F-HR` or `13F-HR/A`.
    pub submission_type: String,
    /// An amendment's kind: `RESTATEMENT` (replaces the period's report) or `NEW HOLDINGS` (adds to
    /// it); empty for an original report.
    pub amendment_type: String,
    /// The issuer's name, as the manager wrote it.
    pub issuer: String,
    /// The title of the class of the security (`COM`, `CL A`).
    pub class_title: String,
    /// The security's CUSIP, nine characters.
    pub cusip: String,
    /// The position's market value in US dollars. Reports filed before 2023-01-03 stated thousands,
    /// and are multiplied out here, so every row is whole dollars.
    pub value_usd: i64,
    /// Shares, or the principal amount for a debt security ([`Self::shares_type`]).
    pub shares: i64,
    /// `SH` (shares) or `PRN` (principal amount).
    pub shares_type: String,
    /// `Put` or `Call` for an option position (the shares are the underlying's), empty otherwise.
    pub put_call: String,
}

/// One reported XBRL value, persisted as the `kind=financial_fact` series. Every string is EMPTY where
/// the source gives nothing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FinancialFactRow {
    /// When the value became knowable, epoch milliseconds UTC: 22:00 New York time on its filing's
    /// date (the module doc). The store's `ts`.
    pub filed_ms: i64,
    /// The reporting company's CIK, ten digits: the series' `symbol` too.
    pub cik: String,
    /// The company's name, as the source states it.
    pub entity: String,
    /// The taxonomy (`us-gaap`, `ifrs-full`, `dei`).
    pub taxonomy: String,
    /// The concept (`Revenues`, `AccountsPayableCurrent`).
    pub concept: String,
    /// The unit (`USD`, `shares`, `USD/shares`).
    pub unit: String,
    /// The first day of a duration, `YYYY-MM-DD`; empty for an instant.
    pub period_start: String,
    /// The last day of a duration, or the instant, `YYYY-MM-DD`.
    pub period_end: String,
    /// The value as reported.
    pub value: f64,
    /// The accession number of the filing that reported it.
    pub accession: String,
    /// The fiscal year the filing reports, as the source spells it (`2023`).
    pub fiscal_year: String,
    /// The fiscal period the filing reports (`FY`, `Q1`).
    pub fiscal_period: String,
    /// The form of the filing that reported it (`10-K`, `10-Q/A`).
    pub form: String,
    /// The calendar frame the SEC aligns the value to (`CY2023Q3I`), empty when it aligns none.
    pub frame: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_form_and_its_amendment_share_a_kind_and_both_13g_spellings_meet() {
        assert_eq!(sec_text_kind("10-K"), Some("sec_10k"));
        assert_eq!(sec_text_kind("10-K/A"), Some("sec_10k"));
        assert_eq!(sec_text_kind("SC 13G/A"), Some("sec_13g"));
        assert_eq!(sec_text_kind("SCHEDULE 13G"), Some("sec_13g"));
        assert_eq!(sec_text_kind("DEF 14A"), Some("sec_def14a"));
        assert_eq!(sec_text_kind("4"), None, "a form the text history does not keep");
        assert_eq!(sec_text_kind("10-k"), None, "EDGAR's spelling, exactly");
        assert_eq!(sec_text_kind_words().len(), 10, "two forms share each schedule's kind");
    }
}
