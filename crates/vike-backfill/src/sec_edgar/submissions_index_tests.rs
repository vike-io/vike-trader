use super::{is_page_name, read_older_page, read_submissions};
use crate::sec_edgar::submission::parse_submission;
use crate::testing::sec_edgar::{CIK, EIGHT_K, SUBMISSIONS, TEN_K};

#[test]
fn the_index_states_in_utc_the_instant_each_header_states_in_new_york_time() {
    let index = read_submissions(SUBMISSIONS.as_bytes(), CIK).expect("an index");
    assert_eq!(index.name, "EXAMPLE CORP");
    assert_eq!(index.filings.len(), 5);
    for (form, text) in [("10-K", TEN_K), ("8-K", EIGHT_K)] {
        let listed = index.filings.iter().find(|f| f.form == form).expect("listed");
        assert_eq!(
            listed.accepted_ms,
            parse_submission(text).expect("its submission").accepted_ms,
            "{form}: the index's UTC time and the header's New York time are one instant"
        );
    }
    let ten_k = index.filings.iter().find(|f| f.form == "10-K").expect("the 10-K");
    assert_eq!(ten_k.bytes, 9_876_543);
    assert_eq!(index.older_pages.len(), 1, "a page name that is not the company's is dropped");
    assert_eq!(index.older_pages[0].name, "CIK0001234567-submissions-001.json");
    assert_eq!(index.older_pages[0].filing_to, "2015-06-30");
}

#[test]
fn an_older_page_reads_the_same_columns_and_names_are_held_to_the_grammar() {
    let page = r#"{"accessionNumber":["0001234567-05-000001","0001234567-05-000002"],"form":["10-K","8-K"],
"acceptanceDateTime":["2005-07-01T12:00:00.000Z","2005-07-01T12:00:00"],"size":[100,200]}"#;
    let filings = read_older_page(page.as_bytes()).expect("a page");
    let noon = 1_120_176_000_000 + 12 * 3_600_000;
    assert_eq!(
        (filings[0].accepted_ms, filings[1].accepted_ms),
        (noon, noon),
        "UTC, with or without Z"
    );
    let offset = page.replace("12:00:00\"]", "12:00:00-04:00\"]");
    assert!(read_older_page(offset.as_bytes()).is_err(), "another offset is never guessed");
    assert!(is_page_name("CIK0001234567-submissions-002.json", CIK));
    assert!(!is_page_name("CIK0001234567-submissions-.json", CIK));
    assert!(!is_page_name("CIK0001234568-submissions-001.json", CIK));
    assert!(read_submissions(SUBMISSIONS.as_bytes(), "0000000001").is_err());
    let short = r#"{"accessionNumber":["0001234567-05-000001"],"form":[],"acceptanceDateTime":[]}"#;
    assert!(read_older_page(short.as_bytes()).unwrap_err().contains("same length"));
}
