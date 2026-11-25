//! **SEC EDGAR's DOWNLOAD** — the datahub fetches a company's filings (`sec-filings`) and its XBRL values
//! (`sec-companyfacts`) from the SEC into the dataset's folder under the imports root, where the import
//! then reads them unchanged (`crate::import::formats::sec_edgar`). The wire is
//! `vike_datahub_client::archive_fetch`'s; the CLI door is `vike-cli data history import sec-filings
//! <CIK> --from … --to … --fetch`.
//!
//! # SEC's fair-access rules, kept here
//!
//! - **A declared User-Agent naming a contact**: the WHOLE `User-Agent` is the box's
//!   `config.datahub_sec_user_agent` row (a name and an email). Unset — every install's default — the
//!   download refuses before a request, naming the row: SEC rate-limits per declared agent, so no two
//!   installs may share one, and no value is compiled in.
//! - **At most ten requests a second**: every GET of both downloads waits its turn at one
//!   [`SecRequestGate`] ([`SEC_REQUEST_SPACING`] apart), retries included, and the downloads run one
//!   request at a time (the request's `concurrency` is not used: the gate is the limit).
//! - A refused or failed request is asked again after the S3 download's pauses (`super::s3`'s
//!   `RETRY_PAUSES`) for a 429, a 5xx or a transport failure; a 403 (SEC's answer to an undeclared
//!   agent) is not, and says to check the row.
//!
//! # What a request fetches
//!
//! - `sec-filings`: the company's submissions index (`{data}/submissions/CIK##########.json`, plus each
//!   older page whose filing dates meet the window), then, for every UTC day of the window holding a
//!   filing of a kept form (`vike_data::sec_log::SEC_TEXT_KINDS`) and no day file yet, each filing's
//!   complete submission text (`{www}/Archives/edgar/data/<CIK>/<accession>/<accession>.txt`), written
//!   one after another as the day file `YYYY-MM-DD.txt`, atomically. The day of a filing is its EDGAR
//!   acceptance time in UTC, as the index states it (`vike_backfill::sec_edgar::submissions_index`).
//! - `sec-companyfacts`: `{data}/api/xbrl/companyfacts/CIK##########.json`, written as that name, when
//!   the folder lacks it. The window only says which month's request carries it; to refresh a company,
//!   delete the file and fetch again.
//!
//! The Form 13F data sets are one zip a quarter and are downloaded by hand from the SEC's page into
//! the `sec-form13f` folder (`docs/ops/sec-edgar-import.md`).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use vike_backfill::sec_edgar::cik_of_dataset;
use vike_backfill::sec_edgar::submissions_index::{
    IndexedFiling, read_older_page, read_submissions,
};
use vike_backfill::sec_edgar::texts::EDGAR_WWW;
use vike_data::archive_day::{SEC_COMPANY_FACTS_FORMAT, SEC_FILINGS_FORMAT};
use vike_data::sec_log::sec_text_kind;
use vike_datahub_client::archive_fetch::{
    FetchDone, FetchFailure, FetchFile, FetchOutcome, FetchPlan, FetchSpec,
};
use vike_model::MS_PER_DAY;
use vike_model::time::epoch_ms_to_utc_date;

use super::ArchiveDownload;

/// SEC's data host: the submissions index and the XBRL APIs.
pub const SEC_DATA: &str = "https://data.sec.gov";

/// The time between two SEC requests' starts: ten a second, SEC's stated ceiling.
pub const SEC_REQUEST_SPACING: Duration = Duration::from_millis(100);

/// The largest submissions index page read.
const INDEX_CAP: u64 = 64 * 1024 * 1024;

/// One request's whole wait (a large filing is tens of megabytes).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

/// How long opening the socket may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// **One HTTP GET** with the declared User-Agent, its 2xx body read up to `cap` bytes: the seam a test
/// replaces with a scripted server. `Err` carries the status when the server answered one.
pub trait SecHttp: Send + Sync {
    fn get(&self, url: &str, user_agent: &str, cap: u64) -> Result<Vec<u8>, SecHttpError>;
}

