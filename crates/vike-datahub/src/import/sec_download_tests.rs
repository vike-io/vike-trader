use std::collections::HashMap;
use std::time::Instant;

use vike_backfill::testing::sec_edgar::{
    CIK, COMPANYFACTS, EIGHT_K, SCHEDULE_13G, SUBMISSIONS, TEN_K, TEN_K_AMENDMENT,
};

use super::*;

const WWW: &str = "https://www.example.test";
const DATA: &str = "https://data.example.test";
const AGENT: &str = "Example Research admin@example.test";

/// 2023-11-02, 2024-01-10 and 2024-02-01, UTC.
const NOV_2: i64 = 1_698_883_200_000;
const JAN_10: i64 = 1_704_844_800_000;
const FEB_1: i64 = 1_706_745_600_000;

/// SEC as a script: each URL's answer, and every request it was asked, with its User-Agent.
struct ScriptedSec {
    answers: HashMap<String, Result<Vec<u8>, SecHttpError>>,
    asked: Mutex<Vec<(String, String)>>,
}

impl ScriptedSec {
    fn new(answers: &[(&str, Result<&str, u16>)]) -> Arc<ScriptedSec> {
        let answers = answers
            .iter()
            .map(|(url, answer)| {
                let answer = answer.map(|body| body.as_bytes().to_vec()).map_err(|status| {
                    SecHttpError { status: Some(status), why: format!("HTTP {status}") }
                });
                (url.to_string(), answer)
            })
            .collect();
        Arc::new(ScriptedSec { answers, asked: Mutex::new(Vec::new()) })
    }

    fn asked(&self) -> Vec<(String, String)> {
        self.asked.lock().unwrap().clone()
    }
}

impl SecHttp for ScriptedSec {
    fn get(&self, url: &str, user_agent: &str, _cap: u64) -> Result<Vec<u8>, SecHttpError> {
        self.asked.lock().unwrap().push((url.to_string(), user_agent.to_string()));
        self.answers
            .get(url)
            .cloned()
            .unwrap_or(Err(SecHttpError { status: Some(404), why: "HTTP 404".into() }))
    }
}

fn filing_url(accession: &str) -> String {
    let folder = accession.replace('-', "");
    format!("{WWW}/Archives/edgar/data/1234567/{folder}/{accession}.txt")
}

fn index_url() -> String {
    format!("{DATA}/submissions/CIK{CIK}.json")
}

fn downloads(agent: Option<&str>, sec: &Arc<ScriptedSec>) -> Vec<SecDownload> {
    let http: Arc<dyn SecHttp> = Arc::clone(sec) as Arc<dyn SecHttp>;
    sec_downloads(agent.map(str::to_string), http, WWW, DATA)
}

fn spec(from_day: i64, to_day: i64, go: bool) -> FetchSpec {
    FetchSpec {
        format: SEC_FILINGS_FORMAT.to_string(),
        dataset: CIK.to_string(),
        from_day,
        to_day,
        go,
        concurrency: 8,
    }
}

#[test]
fn a_box_that_declares_no_user_agent_requests_nothing() {
    let sec = ScriptedSec::new(&[]);
    let dir = tempfile::tempdir().unwrap();
    for download in downloads(None, &sec) {
        let err =
            download.fetch(dir.path(), &spec(NOV_2, FEB_1, true), 1 << 20, &|| false).unwrap_err();
        assert!(err.contains("config.datahub_sec_user_agent"), "{err}");
    }
    assert!(sec.asked().is_empty(), "not one request without a declared agent");
}

