//! The user lane: the file held to the operator's interval, the label's grammar, and an import over a
//! real store that lands in the lane and nowhere else.

use vike_data::HistoryStore;

use super::*;
use crate::archive_import::{ArchiveDayClass, ArchiveDayResult, ArchiveImport};

/// 2024-01-15, a UTC midnight.
const DAY: i64 = 19_737 * MS_PER_DAY;
const HOUR: i64 = 3_600_000;
const LATER: i64 = DAY + 400 * MS_PER_DAY;

fn new_lane(interval: &str, label: &str) -> Result<ArchiveLaneUserBars, String> {
    ArchiveLaneUserBars::new(
        "binance",
        "BTCUSDT",
        interval,
        label,
        CsvBarsFormat::default(),
        CsvBarsColumns::Header,
    )
}

fn lane() -> ArchiveLaneUserBars {
    new_lane("1h", "my-file").unwrap()
}

/// `hours` one-hour bars from `start`, in milliseconds, under a header.
fn csv(start: i64, hours: i64) -> String {
    let rows: String =
        (0..hours).map(|h| format!("{},10,20,5,15,1.5\n", start + h * HOUR)).collect();
    format!("time,open,high,low,close,volume\n{rows}")
}

#[test]
fn a_file_is_held_to_the_operators_interval_and_its_grid() {
    let bars = lane().decode(&csv(DAY, 24)).unwrap();
    assert_eq!(bars.len(), 24);
    assert_eq!((bars[0].timestamp, bars[0].close, bars[0].volume), (DAY, 15.0, 1.5));
    let at_1m = new_lane("1m", "my-file").unwrap();
    assert!(at_1m.decode(&csv(DAY, 3)).unwrap_err().contains("--interval 1m"));
    let off_grid = lane().decode(&csv(DAY + 60_000, 3)).unwrap_err();
    assert!(off_grid.contains("off the UTC 1h grid"), "{off_grid}");
    let bad = lane().decode("time,open,high,low,close,volume\n1705276800000,10,1,5,15,1\n");
    assert!(bad.unwrap_err().contains("line 2"), "the decoder's refusal names the line");
}

/// A regional export reads with the operator's format words and only with them: the same bars as
/// the plain file, its Moscow wall clock turned into UTC.
#[test]
fn a_regional_file_reads_with_its_format_words_and_is_refused_without_them() {
    let regional = "time;open;high;low;close;volume\n\
                    15.01.2024 03:00;10,5;20;5;15;1 500,25\n\
                    15.01.2024 04:00;10,5;20;5;15;1 500,25\n";
    let words = vike_data::csv_bars::CsvBarsFormatWords {
        delimiter: Some(";".into()),
        decimal: Some(",".into()),
        thousands: Some("space".into()),
        time: Some("DD.MM.YYYY HH:mm".into()),
        tz: Some("+03:00".into()),
    };
    let format = CsvBarsFormat::parse(&words).unwrap();
    let ru =
        ArchiveLaneUserBars::new("binance", "BTCUSDT", "1h", "ru", format, CsvBarsColumns::Header)
            .unwrap();
    let bars = ru.decode(regional).unwrap();
    assert_eq!(
        (bars[0].timestamp, bars[1].timestamp),
        (DAY, DAY + HOUR),
        "03:00 at +03:00 is midnight UTC"
    );
    assert_eq!((bars[0].open, bars[0].volume), (10.5, 1500.25));
    assert!(lane().decode(regional).is_err(), "the plain format reads no regional file");
}

#[test]
fn a_label_the_lane_grammar_refuses_and_an_interval_a_day_cannot_hold_are_refused() {
    for label in ["My-File", "", "a/b", "my file"] {
        assert!(new_lane("1h", label).is_err(), "{label:?}");
    }
    for interval in ["7m", "1w", "01h"] {
        assert!(new_lane(interval, "ok").is_err(), "{interval}");
    }
}

/// The day lands in the lane, and the venue's own series stays empty; a second run reads no file.
#[test]
fn an_imported_day_lands_in_the_lane_alone() {
    let dir = tempfile::tempdir().unwrap();
    let hist = HistoryStoreDataFusion::open(dir.path().join("hist")).unwrap();
    let bars = lane().decode(&csv(DAY, 24)).unwrap();
    let mut import = ArchiveImport::open(&hist, lane(), &[], LATER).unwrap();
    assert_eq!(import.classify(DAY), ArchiveDayClass::Free);
    let result = import.import_day(DAY, || Ok(bars.clone())).unwrap();
    assert_eq!(result, ArchiveDayResult::Imported { ticks: 24, bars: vec![] });
    drop(import);

    let range = TimestampRange::of(DAY, DAY + MS_PER_DAY - 1);
    let laned =
        hist.load_bars_sourced_capped("binance", lane().lane(), "BTCUSDT", "1h", range, None);
    assert_eq!(laned.unwrap(), bars);
    assert!(hist.load_bars("binance", "BTCUSDT", "1h", range).unwrap().is_empty(), "no candle");

    let mut again = ArchiveImport::open(&hist, lane(), &[], LATER).unwrap();
    assert_eq!(again.classify(DAY), ArchiveDayClass::HeldByArchive);
    let repeat = again.import_day(DAY, || panic!("a held day's file is never read")).unwrap();
    assert_eq!(repeat, ArchiveDayResult::ToppedUp { bars: vec![] });
}

#[test]
fn a_day_that_has_not_ended_is_too_recent() {
    let table = lane().owners(&[]);
    assert_eq!(table.classify(DAY, DAY + MS_PER_DAY - 1), ArchiveDayClass::TooRecent);
    assert_eq!(table.classify(DAY, DAY + MS_PER_DAY), ArchiveDayClass::Free);
}
