//! The hourly summary's decode over rows copied from the vendor's `BTCUSDT-EOHSummary-2023-10-23.csv`,
//! its refusals, and the day over a real store read back point-in-time.

use vike_data::{HistoryStore, TimestampRange};
use vike_marketdata::OptionKind;
use vike_model::MS_PER_DAY;

use super::*;
use crate::archive_import::{ArchiveDayClass, ArchiveDayResult, ArchiveImport};
use crate::testing::zip_of;
use crate::venues::binance_archive::dataset_zip_day;

const UNDERLYING: &str = "BTCUSDT";
/// 2023-10-23, a UTC midnight.
const DAY: i64 = 19_653 * MS_PER_DAY;
const LABEL: &str = "2023-10-23";
const LATER: i64 = DAY + 400 * MS_PER_DAY;

/// Hour 01 first: the vendor's rows are not in time order.
const ROWS: &str = "\
2023-10-23,01,BTC-231229-34000-C,BTCUSDT,C,231229-34000,725.00000000,725.00000000,725.00000000,725.00000000,0E-8,0E-8,1430.00000000,1460.00000000,2.83000000,2.35000000,\"\",0.53434968,1444.90000000,0.53123981,0.34994338,0.00005354,48.23448652,-19.05135683,4.98000000,150940.70036045
2023-10-23,00,BTC-231027-33000-C,BTCUSDT,C,231027-33000,75.00000000,100.00000000,65.00000000,90.00000000,29.71000000,29.71000000,80.00000000,90.00000000,8.71000000,16.48000000,\"\",0.62905252,84.90000000,0.61981410,0.09250348,0.00008187,5.41260825,-39.08507011,44.79000000,1349056.18015695
2023-10-23,03,BTC-231027-28500-P,BTCUSDT,P,231027-28500,0E-8,0E-8,0E-8,0E-8,0E-8,0E-8,0E-8,0E-8,0E-8,0E-8,\"\",-1E-8,241.40000000,0.77500000,-0.17065604,0.00009969,8.32929667,-77.46224383,\"\",\"\"
2023-10-23,23,BTC-231027-24000-P,BTCUSDT,P,231027-24000,5.00000000,15.00000000,5.00000000,15.00000000,2.48000000,2.48000000,5.00000000,25.00000000,4.07000000,3.80000000,\"\",1.57340467,7.00000000,1.34410985,-0.00523087,0.00000354,0.47571893,-9.59124420,240.62000000,7955548.09032207
";

fn member() -> String {
    dataset_member_name(UNDERLYING, FILE_WORD, DAY)
}

fn file(body: &str) -> Vec<u8> {
    zip_of(&member(), format!("{HEADER_LINE}\n{body}").as_bytes())
}

fn decode(body: &str) -> Result<Vec<ChainRow>, ArchiveDayRefusal> {
    decode_summary_zip(&file(body), UNDERLYING, DAY)
}

fn class_of(body: &str) -> String {
    decode(body).expect_err("this file must be refused").class
}

const HOUR: i64 = 3_600_000;

#[test]
fn only_a_daily_summary_name_is_a_day() {
    assert_eq!(dataset_zip_day("BTCUSDT-EOHSummary-2023-10-23.zip", FILE_WORD), Some(DAY));
    for other in [
        "BTCUSDT-EOHSummary-2023-10.zip",
        "BTCUSDT-EOHSummary-2023-10-23.zip.CHECKSUM",
        "BTCUSDT-BVOLIndex-2023-10-23.zip",
        "-EOHSummary-2023-10-23.zip",
    ] {
        assert_eq!(dataset_zip_day(other, FILE_WORD), None, "{other}");
    }
}

/// Every column the row keeps, read as the module doc says: the hour's LAST millisecond, the expiry
/// at 08:00 UTC of the strike's date, a zero quote absent, an empty open interest absent, the venue's
/// greeks as published; the day sorted by time.
#[test]
fn a_day_decodes_to_chain_rows_stamped_at_the_end_of_their_hour() {
    let rows = decode(ROWS).unwrap();
    assert_eq!(
        rows.iter().map(|r| r.ts).collect::<Vec<_>>(),
        vec![DAY + HOUR - 1, DAY + 2 * HOUR - 1, DAY + 4 * HOUR - 1, DAY + MS_PER_DAY - 1,]
    );
    let call = &rows[0];
    assert_eq!(call.instrument, "BTC-231027-33000-C");
    assert_eq!(call.underlying, UNDERLYING);
    assert_eq!(call.kind, OptionKind::Call);
    assert_eq!(call.strike, 33_000.0);
    // 2023-10-27 08:00 UTC.
    assert_eq!(call.expiry_ms, (19_657 * MS_PER_DAY) + 8 * HOUR);
    assert_eq!((call.bid, call.ask, call.mark), (Some(80.0), Some(90.0), Some(84.9)));
    assert_eq!(call.iv, Some(0.619_814_1));
    assert_eq!((call.volume, call.open_interest), (Some(29.71), Some(44.79)));
    assert_eq!(call.delta, Some(0.092_503_48));
    assert_eq!(call.gamma, Some(0.000_081_87));
    assert_eq!(call.vega, Some(5.412_608_25));
    assert_eq!(call.theta, Some(-39.085_070_11));

    let quoteless = &rows[2];
    assert_eq!(quoteless.instrument, "BTC-231027-28500-P");
    assert_eq!(quoteless.kind, OptionKind::Put);
    assert_eq!((quoteless.bid, quoteless.ask), (None, None), "a zero side is absent, never 0");
    assert_eq!(quoteless.volume, Some(0.0), "a zero volume is a real zero");
    assert_eq!(quoteless.open_interest, None, "an empty field is absent");
    assert_eq!(quoteless.mark, Some(241.4));

    assert!(decode("").unwrap().is_empty(), "a header alone holds no row");
}

