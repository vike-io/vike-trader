//! The three SEC formats through the lane's own verb, over a real store and files laid out as the SEC
//! download writes them: hand-built fixtures in the SEC's documented formats
//! (`vike_backfill::testing::sec_edgar`).

use std::path::PathBuf;
use std::sync::Arc;

use vike_backfill::testing::sec_edgar::{
    CIK, COMPANYFACTS, EIGHT_K, FILER, SCHEDULE_13G, TEN_K, TEN_K_AMENDMENT, form13f_zip,
};
use vike_data::sec_log::SEC_VENUE;
use vike_data::{HistoryStore, TimestampRange};
use vike_datahub_client::archive::{
    ArchiveDayClassWire, ArchiveDayResultWire, ImportDone, ImportSpec,
};
use vike_datahub_client::proto::Response;
use vike_model::MS_PER_DAY;
use vike_store::HistoryStoreDataFusion;

use super::{NOT_EDGAR, WRONG_DAY, sec_formats};
use crate::import::{ImportLane, import_archive_verb};

/// 2023-11-02, 2023-11-04, 2024-01-10, 2024-01-11, 2024-02-01, 2024-02-15 and 2024-02-21, UTC.
const NOV_2: i64 = 1_698_883_200_000;
const NOV_4: i64 = NOV_2 + 2 * MS_PER_DAY;
const JAN_10: i64 = 1_704_844_800_000;
const JAN_11: i64 = JAN_10 + MS_PER_DAY;
const FEB_1: i64 = 1_706_745_600_000;
const FEB_15: i64 = FEB_1 + 14 * MS_PER_DAY;
const FEB_21: i64 = FEB_1 + 20 * MS_PER_DAY;

struct Project {
    tmp: tempfile::TempDir,
    store: Arc<HistoryStoreDataFusion>,
}

impl Project {
    fn new() -> Project {
        let tmp = tempfile::tempdir().expect("a temp dir");
        let store =
            Arc::new(HistoryStoreDataFusion::open(tmp.path().join("hist")).expect("a store"));
        Project { tmp, store }
    }

    fn put(&self, format: &str, dataset: &str, name: &str, bytes: &[u8]) {
        let dir: PathBuf = self.tmp.path().join("imports").join(format).join(dataset);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(name), bytes).unwrap();
    }

    fn answer(&self, spec: &ImportSpec) -> Response {
        let lane = ImportLane::new(self.tmp.path().join("imports"), sec_formats(&self.store));
        import_archive_verb(spec, Some(&lane), &|| false)
    }

    fn run(&self, spec: &ImportSpec) -> ImportDone {
        match self.answer(spec) {
            Response::ArchiveImported(done) => *done,
            other => panic!("not an import's answer: {other:?}"),
        }
    }
}

fn spec(format: &str, dataset: &str, window: Option<(i64, i64)>) -> ImportSpec {
    ImportSpec {
        format: format.to_string(),
        dataset: dataset.to_string(),
        from_day: window.map(|w| w.0),
        to_day: window.map(|w| w.1),
        bars: Vec::new(),
        dry_run: false,
        verify: false,
        replace_holed: false,
        target: None,
    }
}

fn results(done: &ImportDone) -> Vec<(i64, ArchiveDayResultWire)> {
    done.outcome
        .as_ref()
        .expect("an outcome")
        .days
        .iter()
        .map(|d| (d.day, d.result.clone()))
        .collect()
}

fn imported(result: &ArchiveDayResultWire) -> u64 {
    match result {
        ArchiveDayResultWire::Imported { ticks, .. } => *ticks,
        other => panic!("not imported: {other:?}"),
    }
}

#[test]
fn a_companys_filing_days_import_in_time_order_and_a_rerun_writes_nothing() {
    let project = Project::new();
    project.put("sec-filings", CIK, "2023-11-02.txt", TEN_K.as_bytes());
    project.put("sec-filings", CIK, "2024-01-10.txt", TEN_K_AMENDMENT.as_bytes());
    project.put(
        "sec-filings",
        CIK,
        "2024-02-01.txt",
        format!("{EIGHT_K}\n{SCHEDULE_13G}").as_bytes(),
    );
    project.put("sec-filings", CIK, "notes.md", b"not a day file");

    for window in [(NOV_2, NOV_2 + 30 * MS_PER_DAY), (JAN_10, JAN_10), (FEB_1, FEB_1)] {
        let done = project.run(&spec("sec-filings", CIK, Some(window)));
        for (_, result) in results(&done) {
            assert!(imported(&result) > 0, "{result:?}");
        }
    }
    let ten_k = project.store.scan_texts("sec_10k", CIK, TimestampRange::all()).unwrap();
    assert_eq!(ten_k.len(), 5, "four sections of the 10-K and one of its amendment");
    assert_eq!(ten_k[4].amends, "0001234567-23-000106/item_1a", "the original was stored first");
    assert_eq!(project.store.scan_texts("sec_8k", CIK, TimestampRange::all()).unwrap().len(), 3);
    assert_eq!(project.store.scan_texts("sec_13g", CIK, TimestampRange::all()).unwrap().len(), 1);
    // The point-in-time read: on 2024-01-09 the 10-K is known, its amendment is not.
    let known = project.store.texts_as_of("sec_10k", CIK, JAN_10 - 1, 400 * MS_PER_DAY).unwrap();
    assert_eq!(known.len(), 4);

    let again = project.run(&spec("sec-filings", CIK, Some((FEB_1, FEB_1))));
    assert_eq!(again.plan.days[0].class, ArchiveDayClassWire::HeldByArchive);
    assert_eq!(results(&again)[0].1, ArchiveDayResultWire::ToppedUp { bars: Vec::new() });
    assert_eq!(project.store.scan_texts("sec_8k", CIK, TimestampRange::all()).unwrap().len(), 3);
}

