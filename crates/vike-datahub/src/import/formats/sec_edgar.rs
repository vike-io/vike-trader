//! **The three SEC EDGAR formats** (`sec-filings`, `sec-companyfacts`, `sec-form13f`): the decoders, the
//! day keys and the appends are `crates/vike-backfill/src/sec_edgar.rs`'s; this file maps them onto the
//! lane's seam and the wire. The operator's page is `docs/ops/sec-edgar-import.md`.
//!
//! - **`sec-filings <CIK>`**: one daily file per UTC day of EDGAR acceptance, `YYYY-MM-DD.txt`, the day's
//!   complete submission texts. Every filing must be accepted on its file's day and be ABOUT the folder's
//!   company (a schedule 13G a manager filed about another company is skipped and counted). An
//!   amendment's original is looked up in the store, so import a company's days in time order.
//! - **`sec-companyfacts <CIK>`**: the company's `CIK##########.json`, read at plan time for the days it
//!   holds values of (a probe), each day's values stored under that day.
//! - **`sec-form13f <data set>`**: the quarter's `*_form13f.zip`, read at plan time for the days its
//!   reports became knowable; each day's reports stored, one commit per manager.
//!
//! None derives bars (`--bars none`), and every day is one this format alone writes: a day is HELD when
//! its keys are stored, and an import of a held day writes only the series that miss its key.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};

use vike_backfill::sec_edgar::companyfacts::financial_facts;
use vike_backfill::sec_edgar::form13f::{Form13fFiling, holdings_of, read_filings};
use vike_backfill::sec_edgar::store::{
    append_facts_day, append_filing_day, append_holdings_day, held_facts_days, held_filing_days,
    held_holdings_days, original_of,
};
use vike_backfill::sec_edgar::submission::{parse_submission, split_day_file};
use vike_backfill::sec_edgar::texts::filing_text_rows;
use vike_backfill::sec_edgar::{cik_of_dataset, parse_date};
use vike_data::SeriesCoverage;
use vike_data::archive_day::{
    ArchiveDayRefusal, SEC_COMPANY_FACTS_FORMAT, SEC_FILINGS_FORMAT, SEC_FORM_13F_FORMAT,
};
use vike_data::sec_log::{SEC_AMENDMENT_SUFFIX, SEC_VENUE, sec_text_kind};
use vike_datahub_client::archive::{ArchiveDayClassWire, ArchiveDayResultWire, ImportSpec};
use vike_model::MS_PER_DAY;
use vike_model::time::epoch_ms_to_utc_date;
use vike_store::HistoryStoreDataFusion;

use super::empty_payload;
use crate::import::walk::FILE_TOO_LARGE;
use crate::import::{ArchiveFormat, ArchiveFormatLayoutFile, ArchiveFormatProbe, ImportSession};

/// The largest day file of filings read: a day of a large filer's complete submissions, exhibits and
/// XBRL included, is tens of megabytes.
const FILINGS_MAX_BYTES: u64 = 512 * 1024 * 1024;

/// The largest `companyfacts` document read (the largest companies' are tens of megabytes).
const COMPANYFACTS_MAX_BYTES: u64 = 256 * 1024 * 1024;

/// The largest 13F data-set zip read (a quarter is about a hundred megabytes).
const FORM13F_MAX_BYTES: u64 = 1024 * 1024 * 1024;

/// The class a day is refused under when a filing in its file was accepted on another UTC day, or
/// when a file is not the company's.
pub const WRONG_DAY: &str = "WrongDay";

/// The class a file this format cannot read is refused under.
pub const NOT_EDGAR: &str = "NotEdgar";

/// What an empty day says.
const NO_KEPT_FILING: &str = "the file holds no filing of a form the SEC text history keeps (10-K, \
     10-Q, 8-K, 20-F, 6-K, S-1, F-1, DEF 14A, 13D, 13G and their amendments) about this company. \
     Nothing was stored and no commit key was spent.";

/// The SEC formats, each over `store`.
pub(super) fn sec_formats(store: &Arc<HistoryStoreDataFusion>) -> Vec<Box<dyn ArchiveFormat>> {
    vec![
        Box::new(SecFilings { store: Arc::clone(store) }),
        Box::new(SecCompanyFacts { store: Arc::clone(store) }),
        Box::new(SecForm13f { store: Arc::clone(store), probed: Mutex::new(None) }),
    ]
}

