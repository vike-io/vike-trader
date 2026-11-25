//! **EDGAR's complete submission text** (`<accession>.txt` in a filing's folder, the EDGAR Public
//! Dissemination Service format): one `<SEC-DOCUMENT>` holding the `<SEC-HEADER>` and every document of
//! the filing, each `<DOCUMENT>` with its `<TYPE>`, `<SEQUENCE>`, `<FILENAME>`, `<DESCRIPTION>` and
//! `<TEXT>`. A `sec-filings` day file is the day's submissions one after another ([`split_day_file`]).
//!
//! The header is `KEY:<tabs>VALUE` lines; a line at the left margin that ends in `:` opens a section
//! (`FILER:`, `SUBJECT COMPANY:`, `FILED BY:`), and the company data below it is indented. What is
//! read: the accession number, the form (`CONFORMED SUBMISSION TYPE`), the acceptance time
//! (`<ACCEPTANCE-DATETIME>`, New York wall clock, [`super::new_york_to_utc_ms`]), the period of report,
//! and the CIK and name of the company the filing is ABOUT: the `SUBJECT COMPANY` of a schedule 13D or
//! 13G, the first `FILER` otherwise. A header without an accession number, a form or an acceptance time
//! is refused, never guessed.

/// One filing, as its complete submission text states it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgarSubmission {
    /// `0000320193-23-000106`.
    pub accession: String,
    /// The form type as EDGAR spells it (`10-K`, `8-K/A`, `SC 13G`).
    pub form: String,
    /// The EDGAR acceptance time, epoch milliseconds UTC.
    pub accepted_ms: i64,
    /// The period of report, `YYYY-MM-DD`; empty when the header states none.
    pub period: String,
    /// The CIK of the company the filing is about, ten digits.
    pub company_cik: String,
    /// That company's conformed name.
    pub company_name: String,
    /// Every document, in the filing's order.
    pub documents: Vec<EdgarDocument>,
}

/// One document of a filing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgarDocument {
    /// `10-K`, `EX-99.1`, `GRAPHIC`.
    pub doc_type: String,
    /// The file name inside the filing (`aapl-20230930.htm`).
    pub filename: String,
    /// The filer's description; empty when none.
    pub description: String,
    /// What `<TEXT>` holds, verbatim (HTML, inline XBRL or plain text).
    pub text: String,
}

const DOCUMENT_OPEN: &str = "<SEC-DOCUMENT>";
const DOCUMENT_CLOSE: &str = "</SEC-DOCUMENT>";

/// The complete submission texts of a day file, in order. Text outside them (blank lines between two)
/// is allowed; an unclosed one, or a file with none, is refused.
pub fn split_day_file(file: &str) -> Result<Vec<&str>, String> {
    let mut out = Vec::new();
    let mut rest = file;
    while let Some(open) = rest.find(DOCUMENT_OPEN) {
        let Some(close) = rest[open..].find(DOCUMENT_CLOSE) else {
            return Err(format!(
                "submission {} opens with {DOCUMENT_OPEN} and never closes: the file is cut short",
                out.len() + 1
            ));
        };
        let end = open + close + DOCUMENT_CLOSE.len();
        out.push(&rest[open..end]);
        rest = &rest[end..];
    }
    if out.is_empty() {
        return Err(format!(
            "the file holds no EDGAR complete submission text (none opens with {DOCUMENT_OPEN})"
        ));
    }
    Ok(out)
}

