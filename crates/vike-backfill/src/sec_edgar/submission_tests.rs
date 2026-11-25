use super::{parse_submission, split_day_file};
use crate::testing::sec_edgar::{CIK, EIGHT_K, FILER, SCHEDULE_13G, TEN_K};

#[test]
fn a_10k_header_and_its_documents_are_read() {
    let filing = parse_submission(TEN_K).expect("a 10-K");
    assert_eq!(filing.accession, "0001234567-23-000106");
    assert_eq!(filing.form, "10-K");
    assert_eq!(filing.accepted_ms, 1_698_883_200_000 + 22 * 3_600_000 + 8 * 60_000 + 27_000);
    assert_eq!(filing.period, "2023-09-30");
    assert_eq!((filing.company_cik.as_str(), filing.company_name.as_str()), (CIK, "EXAMPLE CORP"));
    let types: Vec<&str> = filing.documents.iter().map(|d| d.doc_type.as_str()).collect();
    assert_eq!(types, ["10-K", "EX-21.1", "GRAPHIC"]);
    assert_eq!(filing.documents[1].description, "SUBSIDIARIES");
    assert!(filing.documents[0].text.contains("dei:TradingSymbol"));
}

#[test]
fn a_schedule_is_about_its_subject_company_not_its_filer() {
    let filing = parse_submission(SCHEDULE_13G).expect("a 13G");
    assert_eq!(filing.company_cik, CIK);
    assert_ne!(filing.company_cik, FILER);
    assert_eq!(filing.period, "", "a schedule states no period");
}

#[test]
fn a_day_file_is_its_submissions_in_order_and_a_cut_one_is_refused() {
    let day = format!("{EIGHT_K}\n{SCHEDULE_13G}");
    let parts = split_day_file(&day).expect("two submissions");
    assert_eq!(parts.len(), 2);
    assert_eq!(parse_submission(parts[1]).expect("the second").form, "SC 13G");
    let cut = &EIGHT_K[..EIGHT_K.len() - 20];
    assert!(split_day_file(cut).unwrap_err().contains("never closes"));
    assert!(split_day_file("hello").unwrap_err().contains("no EDGAR complete submission"));
}

#[test]
fn a_header_missing_what_it_must_state_is_refused() {
    let no_time = TEN_K.replace("<ACCEPTANCE-DATETIME>20231102180827\n", "");
    assert!(parse_submission(&no_time).unwrap_err().contains("ACCEPTANCE-DATETIME"));
    let bad_accession =
        TEN_K.replace("ACCESSION NUMBER:\t\t0001234567-23-000106", "ACCESSION NUMBER:\tX");
    assert!(parse_submission(&bad_accession).unwrap_err().contains("ACCESSION NUMBER"));
    let no_form = TEN_K.replace("CONFORMED SUBMISSION TYPE:\t10-K\n", "");
    assert!(parse_submission(&no_form).unwrap_err().contains("CONFORMED SUBMISSION TYPE"));
}