/// The size check every SEC format makes at plan time: the file under its format's cap.
fn under_cap(file_len: u64, cap: u64) -> Result<Option<u64>, ArchiveDayRefusal> {
    if file_len > cap {
        return Err(ArchiveDayRefusal::new(
            FILE_TOO_LARGE,
            format!(
                "the file is {file_len} bytes, over this format's {cap}-byte cap. Nothing was read."
            ),
        ));
    }
    Ok(None)
}

/// The SEC formats derive no bars: a request naming any is refused before a directory is touched.
fn no_bars(source: &str, bars: &[String]) -> Result<Option<String>, String> {
    if bars.is_empty() {
        return Ok(None);
    }
    Err(format!("{source} derives no bars: import it with `--bars none`."))
}

/// What a day this format alone writes is, by its stored keys.
fn class_of(held: &BTreeSet<String>, day: i64) -> ArchiveDayClassWire {
    if held.contains(&epoch_ms_to_utc_date(day)) {
        ArchiveDayClassWire::HeldByArchive
    } else {
        ArchiveDayClassWire::Free
    }
}

/// An import's answer: what was written, or a held day that needed nothing.
fn stored(written: usize, report: String) -> ArchiveDayResultWire {
    if written == 0 {
        return ArchiveDayResultWire::ToppedUp { bars: Vec::new() };
    }
    ArchiveDayResultWire::Imported { ticks: written as u64, bars: Vec::new(), report: Some(report) }
}

// ---- sec-filings -------------------------------------------------------------------------------

/// EDGAR filing texts, one day file per UTC day of acceptance.
struct SecFilings {
    store: Arc<HistoryStoreDataFusion>,
}

impl ArchiveFormat for SecFilings {
    fn id(&self) -> &'static str {
        SEC_FILINGS_FORMAT
    }

    fn venue(&self) -> &'static str {
        SEC_VENUE
    }

    fn admit(&self, dataset: &str) -> Result<String, String> {
        let cik = cik_of_dataset(SEC_FILINGS_FORMAT, dataset)?;
        Ok(format!(
            "company {cik}: each section of each filing of a kept form is a text row under \
             kind=text, venue=<sec_10k, sec_8k …>, symbol={cik}, received at its EDGAR acceptance \
             time · one file per UTC day of acceptance, YYYY-MM-DD.txt · an amendment names its \
             original only when that is already stored, so import the days in time order"
        ))
    }

    fn accepts_dir(&self, _rel: &[&str], _now_ms: i64) -> bool {
        false
    }

    fn classify_file(&self, rel: &[&str], _now_ms: i64) -> ArchiveFormatLayoutFile {
        match rel {
            [name] => day_file(name)
                .map_or(ArchiveFormatLayoutFile::Other, ArchiveFormatLayoutFile::Daily),
            _ => ArchiveFormatLayoutFile::Other,
        }
    }

    fn max_file_bytes(&self) -> u64 {
        FILINGS_MAX_BYTES
    }

    fn header_len(&self) -> usize {
        0
    }

    fn read_header(&self, file_len: u64, _prefix: &[u8]) -> Result<Option<u64>, ArchiveDayRefusal> {
        under_cap(file_len, FILINGS_MAX_BYTES)
    }

    fn request_dir(&self, _dataset: &str, bars: &[String]) -> Result<Option<String>, String> {
        no_bars(SEC_FILINGS_FORMAT, bars)
    }

    fn session<'a>(
        &'a self,
        spec: &ImportSpec,
        _now_ms: i64,
    ) -> Result<Box<dyn ImportSession + 'a>, String> {
        let cik = cik_of_dataset(SEC_FILINGS_FORMAT, &spec.dataset)?;
        let held = held_filing_days(self.store.as_ref(), &cik).map_err(|e| e.to_string())?;
        Ok(Box::new(FilingsSession { hist: &self.store, cik, held }))
    }
}

/// The UTC day a day file's name `YYYY-MM-DD.txt` names, as its midnight in epoch milliseconds.
fn day_file(name: &str) -> Option<i64> {
    let (year, month, day) = parse_date(name.strip_suffix(".txt")?)?;
    name.as_bytes()
        .get(4)
        .is_some_and(|b| *b == b'-')
        .then(|| vike_utility::calendar::days_from_civil(year, month, day) * MS_PER_DAY)
}

struct FilingsSession<'h> {
    hist: &'h HistoryStoreDataFusion,
    cik: String,
    held: BTreeSet<String>,
}

