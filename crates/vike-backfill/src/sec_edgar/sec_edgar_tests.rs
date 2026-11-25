use super::{cik_of_dataset, end_of_filing_day_ms, is_accession, new_york_to_utc_ms, parse_date};

const HOUR: i64 = 3_600_000;

/// 2023-11-02 00:00 UTC.
const NOV_2_2023: i64 = 1_698_883_200_000;

#[test]
fn new_york_time_follows_the_us_rule_from_2007_and_reads_standard_time_before() {
    // 2023-11-02 is daylight time (it ends 2023-11-05): 18:08:27 EDT is 22:08:27 UTC.
    assert_eq!(
        new_york_to_utc_ms(2023, 11, 2, 18, 8, 27),
        Some(NOV_2_2023 + 22 * HOUR + 8 * 60_000 + 27_000)
    );
    // 2024-02-01 is standard time: 16:30:12 EST is 21:30:12 UTC.
    let feb_1_2024 = 1_706_745_600_000;
    assert_eq!(
        new_york_to_utc_ms(2024, 2, 1, 16, 30, 12),
        Some(feb_1_2024 + 21 * HOUR + 30 * 60_000 + 12_000)
    );
    // 2005-07-01 was daylight time under the old rule; read as standard time, an hour LATE, never early.
    let jul_1_2005 = 1_120_176_000_000;
    assert_eq!(new_york_to_utc_ms(2005, 7, 1, 12, 0, 0), Some(jul_1_2005 + 17 * HOUR));
    assert_eq!(new_york_to_utc_ms(2023, 2, 30, 12, 0, 0), None, "no such day");
    assert_eq!(new_york_to_utc_ms(2023, 1, 1, 24, 0, 0), None, "no such hour");
}

#[test]
fn a_filing_date_is_knowable_at_03_00_utc_the_next_day() {
    assert_eq!(end_of_filing_day_ms(2023, 11, 2), Some(NOV_2_2023 + 27 * HOUR));
    // 22:00 EDT is 02:00 UTC and 22:00 EST is 03:00 UTC: never before EDGAR's close under either clock.
    assert!(end_of_filing_day_ms(2023, 7, 3).is_some_and(|t| t % 86_400_000 == 3 * HOUR));
    assert_eq!(vike_model::time::epoch_ms_to_utc_date(NOV_2_2023 + 27 * HOUR), "2023-11-03");
    assert_eq!(end_of_filing_day_ms(2023, 13, 1), None);
}

#[test]
fn dates_ciks_and_accessions_are_read_exactly() {
    assert_eq!(parse_date("2023-09-30"), Some((2023, 9, 30)));
    assert_eq!(parse_date("20230930"), Some((2023, 9, 30)));
    assert_eq!(parse_date("2023-02-29"), None);
    assert_eq!(parse_date("2023-9-30"), None);
    assert_eq!(cik_of_dataset("sec-filings", "0000320193").as_deref(), Ok("0000320193"));
    assert!(cik_of_dataset("sec-filings", "320193").is_err(), "zero-padded, as EDGAR spells it");
    assert!(cik_of_dataset("sec-filings", "AAPL").is_err());
    assert!(is_accession("0000320193-23-000106"));
    assert!(!is_accession("0000320193-23-00010"));
    assert!(!is_accession("000032019323000106"));
}