/// Why a GET failed. Carries no header (the User-Agent is not a secret, but nothing here echoes a
/// request).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecHttpError {
    /// The status SEC answered, when it answered one.
    pub status: Option<u16>,
    /// In words.
    pub why: String,
}

impl SecHttpError {
    /// Whether asking again may help: no answer, a 429 or a 5xx.
    fn transient(&self) -> bool {
        self.status.is_none_or(|s| s == 429 || s >= 500)
    }
}

/// **SEC's ten-a-second ceiling**, one per datahub: every GET of both downloads waits here.
#[derive(Debug)]
pub struct SecRequestGate {
    next: Mutex<Instant>,
    spacing: Duration,
}

impl SecRequestGate {
    /// A gate letting one request start every `spacing`.
    pub fn new(spacing: Duration) -> Self {
        Self { next: Mutex::new(Instant::now()), spacing }
    }

    /// Wait for this request's turn.
    fn wait(&self) {
        let start = {
            let mut next = self.next.lock().unwrap_or_else(PoisonError::into_inner);
            let start = (*next).max(Instant::now());
            *next = start + self.spacing;
            start
        };
        let now = Instant::now();
        if start > now {
            std::thread::sleep(start - now);
        }
    }
}

/// **One SEC download** (`sec-filings` or `sec-companyfacts`) over the shared gate and transport.
pub struct SecDownload {
    format: &'static str,
    user_agent: Option<String>,
    http: Arc<dyn SecHttp>,
    gate: Arc<SecRequestGate>,
    /// The two hosts, `https://www.sec.gov` and `https://data.sec.gov` (a loopback base in a test).
    www: String,
    data: String,
}

/// The two SEC downloads a datahub mounts, sharing one gate and one transport: `user_agent` is the
/// `config.datahub_sec_user_agent` row as booted.
pub fn sec_downloads(
    user_agent: Option<String>,
    http: Arc<dyn SecHttp>,
    www: &str,
    data: &str,
) -> Vec<SecDownload> {
    let gate = Arc::new(SecRequestGate::new(SEC_REQUEST_SPACING));
    [SEC_FILINGS_FORMAT, SEC_COMPANY_FACTS_FORMAT]
        .into_iter()
        .map(|format| SecDownload {
            format,
            user_agent: user_agent.clone(),
            http: Arc::clone(&http),
            gate: Arc::clone(&gate),
            www: www.to_string(),
            data: data.to_string(),
        })
        .collect()
}

/// The refusal of a box that declares no User-Agent.
pub fn no_user_agent_message() -> String {
    "this datahub declares no User-Agent to SEC EDGAR, so it downloads nothing from it. SEC's \
     fair-access policy asks every automated client for its name and a contact email, and limits \
     each declared agent's requests: declare YOUR OWN, never another install's, on the DATAHUB's \
     box:\n  vike-cli config set config.datahub_sec_user_agent \"Your Name you@example.com\"\nthen \
     restart the datahub. Nothing was requested."
        .to_string()
}

impl SecDownload {
    /// One GET through the gate, asked again on a transient failure.
    fn get(
        &self,
        url: &str,
        user_agent: &str,
        cap: u64,
        requests: &mut u64,
    ) -> Result<Vec<u8>, String> {
        let got = vike_utility::retry::retry_on_pauses(
            &super::s3::RETRY_PAUSES,
            |_| {
                self.gate.wait();
                *requests += 1;
                self.http.get(url, user_agent, cap)
            },
            // Settled: an answer, or a failure asking again cannot fix.
            |answer| answer.as_ref().map_or_else(|e| !e.transient(), |_| true),
            std::thread::sleep,
        );
        got.map_err(|e| match e.status {
            Some(403) => format!(
                "{url}: SEC answered HTTP 403, its answer to a client whose User-Agent it does not \
                 accept: check config.datahub_sec_user_agent (a name and a contact email)"
            ),
            Some(status) => {
                format!("{url}: SEC answered HTTP {status} ({}), after its retries", e.why)
            }
            None => format!("{url}: {}, after its retries", e.why),
        })
    }