/// What one day file decodes to.
struct FilingsDay {
    rows: Vec<vike_data::TextRow>,
    filings: usize,
    skipped: Vec<String>,
}

impl FilingsSession<'_> {
    /// Decode the day's file into its rows, or the day's refusal.
    fn decode(
        &self,
        day: i64,
        bytes: &[u8],
    ) -> Result<Result<FilingsDay, ArchiveDayRefusal>, String> {
        let text = String::from_utf8_lossy(bytes);
        let parts = match split_day_file(&text) {
            Ok(parts) => parts,
            Err(why) => {
                return Ok(Err(ArchiveDayRefusal::new(
                    NOT_EDGAR,
                    format!("{why}. Nothing was stored."),
                )));
            }
        };
        let mut out = FilingsDay { rows: Vec::new(), filings: 0, skipped: Vec::new() };
        for part in parts {
            let filing = match parse_submission(part) {
                Ok(filing) => filing,
                Err(why) => {
                    return Ok(Err(ArchiveDayRefusal::new(
                        NOT_EDGAR,
                        format!("{why}. Nothing was stored."),
                    )));
                }
            };
            if filing.accepted_ms.div_euclid(MS_PER_DAY) * MS_PER_DAY != day {
                return Ok(Err(ArchiveDayRefusal::new(
                    WRONG_DAY,
                    format!(
                        "{} was accepted on the UTC day {}, not this file's: move it to that day's \
                         file. Nothing was stored and no commit key was spent.",
                        filing.accession,
                        epoch_ms_to_utc_date(filing.accepted_ms)
                    ),
                )));
            }
            let Some(kind) = sec_text_kind(&filing.form) else {
                out.skipped.push(format!("{} ({})", filing.accession, filing.form));
                continue;
            };
            if filing.company_cik != self.cik {
                out.skipped.push(format!("{} (about {})", filing.accession, filing.company_cik));
                continue;
            }
            let amends = if filing.form.ends_with(SEC_AMENDMENT_SUFFIX) {
                original_of(self.hist, kind, &self.cik, &filing.period, filing.accepted_ms)
                    .map_err(|e| e.to_string())?
                    .unwrap_or_default()
            } else {
                String::new()
            };
            out.rows.extend(filing_text_rows(&filing, &amends));
            out.filings += 1;
        }
        Ok(Ok(out))
    }
}

impl FilingsDay {
    fn report(&self) -> String {
        let skipped = match self.skipped.len() {
            0 => String::new(),
            n => format!("; {n} skipped: {}", self.skipped.join(", ")),
        };
        format!("{} filings, {} sections{skipped}", self.filings, self.rows.len())
    }
}

impl ImportSession for FilingsSession<'_> {
    fn series(&self) -> Option<SeriesCoverage> {
        None
    }

    fn classify(&self, day: i64) -> ArchiveDayClassWire {
        class_of(&self.held, day)
    }

    fn import_day(
        &mut self,
        day: i64,
        read: &mut dyn FnMut() -> Result<Vec<u8>, ArchiveDayRefusal>,
    ) -> Result<ArchiveDayResultWire, String> {
        let bytes = match read() {
            Ok(bytes) => bytes,
            Err(refusal) => return Ok(ArchiveDayResultWire::Refused(refusal)),
        };
        let decoded = match self.decode(day, &bytes)? {
            Ok(decoded) => decoded,
            Err(refusal) => return Ok(ArchiveDayResultWire::Refused(refusal)),
        };
        if decoded.rows.is_empty() {
            return Ok(ArchiveDayResultWire::Refused(empty_payload(NO_KEPT_FILING)));
        }
        let written = append_filing_day(self.hist, &self.cik, day, &decoded.rows)
            .map_err(|e| e.to_string())?;
        self.held.insert(epoch_ms_to_utc_date(day));
        Ok(stored(written, decoded.report()))
    }

    fn verify_day(
        &mut self,
        day: i64,
        read: &mut dyn FnMut() -> Result<Vec<u8>, ArchiveDayRefusal>,
    ) -> Result<ArchiveDayResultWire, String> {
        let bytes = match read() {
            Ok(bytes) => bytes,
            Err(refusal) => return Ok(ArchiveDayResultWire::Refused(refusal)),
        };
        Ok(match self.decode(day, &bytes)? {
            Ok(decoded) if decoded.rows.is_empty() => {
                ArchiveDayResultWire::Refused(empty_payload(NO_KEPT_FILING))
            }
            Ok(decoded) => ArchiveDayResultWire::Verified {
                ticks: decoded.rows.len() as u64,
                report: Some(decoded.report()),
            },
            Err(refusal) => ArchiveDayResultWire::Refused(refusal),
        })
    }
}

