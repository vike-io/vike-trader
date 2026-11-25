//! **LIVE check of the SEC EDGAR import against SEC itself** — `#[ignore]`d, run by hand from a lane,
//! never in CI: a handful of requests, well under SEC's ten a second.
//!
//! In a SCRATCH project whose own `vike.db` holds the `config.datahub_sec_user_agent` row, it loads the
//! settings the way the daemon's boot does (`vike_secrets::read_settings_in`, then
//! `vike_config::load_with_source`), mounts the import lane with the real formats and the real SEC
//! downloads over a store in that project, and runs the two verbs the datahub serves
//! (`fetch_archive_verb`, `import_archive_verb`; the wire and its device-key handshake are the other
//! suites'): one company's filings of a two-day window and its `companyfacts`, downloaded, imported and
//! read back point-in-time. One more GET, through the same transport and agent, holds the submissions
//! index's `acceptanceDateTime` (UTC) against the filing header's `<ACCEPTANCE-DATETIME>` (New York
//! time): one instant. Its first run (2026-11-16) is what showed the index's clock is UTC.
//!
//! ```sh
//! cargo test -p vike-datahub --features backfill-serve --test sec_edgar_live -- --ignored --nocapture
//! ```
//!
//! The declared agent is the one the owner chose for OUR boxes on 2026-11-16 ([`OUR_BOXES_AGENT`]), and
//! it reaches the download only as a row of the scratch project's `vike.db`: no binary ships it, and a
//! user declares their own.
#![cfg(feature = "backfill-serve")]

use std::sync::Arc;

use vike_backfill::sec_edgar::submission::{parse_submission, split_day_file};
use vike_backfill::sec_edgar::submissions_index::read_submissions;
use vike_backfill::sec_edgar::texts::EDGAR_WWW;
use vike_data::{HistoryStore, TimestampRange};
use vike_datahub::import::formats::real_import_registry;
use vike_datahub::import::sec_download::{SEC_DATA, SecHttp, SecHttpUreq, sec_downloads};
use vike_datahub::import::{ImportLane, fetch_archive_verb, import_archive_verb};
use vike_datahub_client::archive::{ArchiveDayResultWire, ImportDone, ImportSpec};
use vike_datahub_client::archive_fetch::{FetchDone, FetchSpec};
use vike_datahub_client::proto::Response;
use vike_store::HistoryStoreDataFusion;

/// The User-Agent the owner set for vike's own boxes (2026-11-16): the project's domain, no person.
const OUR_BOXES_AGENT: &str = "vike-trader research data@vike.io";

/// Apple Inc.: a company whose 8-K (2024-10-31) and 10-K (2024-11-01) sit on two consecutive days.
const CIK: &str = "0000320193";
/// 2024-10-31 and 2024-11-01, UTC.
const FROM: i64 = 1_730_332_800_000;
const TO: i64 = FROM + 86_400_000;
const DAY: i64 = 86_400_000;

fn fetched(response: Response) -> FetchDone {
    match response {
        Response::ArchiveFetched(done) => *done,
        other => panic!("not a download's answer: {other:?}"),
    }
}

fn imported(response: Response) -> ImportDone {
    match response {
        Response::ArchiveImported(done) => *done,
        other => panic!("not an import's answer: {other:?}"),
    }
}

fn import(format: &str, window: Option<(i64, i64)>, dry_run: bool) -> ImportSpec {
    ImportSpec {
        format: format.to_string(),
        dataset: CIK.to_string(),
        from_day: window.map(|w| w.0),
        to_day: window.map(|w| w.1),
        bars: Vec::new(),
        dry_run,
        verify: false,
        replace_holed: false,
        target: None,
    }
}