#[test]
fn a_filing_on_the_wrong_day_or_about_another_company_stores_nothing() {
    let project = Project::new();
    project.put("sec-filings", CIK, "2023-11-03.txt", TEN_K.as_bytes());
    project.put("sec-filings", CIK, "2023-11-04.txt", b"<html>not EDGAR</html>");
    let done = project.run(&spec("sec-filings", CIK, None));
    let refused: Vec<String> = results(&done)
        .into_iter()
        .map(|(_, r)| match r {
            ArchiveDayResultWire::Refused(refusal) => refusal.class,
            other => panic!("not refused: {other:?}"),
        })
        .collect();
    assert_eq!(refused, [WRONG_DAY, NOT_EDGAR]);
    // The manager's own folder: its schedule 13G is about another company, so its day keeps nothing.
    project.put("sec-filings", FILER, "2024-02-01.txt", SCHEDULE_13G.as_bytes());
    let done = project.run(&spec("sec-filings", FILER, None));
    assert!(
        matches!(&results(&done)[0].1, ArchiveDayResultWire::Refused(r) if r.class == super::super::EMPTY_PAYLOAD)
    );
    assert!(project.store.scan_texts("sec_13g", FILER, TimestampRange::all()).unwrap().is_empty());
}

#[test]
fn companyfacts_land_on_the_day_after_their_filing_date() {
    let project = Project::new();
    project.put("sec-companyfacts", CIK, &format!("CIK{CIK}.json"), COMPANYFACTS.as_bytes());
    let plan = project.run(&ImportSpec { dry_run: true, ..spec("sec-companyfacts", CIK, None) });
    let days: Vec<i64> = plan.plan.days.iter().map(|d| d.day).collect();
    assert_eq!(days, [NOV_4, JAN_11], "the days its values became knowable, and only those");
    let mut written = Vec::new();
    for window in [(NOV_4, NOV_4 + 30 * MS_PER_DAY), (JAN_11, JAN_11)] {
        let done = project.run(&spec("sec-companyfacts", CIK, Some(window)));
        written.extend(results(&done).iter().map(|(_, r)| imported(r)));
    }
    assert_eq!(written, [2, 1]);
    let facts = project.store.scan_financial_facts(SEC_VENUE, CIK, TimestampRange::all()).unwrap();
    assert_eq!(facts.len(), 3);
    let as_of =
        project.store.financial_facts_as_of(SEC_VENUE, CIK, JAN_11 - 1, 400 * MS_PER_DAY).unwrap();
    assert_eq!(as_of.len(), 2, "the restated revenue is not known before its filing day ends");
    let again = project.run(&ImportSpec { dry_run: true, ..spec("sec-companyfacts", CIK, None) });
    assert!(again.plan.days.iter().all(|d| d.class == ArchiveDayClassWire::HeldByArchive));
}

#[test]
fn a_13f_data_set_lands_per_manager_and_its_days_are_held_once_stored() {
    let project = Project::new();
    project.put("sec-form13f", "2024Q1", "2024q1_form13f.zip", &form13f_zip());
    let done = project.run(&spec("sec-form13f", "2024Q1", Some((FEB_1, FEB_1 + 28 * MS_PER_DAY))));
    let days: Vec<i64> = done.plan.days.iter().map(|d| d.day).collect();
    assert_eq!(days, [FEB_15, FEB_21], "the notice's day is no day: it holds no line");
    let written: Vec<u64> = results(&done).iter().map(|(_, r)| imported(r)).collect();
    assert_eq!(written, [2, 1]);
    let lines =
        project.store.scan_institutional_holdings(SEC_VENUE, FILER, TimestampRange::all()).unwrap();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[2].amendment_type, "RESTATEMENT");
    let again = project.run(&spec("sec-form13f", "2024Q1", Some((FEB_1, FEB_1 + 28 * MS_PER_DAY))));
    assert!(again.plan.days.iter().all(|d| d.class == ArchiveDayClassWire::HeldByArchive));
}

#[test]
fn bars_and_a_dataset_that_is_no_cik_are_refused_at_the_door() {
    let project = Project::new();
    let bars = ImportSpec { bars: vec!["1m".to_string()], ..spec("sec-filings", CIK, None) };
    match project.answer(&bars) {
        Response::Refused(why) => assert!(why.text.contains("--bars none"), "{why}"),
        other => panic!("not refused: {other:?}"),
    }
    match project.answer(&spec("sec-companyfacts", "AAPL", None)) {
        Response::Refused(why) => assert!(why.text.contains("ten digits"), "{why}"),
        other => panic!("not refused: {other:?}"),
    }
}