// ---- sec-companyfacts --------------------------------------------------------------------------

/// A company's XBRL values, from its `companyfacts` document.
struct SecCompanyFacts {
    store: Arc<HistoryStoreDataFusion>,
}

impl ArchiveFormat for SecCompanyFacts {
    fn id(&self) -> &'static str {
        SEC_COMPANY_FACTS_FORMAT
    }

    fn venue(&self) -> &'static str {
        SEC_VENUE
    }

    fn admit(&self, dataset: &str) -> Result<String, String> {
        let cik = cik_of_dataset(SEC_COMPANY_FACTS_FORMAT, dataset)?;
        Ok(format!(
            "company {cik}: every reported XBRL value of CIK{cik}.json is a row under \
             kind=financial_fact, venue=sec, symbol={cik}, knowable at 03:00 UTC the day after its \
             filing date (the end of that EDGAR day) · a restated value is a later row"
        ))
    }

    fn accepts_dir(&self, _rel: &[&str], _now_ms: i64) -> bool {
        false
    }

    fn classify_file(&self, rel: &[&str], _now_ms: i64) -> ArchiveFormatLayoutFile {
        match rel {
            [name] if is_cik_json(name) => ArchiveFormatLayoutFile::Probe,
            _ => ArchiveFormatLayoutFile::Other,
        }
    }

    fn max_file_bytes(&self) -> u64 {
        COMPANYFACTS_MAX_BYTES
    }

    fn header_len(&self) -> usize {
        0
    }

    fn read_header(&self, file_len: u64, _prefix: &[u8]) -> Result<Option<u64>, ArchiveDayRefusal> {
        under_cap(file_len, COMPANYFACTS_MAX_BYTES)
    }

    fn request_dir(&self, _dataset: &str, bars: &[String]) -> Result<Option<String>, String> {
        no_bars(SEC_COMPANY_FACTS_FORMAT, bars)
    }

    fn probe(
        &self,
        spec: &ImportSpec,
        file_name: &str,
        file: &[u8],
    ) -> Result<ArchiveFormatProbe, String> {
        let cik = cik_of_dataset(SEC_COMPANY_FACTS_FORMAT, &spec.dataset)?;
        let facts = financial_facts(file, &cik).map_err(|why| format!("{file_name}: {why}"))?;
        let days: BTreeSet<i64> =
            facts.iter().map(|f| f.filed_ms.div_euclid(MS_PER_DAY) * MS_PER_DAY).collect();
        Ok(ArchiveFormatProbe { days: days.into_iter().collect(), preview: Vec::new() })
    }

    fn session<'a>(
        &'a self,
        spec: &ImportSpec,
        _now_ms: i64,
    ) -> Result<Box<dyn ImportSession + 'a>, String> {
        let cik = cik_of_dataset(SEC_COMPANY_FACTS_FORMAT, &spec.dataset)?;
        let held = held_facts_days(self.store.as_ref(), &cik).map_err(|e| e.to_string())?;
        Ok(Box::new(FactsSession { hist: &self.store, cik, held }))
    }
}

/// `CIK##########.json`.
fn is_cik_json(name: &str) -> bool {
    name.strip_prefix("CIK")
        .and_then(|rest| rest.strip_suffix(".json"))
        .is_some_and(|cik| cik.len() == 10 && cik.bytes().all(|b| b.is_ascii_digit()))
}

struct FactsSession<'h> {
    hist: &'h HistoryStoreDataFusion,
    cik: String,
    held: BTreeSet<String>,
}

impl FactsSession<'_> {
    /// The values of `day` in the document `bytes`, or the day's refusal.
    fn decode(
        &self,
        day: i64,
        bytes: &[u8],
    ) -> Result<Vec<vike_data::FinancialFactRow>, ArchiveDayRefusal> {
        let facts = financial_facts(bytes, &self.cik).map_err(|why| {
            ArchiveDayRefusal::new(NOT_EDGAR, format!("{why}. Nothing was stored."))
        })?;
        Ok(facts
            .into_iter()
            .filter(|f| f.filed_ms.div_euclid(MS_PER_DAY) * MS_PER_DAY == day)
            .collect())
    }
}