#[test]
fn the_filings_of_a_window_are_written_as_one_file_per_acceptance_day() {
    let urls = [
        (index_url(), SUBMISSIONS),
        (filing_url("0001234567-23-000106"), TEN_K),
        (filing_url("0001234567-24-000002"), TEN_K_AMENDMENT),
        (filing_url("0001234567-24-000010"), EIGHT_K),
        (filing_url("0009876543-24-000001"), SCHEDULE_13G),
    ];
    let script: Vec<(&str, Result<&str, u16>)> =
        urls.iter().map(|(u, b)| (u.as_str(), Ok(*b))).collect();
    let sec = ScriptedSec::new(&script);
    let [filings, _] = <[SecDownload; 2]>::try_from(downloads(Some(AGENT), &sec)).ok().unwrap();
    let dir = tempfile::tempdir().unwrap();

    let plan =
        filings.fetch(dir.path(), &spec(NOV_2, FEB_1, false), 1 << 20, &|| false).unwrap().plan;
    let days: Vec<i64> = plan.missing.iter().map(|f| f.day).collect();
    assert_eq!(days, [NOV_2, JAN_10, FEB_1], "the Form 4 is no kept form, so its day is no file");
    assert_eq!(plan.missing[0].key, "2023-11-02.txt");
    assert_eq!(plan.list_requests, 1, "the older page ends in 2015: not asked");
    assert_eq!(sec.asked().len(), 1);

    let done = filings.fetch(dir.path(), &spec(NOV_2, FEB_1, true), 1 << 20, &|| false).unwrap();
    let outcome = done.fetched.expect("a download");
    assert_eq!((outcome.fetched, outcome.failed.len()), (3, 0), "{:?}", outcome.failed);
    let feb_1 = std::fs::read_to_string(dir.path().join("2024-02-01.txt")).unwrap();
    assert_eq!(feb_1, format!("{SCHEDULE_13G}\n{EIGHT_K}\n"), "in acceptance order");
    assert!(sec.asked().iter().all(|(_, agent)| agent == AGENT), "every request declares the row");

    let again = filings.fetch(dir.path(), &spec(NOV_2, FEB_1, true), 1 << 20, &|| false).unwrap();
    assert_eq!(
        (again.plan.held, again.plan.missing.len()),
        (3, 0),
        "a written day is never fetched again"
    );
}

#[test]
fn a_403_names_the_row_and_is_not_asked_again() {
    let index = index_url();
    let sec = ScriptedSec::new(&[(index.as_str(), Err(403))]);
    let [filings, _] = <[SecDownload; 2]>::try_from(downloads(Some(AGENT), &sec)).ok().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let err =
        filings.fetch(dir.path(), &spec(NOV_2, FEB_1, false), 1 << 20, &|| false).unwrap_err();
    assert!(err.contains("HTTP 403") && err.contains("config.datahub_sec_user_agent"), "{err}");
    assert_eq!(sec.asked().len(), 1, "a 403 is not transient");
}

#[test]
fn companyfacts_is_one_file_fetched_once() {
    let url = format!("{DATA}/api/xbrl/companyfacts/CIK{CIK}.json");
    let sec = ScriptedSec::new(&[(url.as_str(), Ok(COMPANYFACTS))]);
    let [_, facts] = <[SecDownload; 2]>::try_from(downloads(Some(AGENT), &sec)).ok().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let spec =
        FetchSpec { format: SEC_COMPANY_FACTS_FORMAT.to_string(), ..spec(NOV_2, NOV_2, true) };
    let done = facts.fetch(dir.path(), &spec, 1 << 20, &|| false).unwrap();
    assert_eq!(done.fetched.map(|o| o.fetched), Some(1));
    assert_eq!(
        std::fs::read_to_string(dir.path().join(format!("CIK{CIK}.json"))).unwrap(),
        COMPANYFACTS
    );
    let again = facts.fetch(dir.path(), &spec, 1 << 20, &|| false).unwrap();
    assert_eq!(again.plan.held, 1);
    assert_eq!(sec.asked().len(), 1);
}

#[test]
fn the_gate_spaces_every_request() {
    let gate = SecRequestGate::new(Duration::from_millis(40));
    let start = Instant::now();
    for _ in 0..4 {
        gate.wait();
    }
    assert!(start.elapsed() >= Duration::from_millis(120), "{:?}", start.elapsed());
}
