//! The user lane: the file held to the operator's interval, the label's grammar, and an import over a
//! real store that lands in the lane and nowhere else.

use vike_data::HistoryStore;

use super::*;
use crate::archive_import::{ArchiveDayClass, ArchiveDayResult, ArchiveImport};

/// 2024-01-15, a UTC midnight.
const DAY: i64 = 19_737 * MS_PER_DAY;
const HOUR: i64 = 3_600_000;
const LATER: i64 = DAY + 400 * MS_PER_DAY;

/// The lane at `interval`: `sparse` is `--sparse`, `coarse` `--allow-coarse-grid`.
fn new_lane(
    interval: &str,
    label: &str,
    sparse: bool,
    coarse: bool,
) -> Result<ArchiveLaneUserBars, String> {
    let (format, columns) = (CsvBarsFormat::default(), CsvBarsColumns::Header);
    ArchiveLaneUserBars::new("binance", "BTCUSDT", interval, label, format, columns, sparse, coarse)
}

fn lane() -> ArchiveLaneUserBars {
    new_lane("1h", "my-file", false, false).unwrap()
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
    let off_grid = lane().decode(&csv(DAY + 60_000, 3)).unwrap_err();
    assert!(off_grid.contains("off the UTC 1h grid"), "{off_grid}");
    let bad = lane().decode("time,open,high,low,close,volume\n1705276800000,10,1,5,15,1\n");
    assert!(bad.unwrap_err().contains("line 2"), "the decoder's refusal names the line");
}

/// One-minute bars at the minutes `minutes` from `DAY`, in milliseconds, under a header.
fn minutes(minutes: &[i64]) -> String {
    let rows: String =
        minutes.iter().map(|m| format!("{},10,20,5,15,1\n", DAY + m * 60_000)).collect();
    format!("time,open,high,low,close,volume\n{rows}")
}

/// A sparse one-minute signal file's minutes: irregular gaps, thirteen rows, not all on any coarser grid.
const SIGNALS: [i64; 13] = [3, 4, 9, 17, 42, 43, 44, 120, 301, 302, 777, 1_001, 1_439];

/// **A file is dense unless it says `--sparse`** (decision 0271's amendment of 2026-11-18): dense 1m
/// data imports; 5-minute bars, or hourly ones, at `--interval 1m` are refused naming both ways out,
/// and so is a sparse file sent without the flag.
#[test]
fn a_dense_file_is_held_to_its_step_and_a_wrong_interval_is_refused_naming_both_fixes() {
    let dense = new_lane("1m", "prices", false, false).unwrap();
    let every: Vec<i64> = (0..60).collect();
    assert_eq!(dense.decode(&minutes(&every)).unwrap().len(), 60, "dense 1m data imports");
    let fives: Vec<i64> = (0..24).map(|k| k * 5).collect();
    assert_eq!(
        dense.decode(&minutes(&fives)).unwrap_err(),
        "rows are 5m apart but --interval is 1m; if this file holds only events, add --sparse"
    );
    let hourly = dense.decode(&csv(DAY, 3)).unwrap_err();
    assert!(hourly.starts_with("rows are 1h apart but --interval is 1m"), "{hourly}");
    // 2m and 3m apart: the commonest is 2m, and 3m is a second interval.
    let events = dense.decode(&minutes(&[0, 2, 5, 77, 1_439])).unwrap_err();
    assert!(
        events.contains("second interval")
            && events.ends_with("if this file holds only events, add --sparse"),
        "an event file needs the flag: {events}"
    );
}

