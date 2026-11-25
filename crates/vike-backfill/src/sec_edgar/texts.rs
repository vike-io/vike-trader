//! **A filing as text rows**: one [`vike_data::TextRow`] per standard section of its primary document
//! ([`super::sections`]), plus, for an 8-K or a 6-K, one per `EX-99` exhibit (the press release an
//! Item 2.02 points at is `EX-99.1`). A form outside `vike_data::sec_log::SEC_TEXT_KINDS` gives none.
//!
//! - `received_ms` = `published_ms` = the EDGAR acceptance time: EDGAR stamps it and nothing of the
//!   filing is public before it, so an archived filing is RECEIVED when it was accepted (the
//!   `crates/vike-data/src/text_log.rs` rule for a source that cannot back-date).
//! - `source_id` = `<accession>/<section>` (`0000320193-23-000106/item_1a`), the text's id;
//!   `amends` = the same section of the filing this `/A` corrects, when the caller found it.
//! - `subject` = the CIK, then the symbols the filing's inline XBRL cover tags (`dei:TradingSymbol`).
//! - `url` = the filing's index page on EDGAR; `tags` = the form exactly as EDGAR spells it.

use vike_data::TextRow;
use vike_data::sec_log::{SEC_AMENDMENT_SUFFIX, SEC_TEXT_SOURCE, sec_text_kind};

use super::html_text::html_to_text;
use super::sections::{WHOLE_DOCUMENT, split_sections};
use super::submission::{EdgarDocument, EdgarSubmission};

/// EDGAR's public web host: a filing's index page lives under its `Archives/edgar/data`.
pub const EDGAR_WWW: &str = "https://www.sec.gov";

/// The market bucket of every SEC text (`TextRow::market`).
const MARKET: &str = "stocks";

/// The base forms whose `EX-99` exhibits (a press release, a shareholder letter) are kept beside the
/// primary document.
const EXHIBIT_FORMS: [&str; 2] = ["8-K", "6-K"];

/// The inline XBRL element that tags a security's trading symbol on a cover page.
const TRADING_SYMBOL: &str = "name=\"dei:TradingSymbol\"";

/// The text rows of `filing`, none for a form the SEC text history does not keep. `amends` is the
/// accession of the filing an `/A` corrects (empty when unknown or not an amendment).
pub fn filing_text_rows(filing: &EdgarSubmission, amends: &str) -> Vec<TextRow> {
    let Some(kind) = sec_text_kind(&filing.form) else { return Vec::new() };
    let base = filing.form.strip_suffix(SEC_AMENDMENT_SUFFIX).unwrap_or(&filing.form);
    let Some(primary) =
        filing.documents.iter().find(|d| d.doc_type == filing.form).or(filing.documents.first())
    else {
        return Vec::new();
    };
    let mut subject = filing.company_cik.clone();
    for symbol in trading_symbols(&primary.text) {
        subject.push(' ');
        subject.push_str(&symbol);
    }
    let row = |key: &str, title: String, body: String| TextRow {
        published_ms: filing.accepted_ms,
        source_id: format!("{}/{key}", filing.accession),
        subject: subject.clone(),
        period: filing.period.clone(),
        title,
        body,
        url: index_url(&filing.company_cik, &filing.accession),
        market: MARKET.to_string(),
        tags: TextRow::joined_tags(&[filing.form.as_str()]),
        amends: if amends.is_empty() { String::new() } else { format!("{amends}/{key}") },
        ..TextRow::new(kind, SEC_TEXT_SOURCE, filing.accepted_ms)
    };
    let mut rows: Vec<TextRow> = split_sections(base, &html_to_text(&primary.text))
        .into_iter()
        .filter(|s| !s.text.is_empty())
        .map(|s| {
            let title = match s.key.as_str() {
                WHOLE_DOCUMENT => format!("{} {}", filing.company_name, filing.form),
                _ => s.title,
            };
            row(&s.key, title, s.text)
        })
        .collect();
    if EXHIBIT_FORMS.contains(&base) {
        for exhibit in filing.documents.iter().filter(|d| d.doc_type.starts_with("EX-99")) {
            let text = html_to_text(&exhibit.text);
            if !text.is_empty() {
                rows.push(row(&exhibit_key(exhibit), exhibit_title(exhibit), text));
            }
        }
    }
    rows
}

/// The section key of an exhibit: its type in lower case, `-` and `.` as `_` (`ex_99_1`).
fn exhibit_key(exhibit: &EdgarDocument) -> String {
    exhibit.doc_type.to_ascii_lowercase().replace(['-', '.'], "_")
}

/// An exhibit's title: its type, then the filer's description of it when that says more (Apple's
/// press release describes itself as `EX-99.1`, measured live).
fn exhibit_title(exhibit: &EdgarDocument) -> String {
    match exhibit.description.as_str() {
        "" => exhibit.doc_type.clone(),
        same if same == exhibit.doc_type => exhibit.doc_type.clone(),
        description => format!("{} {description}", exhibit.doc_type),
    }
}

/// The filing's index page: `https://www.sec.gov/Archives/edgar/data/<CIK, no leading zeros>/<accession,
/// no dashes>/<accession>-index.htm`.
pub fn index_url(cik: &str, accession: &str) -> String {
    let cik = cik.trim_start_matches('0');
    let folder = accession.replace('-', "");
    format!("{EDGAR_WWW}/Archives/edgar/data/{cik}/{folder}/{accession}-index.htm")
}

/// The distinct trading symbols the document's inline XBRL tags, in order.
fn trading_symbols(html: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (at, _) in html.match_indices(TRADING_SYMBOL) {
        let symbol = html[at..]
            .split_once('>')
            .and_then(|(_, rest)| rest.split_once('<'))
            .map(|(value, _)| value.trim().to_string())
            .unwrap_or_default();
        let plain = !symbol.is_empty()
            && symbol.len() <= 12
            && symbol.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
        if plain && !out.contains(&symbol) {
            out.push(symbol);
        }
    }
    out
}

#[cfg(test)]
#[path = "texts_tests.rs"]
mod texts_tests;