#[test]
#[ignore = "LIVE: a handful of requests to SEC EDGAR; run by hand from a lane"]
fn one_company_downloads_imports_and_reads_back_point_in_time() {
    let project = tempfile::tempdir().expect("a scratch project");
    let settings_dir = project.path().join("settings");
    vike_secrets::plant_settings_rows(
        &settings_dir,
        &vike_secrets::StoredSettings {
            settings: vec![vike_secrets::SettingRow {
                section: "config".to_string(),
                key: "datahub_sec_user_agent".to_string(),
                value: format!("\"{OUR_BOXES_AGENT}\""),
            }],
            ..Default::default()
        },
    )
    .expect("plant the scratch project's settings");
    // The boot's read: the rows, then the layers.
    let read = vike_secrets::read_settings_in(&settings_dir);
    let mut refusal = String::new();
    let source = vike_config::StoreLayer::of(Some(&read), &mut refusal);
    let settings = vike_config::load_with_source(Some(&settings_dir), source, &Default::default())
        .expect("load");
    let agent = settings.config.datahub_sec_user_agent.clone();
    assert_eq!(agent.as_deref(), Some(OUR_BOXES_AGENT), "the row, through the settings load");

    let store = Arc::new(
        HistoryStoreDataFusion::open(project.path().join("market_data/hist")).expect("a store"),
    );
    let imports = project.path().join("market_data/imports");
    let mut lane = ImportLane::new(imports.clone(), real_import_registry(Arc::clone(&store), None));
    for download in sec_downloads(agent, Arc::new(SecHttpUreq::default()), EDGAR_WWW, SEC_DATA) {
        lane = lane.with_download(Box::new(download));
    }
    let fetch = |format: &str| FetchSpec {
        format: format.to_string(),
        dataset: CIK.to_string(),
        from_day: FROM,
        to_day: TO,
        go: true,
        concurrency: 1,
    };

    // 1. The filings of two days, downloaded and imported.
    let done = fetched(fetch_archive_verb(&fetch("sec-filings"), Some(&lane), &|| false));
    println!("sec-filings plan: {:?}", done.plan);
    println!("sec-filings outcome: {:?}", done.fetched);
    let done = imported(import_archive_verb(
        &import("sec-filings", Some((FROM, TO)), false),
        Some(&lane),
        &|| false,
    ));
    for day in &done.outcome.as_ref().expect("an outcome").days {
        println!("sec-filings day {}: {:?}", day.day, day.result);
        assert!(matches!(day.result, ArchiveDayResultWire::Imported { .. }), "{:?}", day.result);
    }
    for kind in ["sec_8k", "sec_10k"] {
        let rows = store.scan_texts(kind, CIK, TimestampRange::all()).expect("read back");
        println!("{kind}: {} rows", rows.len());
        for row in &rows {
            let head: String = row.body.chars().take(120).collect();
            println!(
                "  {} received {} subject {:?} period {:?} title {:?} body {} bytes: {head:?}",
                row.source_id,
                row.received_ms,
                row.subject,
                row.period,
                row.title,
                row.body.len()
            );
        }
        assert!(!rows.is_empty(), "{kind} landed");
    }
    let known = store.texts_as_of("sec_10k", CIK, TO, 400 * DAY).expect("as of");
    println!("10-K rows known at 2024-11-01 00:00 UTC: {}", known.len());

    // 2. The index's `Z` time is New York's: the same instant as the header's, for every filing.
    let http = SecHttpUreq::default();
    let index = http
        .get(&format!("{SEC_DATA}/submissions/CIK{CIK}.json"), OUR_BOXES_AGENT, 64 << 20)
        .expect("the submissions index");
    let index = read_submissions(&index, CIK).expect("an index");
    for entry in std::fs::read_dir(imports.join("sec-filings").join(CIK)).expect("the day files") {
        let text = std::fs::read_to_string(entry.expect("an entry").path()).expect("a day file");
        for part in split_day_file(&text).expect("submissions") {
            let filing = parse_submission(part).expect("a filing");
            let listed =
                index.filings.iter().find(|f| f.accession == filing.accession).expect("indexed");
            println!(
                "{} {} about {} ({}): header {} index {}, {} documents",
                filing.accession,
                filing.form,
                filing.company_cik,
                filing.company_name,
                filing.accepted_ms,
                listed.accepted_ms,
                filing.documents.len()
            );
            assert_eq!(listed.accepted_ms, filing.accepted_ms, "{}", filing.accession);
        }
    }

    // 3. The XBRL values: downloaded, planned over every day, one month imported and read back.
    let done = fetched(fetch_archive_verb(&fetch("sec-companyfacts"), Some(&lane), &|| false));
    println!("sec-companyfacts outcome: {:?}", done.fetched);
    let plan = imported(import_archive_verb(
        &import("sec-companyfacts", None, true),
        Some(&lane),
        &|| false,
    ));
    println!("sec-companyfacts: {} days hold values", plan.plan.days.len());
    let november = (TO, TO + 29 * DAY);
    let done = imported(import_archive_verb(
        &import("sec-companyfacts", Some(november), false),
        Some(&lane),
        &|| false,
    ));
    println!("sec-companyfacts November: {:?}", done.outcome);
    let facts = store
        .scan_financial_facts("sec", CIK, TimestampRange::of(november.0, november.1 + DAY))
        .expect("read back");
    println!("financial facts knowable in November: {}", facts.len());
    for fact in facts
        .iter()
        .filter(|f| f.concept == "RevenueFromContractWithCustomerExcludingAssessedTax")
        .take(5)
    {
        println!("  {fact:?}");
    }
    assert!(!facts.is_empty());
}
