//! **The SEC's submissions JSON** (`https://data.sec.gov/submissions/CIK##########.json`): a
//! company's filings, newest first, as parallel column arrays under `filings.recent`, and the pages
//! holding its older filings under `filings.files` (each page, `CIK##########-submissions-001.json`,
//! carries the same column arrays at its top level). What the SEC download reads to know which
//! filings to fetch and on which UTC day each was accepted.
//!
//! ⚠ **Two clocks for one instant.** `acceptanceDateTime` is UTC, as its `Z` says
//! (`2023-11-02T22:08:27.000Z`), while the filing's own header states the same instant in New York time
//! (`<ACCEPTANCE-DATETIME>20231102180827`, `super::submission`). MEASURED against SEC on 2026-11-16
//! (`crates/vike-datahub/tests/sec_edgar_live.rs`): Apple's 8-K `0000320193-24-000120`, accepted after the
//! close on 2024-10-31, is 20:30:25 UTC (`1730406625000`) by both clocks read this way, and its 10-K
//! `0000320193-24-000123` 10:01:36 UTC on 2024-11-01; reading the index as New York time had put the 8-K
//! on the next UTC day, which the import refused (`WrongDay`). So the index is read as UTC and the header
//! as New York time, and the two agree.

use serde_json::Value;

/// One filing of the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedFiling {
    /// `0000320193-23-000106`.
    pub accession: String,
    /// The form type as EDGAR spells it.
    pub form: String,
    /// The EDGAR acceptance time, epoch milliseconds UTC.
    pub accepted_ms: i64,
    /// The complete submission's size in bytes, as the index states it (0 when it states none).
    pub bytes: u64,
}

/// One page of older filings the index names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OlderPage {
    /// Its file name (`CIK0000320193-submissions-001.json`), fetched beside the index.
    pub name: String,
    /// The first and last filing dates it holds, `YYYY-MM-DD`.
    pub filing_from: String,
    pub filing_to: String,
}

/// A company's submissions index: its recent filings and its older pages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmissionsIndex {
    /// The company's name.
    pub name: String,
    /// The recent filings, as listed.
    pub filings: Vec<IndexedFiling>,
    /// The pages of its older filings.
    pub older_pages: Vec<OlderPage>,
}

/// Read the index document of company `cik` (ten digits; the document's own `cik` must equal it).
pub fn read_submissions(json: &[u8], cik: &str) -> Result<SubmissionsIndex, String> {
    let document: Value =
        serde_json::from_slice(json).map_err(|e| format!("it is not JSON ({e})"))?;
    let stated = match document.get("cik") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    };
    if stated.is_empty() || format!("{stated:0>10}") != cik {
        return Err(format!("it is company {stated:?}'s, not {cik}'s"));
    }
    let filings = document.get("filings").ok_or("it has no `filings`")?;
    let recent = filings.get("recent").ok_or("it has no `filings.recent`")?;
    let older_pages = filings
        .get("files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|page| OlderPage {
            name: string(page, "name"),
            filing_from: string(page, "filingFrom"),
            filing_to: string(page, "filingTo"),
        })
        .filter(|page| is_page_name(&page.name, cik))
        .collect();
    Ok(SubmissionsIndex { name: string(&document, "name"), filings: columns(recent)?, older_pages })
}

/// Read one page of older filings.
pub fn read_older_page(json: &[u8]) -> Result<Vec<IndexedFiling>, String> {
    let document: Value =
        serde_json::from_slice(json).map_err(|e| format!("it is not JSON ({e})"))?;
    columns(&document)
}

/// Whether `name` is a page of company `cik`'s older filings: `CIK<cik>-submissions-NNN.json`. What
/// makes it safe to put a page's name into a URL and a path.
pub fn is_page_name(name: &str, cik: &str) -> bool {
    name.strip_prefix(&format!("CIK{cik}-submissions-"))
        .and_then(|rest| rest.strip_suffix(".json"))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The filings of one set of column arrays.
fn columns(table: &Value) -> Result<Vec<IndexedFiling>, String> {
    let column = |name: &str| {
        table
            .get(name)
            .and_then(Value::as_array)
            .ok_or_else(|| format!("it has no `{name}` column"))
    };
    let (accessions, forms, accepted) =
        (column("accessionNumber")?, column("form")?, column("acceptanceDateTime")?);
    let sizes = table.get("size").and_then(Value::as_array);
    if forms.len() != accessions.len() || accepted.len() != accessions.len() {
        return Err("its columns are not all the same length".to_string());
    }
    let mut out = Vec::with_capacity(accessions.len());
    for (i, accession) in accessions.iter().enumerate() {
        let accession = accession.as_str().unwrap_or_default().to_string();
        if !super::is_accession(&accession) {
            return Err(format!("row {i}: {accession:?} is not an accession number"));
        }
        let stamp = accepted[i].as_str().unwrap_or_default();
        let accepted_ms = acceptance_ms(stamp)
            .ok_or_else(|| format!("{accession}: acceptanceDateTime {stamp:?} is not a time"))?;
        out.push(IndexedFiling {
            accession,
            form: forms[i].as_str().unwrap_or_default().to_string(),
            accepted_ms,
            bytes: sizes.and_then(|s| s.get(i)).and_then(Value::as_u64).unwrap_or(0),
        });
    }
    Ok(out)
}

/// `2023-11-02T22:08:27.000Z`, UTC, as epoch milliseconds; `None` for anything else (another offset
/// included).
fn acceptance_ms(stamp: &str) -> Option<i64> {
    let (date, time) = stamp.split_once('T')?;
    let (year, month, day) = super::parse_date(date)?;
    let rest = time.get(8..)?;
    let utc = rest.is_empty()
        || rest == "Z"
        || rest
            .strip_prefix('.')
            .is_some_and(|f| f.strip_suffix('Z').unwrap_or(f).bytes().all(|b| b.is_ascii_digit()));
    let mut clock = time.get(..8)?.split(':').map(|p| p.parse::<i64>().ok());
    let (hour, minute, second) = (clock.next()??, clock.next()??, clock.next()??);
    let in_day = (0..24).contains(&hour) && (0..60).contains(&minute) && (0..60).contains(&second);
    (utc && in_day).then(|| {
        vike_utility::calendar::days_from_civil(year, month, day) * vike_model::MS_PER_DAY
            + ((hour * 60 + minute) * 60 + second) * 1000
    })
}

/// `field` of `value` as a string, empty when absent.
fn string(value: &Value, field: &str) -> String {
    value.get(field).and_then(Value::as_str).unwrap_or_default().to_string()
}

#[cfg(test)]
#[path = "submissions_index_tests.rs"]
mod submissions_index_tests;