    /// A company's indexed filings of kept forms accepted inside the window, by UTC day.
    fn filings_by_day(
        &self,
        cik: &str,
        spec: &FetchSpec,
        user_agent: &str,
        requests: &mut u64,
    ) -> Result<BTreeMap<i64, Vec<IndexedFiling>>, String> {
        let index_url = format!("{}/submissions/CIK{cik}.json", self.data);
        let index = read_submissions(&self.get(&index_url, user_agent, INDEX_CAP, requests)?, cik)
            .map_err(|why| format!("{index_url}: {why}"))?;
        let mut filings = index.filings;
        // An older page meets the window when its filing dates do: a filing is dated on or after the
        // day it was accepted, and at most a few days later.
        let (from, to) = (
            epoch_ms_to_utc_date(spec.from_day),
            epoch_ms_to_utc_date(spec.to_day + 7 * MS_PER_DAY),
        );
        for page in index.older_pages {
            if page.filing_to.as_str() < from.as_str() || page.filing_from.as_str() > to.as_str() {
                continue;
            }
            let url = format!("{}/submissions/{}", self.data, page.name);
            let older = read_older_page(&self.get(&url, user_agent, INDEX_CAP, requests)?)
                .map_err(|why| format!("{url}: {why}"))?;
            filings.extend(older);
        }
        let mut by_day: BTreeMap<i64, Vec<IndexedFiling>> = BTreeMap::new();
        for filing in filings {
            let day = filing.accepted_ms.div_euclid(MS_PER_DAY) * MS_PER_DAY;
            if (spec.from_day..=spec.to_day).contains(&day) && sec_text_kind(&filing.form).is_some()
            {
                by_day.entry(day).or_default().push(filing);
            }
        }
        for day in by_day.values_mut() {
            day.sort_by_key(|f| (f.accepted_ms, f.accession.clone()));
            day.dedup_by(|a, b| a.accession == b.accession);
        }
        Ok(by_day)
    }

    /// The `sec-filings` request.
    fn fetch_filings(
        &self,
        cik: &str,
        dir: &Path,
        spec: &FetchSpec,
        user_agent: &str,
        cap: u64,
        should_stop: &dyn Fn() -> bool,
    ) -> Result<FetchDone, String> {
        let mut requests = 0;
        let by_day = self.filings_by_day(cik, spec, user_agent, &mut requests)?;
        let mut plan = self.empty_plan(dir, requests);
        for (day, filings) in &by_day {
            let bytes = filings.iter().map(|f| f.bytes).sum();
            plan.listed += 1;
            plan.listed_bytes += bytes;
            let key = format!("{}.txt", epoch_ms_to_utc_date(*day));
            if dir.join(&key).is_file() {
                plan.held += 1;
            } else {
                plan.missing.push(FetchFile { day: *day, key, bytes });
            }
        }
        if !spec.go || plan.missing.is_empty() {
            return Ok(FetchDone { plan, fetched: None });
        }
        let mut outcome = FetchOutcome::default();
        let cik_number = cik.trim_start_matches('0');
        for file in &plan.missing {
            if should_stop() {
                break;
            }
            let mut day_file = Vec::new();
            let mut failed = None;
            for filing in &by_day[&file.day] {
                let folder = filing.accession.replace('-', "");
                let url = format!(
                    "{}/Archives/edgar/data/{cik_number}/{folder}/{}.txt",
                    self.www, filing.accession
                );
                match self.get(&url, user_agent, cap, &mut outcome.get_requests) {
                    Ok(text) => {
                        day_file.extend_from_slice(&text);
                        day_file.push(b'\n');
                    }
                    Err(why) => {
                        failed = Some(why);
                        break;
                    }
                }
            }
            match failed.map_or_else(|| write(&dir.join(&file.key), &day_file), Err) {
                Ok(()) => {
                    outcome.fetched += 1;
                    outcome.bytes += day_file.len() as u64;
                }
                Err(why) => outcome.failed.push(FetchFailure { day: file.day, why }),
            }
        }
        Ok(FetchDone { plan, fetched: Some(outcome) })
    }