#[test]
fn a_row_that_does_not_belong_refuses_the_day() {
    let first = ROWS.lines().nth(1).unwrap();
    let other_day = first.replacen("2023-10-23", "2023-10-24", 1);
    assert_eq!(class_of(&other_day), "OutsideDay");
    for bad in [
        first.replacen(",BTCUSDT,", ",ETHUSDT,", 1),
        first.replacen(",00,", ",24,", 1),
        first.replacen("231027-33000,", "231027-34000,", 1),
        first.replacen(",C,", ",X,", 1),
        // A date that does not exist, spelled alike in the symbol and the strike.
        first.replace("231027-33000", "231332-33000"),
        first.replacen("84.90000000", "NaN", 1),
        first.replacen("80.00000000", "-80", 1),
        format!("{first},extra"),
        "date,hour".to_string(),
    ] {
        assert_eq!(class_of(&bad), "BadRow", "{bad}");
    }
}

#[test]
fn an_exact_repeat_is_dropped_and_a_contradictory_one_refuses() {
    let twice = format!("{ROWS}{ROWS}");
    assert_eq!(decode(&twice).unwrap(), decode(ROWS).unwrap());
    let first = ROWS.lines().nth(1).unwrap();
    let changed = first.replacen("84.90000000", "85.00000000", 1);
    assert_eq!(class_of(&format!("{first}\n{changed}\n")), "ContradictoryRepeat");
}

#[test]
fn another_underlyings_file_is_refused_by_its_member() {
    let other = zip_of("ETHUSDT-EOHSummary-2023-10-23.csv", ROWS.as_bytes());
    let refusal = decode_summary_zip(&other, UNDERLYING, DAY).unwrap_err();
    assert_eq!(refusal.class, "ForeignMember");
}

#[test]
fn a_dataset_is_the_vendors_upper_case_folder_name() {
    assert!(OptionSummaryLane::for_dataset("BTCUSDT").is_ok());
    for bad in ["BTCUSDT.P", "btcusdt", "BTC-USDT", ""] {
        assert!(OptionSummaryLane::for_dataset(bad).is_err(), "{bad}");
    }
}

// ---- over a real store ----------------------------------------------------------------------

fn store() -> (tempfile::TempDir, HistoryStoreDataFusion) {
    let dir = tempfile::tempdir().unwrap();
    let hist = HistoryStoreDataFusion::open(dir.path().join("hist")).unwrap();
    (dir, hist)
}

fn lane() -> OptionSummaryLane {
    OptionSummaryLane::for_dataset(UNDERLYING).unwrap()
}

/// A day lands once under its key, reads back as decoded, a second session holds it without
/// reading its file, and a point-in-time read between two hours answers the earlier one.
#[test]
fn a_day_lands_once_and_reads_back_point_in_time() {
    let (_dir, hist) = store();
    let mut first = ArchiveImport::open(&hist, lane(), &[], LATER).unwrap();
    assert_eq!(first.classify(DAY), ArchiveDayClass::Free);
    let result = first.import_day(DAY, || decode(ROWS)).unwrap();
    assert_eq!(result, ArchiveDayResult::Imported { ticks: 4, bars: vec![] });
    drop(first);

    let back = hist.scan_chain(VENUE, UNDERLYING, TimestampRange::all()).unwrap();
    assert_eq!(back, decode(ROWS).unwrap());
    let keys = hist.series_commits(&chain_series(UNDERLYING)).unwrap();
    assert_eq!(keys, vec![day_commit_key(UNDERLYING, LABEL)]);

    let mut second = ArchiveImport::open(&hist, lane(), &[], LATER).unwrap();
    assert_eq!(second.classify(DAY), ArchiveDayClass::HeldByArchive);
    let again = second.import_day(DAY, || panic!("a held day's file is never read")).unwrap();
    assert_eq!(again, ArchiveDayResult::ToppedUp { bars: vec![] });

    // At 01:30 the hour 00 summary is known and the hour 01 one is not.
    let at = hist.chain_as_of_within(VENUE, UNDERLYING, DAY + HOUR + HOUR / 2, 2 * HOUR).unwrap();
    assert_eq!(
        at.iter().map(|r| r.instrument.as_str()).collect::<Vec<_>>(),
        vec!["BTC-231027-33000-C"]
    );
}

/// A key this lane did not write meets every day: the day is refused, never stored twice.
#[test]
fn another_writers_key_overlaps_every_day() {
    let owners = lane().owners(&["chain:binance:BTCUSDT:0:1".to_string()]);
    assert!(matches!(owners.classify(DAY, LATER), ArchiveDayClass::Overlapped { .. }));
    let own = lane().owners(&[day_commit_key(UNDERLYING, LABEL)]);
    assert_eq!(own.classify(DAY, LATER), ArchiveDayClass::HeldByArchive);
    assert_eq!(own.classify(DAY + MS_PER_DAY, LATER), ArchiveDayClass::Free);
}