/// Read one complete submission text.
pub fn parse_submission(text: &str) -> Result<EdgarSubmission, String> {
    let header = between(text, "<SEC-HEADER>", "</SEC-HEADER>")
        .ok_or("the submission has no <SEC-HEADER> … </SEC-HEADER>")?;
    let mut accession = String::new();
    let mut form = String::new();
    let mut accepted = String::new();
    let mut period = String::new();
    let mut section = String::new();
    // (CIK, conformed name) of the first FILER / FILED BY, and of the SUBJECT COMPANY.
    let mut filer: Option<(String, String)> = None;
    let mut subject: Option<(String, String)> = None;
    let mut name = String::new();
    for line in header.lines() {
        if let Some(value) = line.trim().strip_prefix("<ACCEPTANCE-DATETIME>") {
            accepted = value.trim().to_string();
            continue;
        }
        if !line.starts_with([' ', '\t']) && line.trim_end().ends_with(':') {
            section = line.trim_end().trim_end_matches(':').trim().to_string();
            continue;
        }
        let Some((key, value)) = line.split_once(':') else { continue };
        let (key, value) = (key.trim(), value.trim());
        match key {
            "ACCESSION NUMBER" => accession = value.to_string(),
            "CONFORMED SUBMISSION TYPE" => form = value.to_string(),
            "CONFORMED PERIOD OF REPORT" => period = value.to_string(),
            "COMPANY CONFORMED NAME" => name = value.to_string(),
            "CENTRAL INDEX KEY" => {
                let company = (value.to_string(), std::mem::take(&mut name));
                match section.as_str() {
                    "SUBJECT COMPANY" if subject.is_none() => subject = Some(company),
                    "FILER" | "FILED BY" if filer.is_none() => filer = Some(company),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    if !super::is_accession(&accession) {
        return Err(format!("the header's ACCESSION NUMBER {accession:?} is not one"));
    }
    if form.is_empty() {
        return Err(format!("{accession}: the header states no CONFORMED SUBMISSION TYPE"));
    }
    let accepted_ms = acceptance_ms(&accepted).ok_or_else(|| {
        format!("{accession}: the header's ACCEPTANCE-DATETIME {accepted:?} is not a time")
    })?;
    let (company_cik, company_name) = subject.or(filer).ok_or_else(|| {
        format!("{accession}: the header names no company (no CENTRAL INDEX KEY)")
    })?;
    let period = match period.as_str() {
        "" => String::new(),
        stated => super::parse_date(stated)
            .map(super::date_label)
            .ok_or_else(|| format!("{accession}: the period of report {stated:?} is not a date"))?,
    };
    Ok(EdgarSubmission {
        accession,
        form,
        accepted_ms,
        period,
        company_cik,
        company_name,
        documents: documents(text),
    })
}

/// `YYYYMMDDHHMMSS`, New York wall clock, as epoch milliseconds UTC.
fn acceptance_ms(stamp: &str) -> Option<i64> {
    if stamp.len() != 14 || !stamp.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n = |range: std::ops::Range<usize>| stamp[range].parse::<u32>().ok();
    super::new_york_to_utc_ms(
        i64::from(n(0..4)?),
        n(4..6)?,
        n(6..8)?,
        n(8..10)?,
        n(10..12)?,
        n(12..14)?,
    )
}

/// Every `<DOCUMENT>` of a submission.
fn documents(text: &str) -> Vec<EdgarDocument> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find("<DOCUMENT>") {
        let body = &rest[open + "<DOCUMENT>".len()..];
        let end = body.find("</DOCUMENT>").unwrap_or(body.len());
        let document = &body[..end];
        let field = |tag: &str| {
            document
                .lines()
                .find_map(|line| line.trim().strip_prefix(tag).map(|v| v.trim().to_string()))
                .unwrap_or_default()
        };
        out.push(EdgarDocument {
            doc_type: field("<TYPE>"),
            filename: field("<FILENAME>"),
            description: field("<DESCRIPTION>"),
            text: between(document, "<TEXT>", "</TEXT>").unwrap_or_default().to_string(),
        });
        rest = &body[end..];
    }
    out
}

/// The text between the first `open` and the `close` after it.
fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = text.find(open)? + open.len();
    let end = text[start..].find(close)? + start;
    Some(&text[start..end])
}

#[cfg(test)]
#[path = "submission_tests.rs"]
mod submission_tests;