    /// The `sec-companyfacts` request.
    fn fetch_companyfacts(
        &self,
        cik: &str,
        dir: &Path,
        spec: &FetchSpec,
        user_agent: &str,
        cap: u64,
    ) -> Result<FetchDone, String> {
        let key = format!("CIK{cik}.json");
        let mut plan = self.empty_plan(dir, 0);
        plan.listed = 1;
        if dir.join(&key).is_file() {
            plan.held = 1;
        } else {
            plan.missing.push(FetchFile { day: spec.from_day, key: key.clone(), bytes: 0 });
        }
        if !spec.go || plan.missing.is_empty() {
            return Ok(FetchDone { plan, fetched: None });
        }
        let mut outcome = FetchOutcome::default();
        let url = format!("{}/api/xbrl/companyfacts/{key}", self.data);
        match self.get(&url, user_agent, cap, &mut outcome.get_requests).and_then(|body| {
            write(&dir.join(&key), &body)?;
            Ok(body.len() as u64)
        }) {
            Ok(bytes) => {
                outcome.fetched = 1;
                outcome.bytes = bytes;
            }
            Err(why) => outcome.failed.push(FetchFailure { day: spec.from_day, why }),
        }
        Ok(FetchDone { plan, fetched: Some(outcome) })
    }

    fn empty_plan(&self, dir: &Path, list_requests: u64) -> FetchPlan {
        FetchPlan {
            source: EDGAR_WWW.to_string(),
            server_dir: dir.display().to_string(),
            list_requests,
            listed: 0,
            listed_bytes: 0,
            held: 0,
            missing: Vec::new(),
        }
    }
}

impl ArchiveDownload for SecDownload {
    fn format(&self) -> &'static str {
        self.format
    }

    fn fetch(
        &self,
        dataset_dir: &Path,
        spec: &FetchSpec,
        cap: u64,
        should_stop: &dyn Fn() -> bool,
    ) -> Result<FetchDone, String> {
        let cik = cik_of_dataset(self.format, &spec.dataset)?;
        let Some(user_agent) = self.user_agent.as_deref() else {
            return Err(no_user_agent_message());
        };
        if self.format == SEC_FILINGS_FORMAT {
            self.fetch_filings(&cik, dataset_dir, spec, user_agent, cap, should_stop)
        } else {
            self.fetch_companyfacts(&cik, dataset_dir, spec, user_agent, cap)
        }
    }
}

/// Write `bytes` at `path` atomically, its folder created: the import sees the whole day file or none.
fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("creating {}: {e}", parent.display()))?;
    }
    vike_utility::atomic_file::write_atomic(path, bytes)
        .map_err(|e| format!("writing {}: {e}", path.display()))
}

/// **The real transport**: one `ureq` agent, statuses read rather than raised, no redirect followed
/// off SEC's hosts beyond five, the body capped.
pub struct SecHttpUreq {
    agent: ureq::Agent,
}

impl Default for SecHttpUreq {
    fn default() -> Self {
        use vike_transport::http::{CallBound, StatusPolicy, public_get_agent_bounded};
        // Five redirects, the public-GET finisher's stated figure (`vike_transport::http::PUBLIC_GET_MAX_REDIRECTS`).
        let agent = public_get_agent_bounded(
            Some(CONNECT_TIMEOUT),
            CallBound::Total(REQUEST_TIMEOUT),
            StatusPolicy::Response,
        );
        Self { agent }
    }
}

impl SecHttp for SecHttpUreq {
    fn get(&self, url: &str, user_agent: &str, cap: u64) -> Result<Vec<u8>, SecHttpError> {
        let failed = |why: &str| SecHttpError { status: None, why: why.to_string() };
        let mut response = self
            .agent
            .get(url)
            .header("User-Agent", user_agent)
            .header("Accept-Encoding", "identity")
            .call()
            .map_err(|e| failed(&vike_transport::http::describe_error(&e)))?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(SecHttpError { status: Some(status), why: format!("HTTP {status}") });
        }
        response.body_mut().with_config().limit(cap).read_to_vec().map_err(|e| match e {
            ureq::Error::BodyExceedsLimit(_) => {
                failed(&format!("the body is over the {cap}-byte cap"))
            }
            // The fixed vocabulary, never the error's own text (`vike_transport::http::describe_error`).
            other => failed(&vike_transport::http::describe_error(&other)),
        })
    }
}

#[cfg(test)]
#[path = "sec_download_tests.rs"]
mod sec_download_tests;
