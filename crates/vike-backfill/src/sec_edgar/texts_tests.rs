use super::{filing_text_rows, index_url};
use crate::sec_edgar::submission::parse_submission;
use crate::testing::sec_edgar::{CIK, EIGHT_K, SCHEDULE_13G, TEN_K, TEN_K_AMENDMENT};

#[test]
fn a_10k_is_one_row_per_section_received_at_its_acceptance() {
    let filing = parse_submission(TEN_K).expect("a 10-K");
    let rows = filing_text_rows(&filing, "");
    let ids: Vec<&str> = rows.iter().map(|r| r.source_id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "0001234567-23-000106/item_1",
            "0001234567-23-000106/item_1a",
            "0001234567-23-000106/item_7",
            "0001234567-23-000106/item_16"
        ],
        "the contents lines are not sections; the exhibit 21 and the graphic are skipped"
    );
    assert_eq!(rows[3].body, "Item 16. Form 10-K Summary\nNone.", "the body's, not the preamble");
    let risk = &rows[1];
    assert_eq!(risk.kind, "sec_10k");
    assert_eq!(risk.source, "EDGAR");
    assert_eq!((risk.received_ms, risk.published_ms), (filing.accepted_ms, filing.accepted_ms));
    assert_eq!(risk.subject, format!("{CIK} EXMP"), "the CIK, then the symbol the cover tags");
    assert_eq!(risk.period, "2023-09-30");
    assert_eq!(risk.title, "Item 1A. Risk Factors");
    assert!(risk.body.starts_with("Item 1A. Risk Factors\nThe Company\u{2019}s business"));
    assert_eq!(risk.tags, "10-K");
    assert_eq!(risk.url, index_url(CIK, "0001234567-23-000106"));
    assert_eq!(
        risk.url,
        "https://www.sec.gov/Archives/edgar/data/1234567/000123456723000106/0001234567-23-000106-index.htm"
    );
    assert!(risk.amends.is_empty());
}

#[test]
fn an_8k_keeps_its_items_and_its_press_release() {
    let rows = filing_text_rows(&parse_submission(EIGHT_K).expect("an 8-K"), "");
    let keys: Vec<&str> =
        rows.iter().map(|r| r.source_id.rsplit('/').next().unwrap_or("")).collect();
    assert_eq!(keys, ["item_2_02", "item_9_01", "ex_99_1"]);
    assert_eq!(rows[2].title, "EX-99.1", "a description that only repeats the type is not doubled");
    assert_eq!(
        rows[2].body,
        "Example Corp Reports First Quarter Results\nRevenue of $1.0 billion."
    );
    assert!(
        rows.iter().all(|r| r.kind == "sec_8k" && r.subject == CIK),
        "no inline XBRL, no symbol"
    );
}

#[test]
fn an_amendment_names_the_same_section_of_its_original_and_a_schedule_is_one_document() {
    let rows = filing_text_rows(
        &parse_submission(TEN_K_AMENDMENT).expect("a 10-K/A"),
        "0001234567-23-000106",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, "sec_10k", "an amendment shares its form's kind");
    assert_eq!(rows[0].amends, "0001234567-23-000106/item_1a");
    assert_eq!(rows[0].tags, "10-K/A");
    let schedule = filing_text_rows(&parse_submission(SCHEDULE_13G).expect("a 13G"), "");
    assert_eq!(schedule.len(), 1);
    assert_eq!(schedule[0].kind, "sec_13g");
    assert_eq!(schedule[0].source_id, "0009876543-24-000001/document");
    assert_eq!(schedule[0].title, "EXAMPLE CORP SC 13G");
    assert!(schedule[0].body.contains("6.1% of the common stock"));
}

#[test]
fn a_form_the_history_does_not_keep_gives_no_row() {
    let form_4 = TEN_K.replace("CONFORMED SUBMISSION TYPE:\t10-K", "CONFORMED SUBMISSION TYPE:\t4");
    assert!(filing_text_rows(&parse_submission(&form_4).expect("a form 4"), "").is_empty());
}