impl ImportSession for FactsSession<'_> {
    fn series(&self) -> Option<SeriesCoverage> {
        None
    }

    fn classify(&self, day: i64) -> ArchiveDayClassWire {
        class_of(&self.held, day)
    }

    fn import_day(
        &mut self,
        day: i64,
        read: &mut dyn FnMut() -> Result<Vec<u8>, ArchiveDayRefusal>,
    ) -> Result<ArchiveDayResultWire, String> {
        let rows = match read().and_then(|bytes| self.decode(day, &bytes)) {
            Ok(rows) => rows,
            Err(refusal) => return Ok(ArchiveDayResultWire::Refused(refusal)),
        };
        let written =
            append_facts_day(self.hist, &self.cik, day, &rows).map_err(|e| e.to_string())?;
        self.held.insert(epoch_ms_to_utc_date(day));
        let filings: BTreeSet<&str> = rows.iter().map(|r| r.accession.as_str()).collect();
        Ok(stored(written, format!("{} values from {} filings", rows.len(), filings.len())))
    }

    fn verify_day(
        &mut self,
        day: i64,
        read: &mut dyn FnMut() -> Result<Vec<u8>, ArchiveDayRefusal>,
    ) -> Result<ArchiveDayResultWire, String> {
        Ok(match read().and_then(|bytes| self.decode(day, &bytes)) {
            Ok(rows) => ArchiveDayResultWire::Verified { ticks: rows.len() as u64, report: None },
            Err(refusal) => ArchiveDayResultWire::Refused(refusal),
        })
    }
}

// ---- sec-form13f -------------------------------------------------------------------------------

/// A quarter's Form 13F data set.
struct SecForm13f {
    store: Arc<HistoryStoreDataFusion>,
    /// The filings the last probe read, by dataset: what a session classifies its days against
    /// without reading the zip a second time.
    probed: Mutex<Option<(String, Vec<Form13fFiling>)>>,
}

/// Whether a 13F submission carries an information table (`13F-HR`, `13F-HR/A`; a `13F-NT` notice
/// does not).
fn has_holdings(filing: &Form13fFiling) -> bool {
    filing.submission_type.starts_with("13F-HR")
}

impl ArchiveFormat for SecForm13f {
    fn id(&self) -> &'static str {
        SEC_FORM_13F_FORMAT
    }

    fn venue(&self) -> &'static str {
        SEC_VENUE
    }

    fn admit(&self, dataset: &str) -> Result<String, String> {
        Ok(format!(
            "the 13F data set {dataset}: every information-table line is a row under \
             kind=institutional_holding, venue=sec, symbol=<the manager's CIK>, knowable at 03:00 \
             UTC the day after its filing date · values in whole dollars (reports filed before \
             2023-01-03 stated thousands and are multiplied out) · one *_form13f.zip per folder"
        ))
    }

    fn accepts_dir(&self, _rel: &[&str], _now_ms: i64) -> bool {
        false
    }

    fn classify_file(&self, rel: &[&str], _now_ms: i64) -> ArchiveFormatLayoutFile {
        match rel {
            [name] if name.to_ascii_lowercase().ends_with("_form13f.zip") => {
                ArchiveFormatLayoutFile::Probe
            }
            _ => ArchiveFormatLayoutFile::Other,
        }
    }

    fn max_file_bytes(&self) -> u64 {
        FORM13F_MAX_BYTES
    }

    fn header_len(&self) -> usize {
        0
    }

    fn read_header(&self, file_len: u64, _prefix: &[u8]) -> Result<Option<u64>, ArchiveDayRefusal> {
        under_cap(file_len, FORM13F_MAX_BYTES)
    }

    fn request_dir(&self, _dataset: &str, bars: &[String]) -> Result<Option<String>, String> {
        no_bars(SEC_FORM_13F_FORMAT, bars)
    }

    fn probe(
        &self,
        spec: &ImportSpec,
        file_name: &str,
        file: &[u8],
    ) -> Result<ArchiveFormatProbe, String> {
        let filings = read_filings(file).map_err(|why| format!("{file_name}: {why}"))?;
        let days: BTreeSet<i64> =
            filings.iter().filter(|f| has_holdings(f)).map(Form13fFiling::day).collect();
        *self.probed.lock().unwrap_or_else(PoisonError::into_inner) =
            Some((spec.dataset.clone(), filings));
        Ok(ArchiveFormatProbe { days: days.into_iter().collect(), preview: Vec::new() })
    }

    /// The days are classified against the probe's filings: a day is HELD when every manager reporting
    /// on it holds its key.
    fn session<'a>(
        &'a self,
        spec: &ImportSpec,
        _now_ms: i64,
    ) -> Result<Box<dyn ImportSession + 'a>, String> {
        let probed = self.probed.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let filings = match probed {
            Some((dataset, filings)) if dataset == spec.dataset => filings,
            _ => Vec::new(),
        };
        let mut filers_by_day: BTreeMap<i64, BTreeSet<String>> = BTreeMap::new();
        for filing in filings.iter().filter(|f| has_holdings(f)) {
            filers_by_day.entry(filing.day()).or_default().insert(filing.filer_cik.clone());
        }
        let mut held_by_filer: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
        for filer in filers_by_day.values().flatten() {
            if !held_by_filer.contains_key(filer.as_str()) {
                let days =
                    held_holdings_days(self.store.as_ref(), filer).map_err(|e| e.to_string())?;
                held_by_filer.insert(filer, days);
            }
        }
        let held = filers_by_day
            .iter()
            .filter(|(day, filers)| {
                let label = epoch_ms_to_utc_date(**day);
                filers
                    .iter()
                    .all(|f| held_by_filer.get(f.as_str()).is_some_and(|d| d.contains(&label)))
            })
            .map(|(day, _)| epoch_ms_to_utc_date(*day))
            .collect();
        Ok(Box::new(Form13fSession { hist: &self.store, held }))
    }
}