/// **A sparse file imports with irregular gaps**, every row on the interval's grid; one whose every row
/// (more than `COARSE_GRID_ROWS`) lies on a coarser grid is that grid's data and refused, `--sparse` or
/// not; a few events on round minutes are not. Still refused: a row off the grid, a time that does not
/// increase, a malformed row.
#[test]
fn a_sparse_file_on_the_grid_imports_with_irregular_gaps() {
    let at_1m = new_lane("1m", "signals", true, false).unwrap();
    let bars = at_1m.decode(&minutes(&SIGNALS)).unwrap();
    let times: Vec<i64> = bars.iter().map(|b| (b.timestamp - DAY) / 60_000).collect();
    assert_eq!(times, SIGNALS.to_vec());
    let fives: Vec<i64> = (0..24).map(|k| k * 5).collect();
    assert_eq!(
        at_1m.decode(&minutes(&fives)).unwrap_err(),
        "all 24 rows lie on the 5-minute grid: is this 5-minute data? use --interval 5m, or add \
         --allow-coarse-grid if these are events at round times"
    );
    let quarter: Vec<i64> = (0..11).map(|k| k * 45).collect();
    let err = at_1m.decode(&minutes(&quarter)).unwrap_err();
    assert!(err.starts_with("all 11 rows lie on the 15-minute grid"), "the coarsest: {err}");
    let ten = &fives[..COARSE_GRID_ROWS];
    assert_eq!(at_1m.decode(&minutes(ten)).unwrap().len(), 10, "at most COARSE_GRID_ROWS pass");
    assert_eq!(at_1m.decode(&minutes(&[0, 30, 60])).unwrap().len(), 3, "a 3-row event file");

    let half =
        format!("time,open,high,low,close,volume\n{DAY},1,1,1,1,1\n{},1,1,1,1,1\n", DAY + 30_000);
    assert!(at_1m.decode(&half).unwrap_err().contains("off the UTC 1m grid"));
    for rows in [[5, 2], [2, 2]] {
        let err = at_1m.decode(&minutes(&rows)).unwrap_err();
        assert!(err.contains("line 3"), "{rows:?}: {err}");
    }
    let bad = at_1m.decode("time,open,high,low,close,volume\n1705276800000,10,1,5,15,1\n");
    assert!(bad.unwrap_err().contains("line 2"), "low <= open, close <= high");
}

/// **`--allow-coarse-grid`: events at round times** (the owner's ruling of 2026-11-18): forty releases at
/// :00 and :30 are refused under `--sparse` alone as 30-minute data, naming the flag, and import with it.
/// It switches off that one check: a row off the grid and a time that does not increase are still
/// refused; and beside a dense file it skips nothing (the wire's door refuses the pair split).
#[test]
fn allow_coarse_grid_lets_round_time_events_in_and_nothing_else() {
    let releases: Vec<i64> = (0..40).map(|k| k * 30).collect();
    let sparse = new_lane("1m", "news", true, false).unwrap();
    assert_eq!(
        sparse.decode(&minutes(&releases)).unwrap_err(),
        "all 40 rows lie on the 30-minute grid: is this 30-minute data? use --interval 30m, or add \
         --allow-coarse-grid if these are events at round times"
    );
    let events = new_lane("1m", "news", true, true).unwrap();
    assert_eq!(events.decode(&minutes(&releases)).unwrap().len(), 40);

    let half =
        format!("time,open,high,low,close,volume\n{DAY},1,1,1,1,1\n{},1,1,1,1,1\n", DAY + 30_000);
    assert!(events.decode(&half).unwrap_err().contains("off the UTC 1m grid"));
    for rows in [[30, 0], [30, 30]] {
        let err = events.decode(&minutes(&rows)).unwrap_err();
        assert!(err.contains("line 3"), "{rows:?}: {err}");
    }
    let dense = new_lane("1m", "news", false, true).unwrap();
    let err = dense.decode(&minutes(&releases)).unwrap_err();
    assert!(err.starts_with("rows are 30m apart but --interval is 1m"), "{err}");
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
    let columns = CsvBarsColumns::Header;
    let ru =
        ArchiveLaneUserBars::new("binance", "BTCUSDT", "1h", "ru", format, columns, false, false)
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
        assert!(new_lane("1h", label, false, false).is_err(), "{label:?}");
    }
    for interval in ["7m", "1w", "01h"] {
        assert!(new_lane(interval, "ok", false, false).is_err(), "{interval}");
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
