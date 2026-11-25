use vike_data::sec_log::SEC_VENUE;
use vike_data::{HistoryStore, TimestampRange};
use vike_store::HistoryStoreDataFusion;

use super::{
    append_facts_day, append_filing_day, append_holdings_day, held_facts_days, held_filing_days,
    held_holdings_days, original_of,
};
use crate::sec_edgar::companyfacts::financial_facts;
use crate::sec_edgar::form13f::{holdings_of, read_filings};
use crate::sec_edgar::submission::parse_submission;
use crate::sec_edgar::texts::filing_text_rows;
use crate::testing::sec_edgar::{
    CIK, COMPANYFACTS, EIGHT_K, FILER, SCHEDULE_13G, TEN_K, form13f_zip,
};

#[test]
fn a_filing_day_is_one_commit_per_kind_and_a_repeat_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let mut rows = filing_text_rows(&parse_submission(EIGHT_K).unwrap(), "");
    rows.extend(filing_text_rows(&parse_submission(SCHEDULE_13G).unwrap(), ""));
    let day = rows[0].received_ms;
    assert_eq!(append_filing_day(&store, CIK, day, &rows).unwrap(), 4, "three 8-K rows, one 13G");
    assert_eq!(append_filing_day(&store, CIK, day, &rows).unwrap(), 0, "the keys are spent");
    assert_eq!(
        held_filing_days(&store, CIK).unwrap().into_iter().collect::<Vec<_>>(),
        ["2024-02-01"]
    );
    let eight_k = store.texts_as_of("sec_8k", CIK, day, 0).unwrap();
    assert_eq!(eight_k.len(), 3, "the point-in-time read at the acceptance instant sees the 8-K");
    assert!(store.texts_as_of("sec_8k", CIK, day - 1, 86_400_000).unwrap().is_empty());
    assert_eq!(store.scan_texts("sec_13g", CIK, TimestampRange::all()).unwrap().len(), 1);
}

#[test]
fn an_amendment_finds_its_stored_original() {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let original = parse_submission(TEN_K).unwrap();
    assert_eq!(
        original_of(&store, "sec_10k", CIK, "2023-09-30", original.accepted_ms).unwrap(),
        None
    );
    let rows = filing_text_rows(&original, "");
    append_filing_day(&store, CIK, original.accepted_ms, &rows).unwrap();
    let later = original.accepted_ms + 60 * 86_400_000;
    assert_eq!(
        original_of(&store, "sec_10k", CIK, "2023-09-30", later).unwrap().as_deref(),
        Some("0001234567-23-000106")
    );
    assert_eq!(original_of(&store, "sec_10k", CIK, "2022-09-30", later).unwrap(), None);
    assert_eq!(original_of(&store, "sec_10k", CIK, "", later).unwrap(), None);
}

#[test]
fn table_days_are_held_per_company_and_per_manager() {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let facts = financial_facts(COMPANYFACTS.as_bytes(), CIK).unwrap();
    let first_day = facts[0].filed_ms;
    let day_rows: Vec<_> = facts.iter().filter(|f| f.filed_ms == first_day).cloned().collect();
    assert_eq!(append_facts_day(&store, CIK, first_day, &day_rows).unwrap(), 2);
    assert_eq!(
        held_facts_days(&store, CIK).unwrap().into_iter().collect::<Vec<_>>(),
        ["2023-11-04"]
    );

    let zip = form13f_zip();
    let filings = read_filings(&zip).unwrap();
    let lines = holdings_of(&zip, &[&filings[0]]).unwrap();
    assert_eq!(append_holdings_day(&store, filings[0].day(), &lines).unwrap(), 2);
    assert_eq!(append_holdings_day(&store, filings[0].day(), &lines).unwrap(), 0);
    assert_eq!(
        held_holdings_days(&store, FILER).unwrap().into_iter().collect::<Vec<_>>(),
        ["2024-02-15"]
    );
    assert_eq!(
        store.institutional_holdings_as_of(SEC_VENUE, FILER, filings[0].filed_ms, 0).unwrap(),
        lines
    );
}