struct Form13fSession<'h> {
    hist: &'h HistoryStoreDataFusion,
    held: BTreeSet<String>,
}

impl Form13fSession<'_> {
    /// The day's lines in the data set `bytes`, and how many reports they come from.
    fn decode(
        &self,
        day: i64,
        bytes: &[u8],
    ) -> Result<(Vec<vike_data::InstitutionalHoldingRow>, usize), ArchiveDayRefusal> {
        let not_read =
            |why: String| ArchiveDayRefusal::new(NOT_EDGAR, format!("{why}. Nothing was stored."));
        let filings = read_filings(bytes).map_err(not_read)?;
        let today: Vec<&Form13fFiling> =
            filings.iter().filter(|f| has_holdings(f) && f.day() == day).collect();
        let rows = holdings_of(bytes, &today).map_err(not_read)?;
        Ok((rows, today.len()))
    }
}

impl ImportSession for Form13fSession<'_> {
    fn series(&self) -> Option<SeriesCoverage> {
        None
    }

    fn classify(&self, day: i64) -> ArchiveDayClassWire {
        class_of(&self.held, day)
    }

    fn import_day(
        &mut self,
        day: i64,
        read: &mut dyn FnMut() -> Result<Vec<u8>, ArchiveDayRefusal>,
    ) -> Result<ArchiveDayResultWire, String> {
        let (rows, reports) = match read().and_then(|bytes| self.decode(day, &bytes)) {
            Ok(decoded) => decoded,
            Err(refusal) => return Ok(ArchiveDayResultWire::Refused(refusal)),
        };
        if rows.is_empty() {
            return Ok(ArchiveDayResultWire::Refused(empty_payload(
                "no report of this day holds an information-table line. Nothing was stored and no \
                 commit key was spent.",
            )));
        }
        let written = append_holdings_day(self.hist, day, &rows).map_err(|e| e.to_string())?;
        self.held.insert(epoch_ms_to_utc_date(day));
        Ok(stored(written, format!("{} lines from {reports} reports", rows.len())))
    }

    fn verify_day(
        &mut self,
        day: i64,
        read: &mut dyn FnMut() -> Result<Vec<u8>, ArchiveDayRefusal>,
    ) -> Result<ArchiveDayResultWire, String> {
        Ok(match read().and_then(|bytes| self.decode(day, &bytes)) {
            Ok((rows, reports)) => ArchiveDayResultWire::Verified {
                ticks: rows.len() as u64,
                report: Some(format!("{reports} reports")),
            },
            Err(refusal) => ArchiveDayResultWire::Refused(refusal),
        })
    }
}

#[cfg(test)]
#[path = "sec_edgar_tests.rs"]
mod sec_edgar_tests;
