use super::{holdings_of, read_filings};
use crate::sec_edgar::end_of_filing_day_ms;
use crate::testing::sec_edgar::{FILER, form13f_zip, zip_of_members};

#[test]
fn the_filings_join_their_cover_pages_and_are_knowable_after_their_day() {
    let zip = form13f_zip();
    let filings = read_filings(&zip).expect("a data set");
    assert_eq!(filings.len(), 4);
    let report = &filings[0];
    assert_eq!(report.filer_cik, FILER, "the CIK is padded to ten digits");
    assert_eq!(report.filer_name, "EXAMPLE CAPITAL LLC");
    assert_eq!(report.period, "2023-12-31");
    assert_eq!(report.filed_ms, end_of_filing_day_ms(2024, 2, 14).unwrap());
    assert!(report.amendment_type.is_empty() && !report.value_in_thousands);
    assert_eq!(filings[1].amendment_type, "RESTATEMENT");
    assert!(filings[3].value_in_thousands, "filed before 2023-01-03");
}

#[test]
fn only_the_asked_filings_lines_are_kept_and_thousands_become_dollars() {
    let zip = form13f_zip();
    let filings = read_filings(&zip).expect("a data set");
    let asked: Vec<_> = [&filings[0], &filings[2], &filings[3]].into_iter().collect();
    let lines = holdings_of(&zip, &asked).expect("the information table");
    assert_eq!(lines.len(), 3, "the notice has no line and the amendment was not asked");
    assert_eq!(lines[0].cusip, "123456789");
    assert_eq!(lines[0].value_usd, 1_500_000_000);
    assert_eq!(lines[1].put_call, "Put");
    assert_eq!(lines[1].class_title, "CL A");
    assert_eq!(lines[2].value_usd, 1_200_000_000, "1,200,000 thousands");
    assert_eq!(lines[2].period, "2022-09-30");
    assert!(lines.iter().all(|l| l.filer_cik == FILER && l.shares_type == "SH"));
}

#[test]
fn a_missing_table_column_or_a_bad_number_refuses() {
    let no_table = zip_of_members(&[("SUBMISSION.tsv", b"ACCESSION_NUMBER\n")]);
    assert!(read_filings(&no_table).unwrap_err().contains("COVERPAGE.tsv"));
    let no_column = zip_of_members(&[
        ("SUBMISSION.tsv", b"ACCESSION_NUMBER\tFILING_DATE\n"),
        ("COVERPAGE.tsv", b"ACCESSION_NUMBER\tISAMENDMENT\tAMENDMENTTYPE\tFILINGMANAGER_NAME\n"),
    ]);
    assert!(read_filings(&no_column).unwrap_err().contains("SUBMISSIONTYPE"));
    let zip = form13f_zip();
    let filings = read_filings(&zip).expect("a data set");
    let bad = crate::testing::sec_edgar::FORM13F_INFOTABLE.replace("1500000000", "lots");
    let bad_zip = zip_of_members(&[("INFOTABLE.tsv", bad.as_bytes())]);
    let err = holdings_of(&bad_zip, &[&filings[0]]).unwrap_err();
    assert!(err.contains("INFOTABLE.tsv line 2") && err.contains("VALUE"), "{err}");
    assert!(read_filings(b"not a zip at all, just bytes").is_err());
}
