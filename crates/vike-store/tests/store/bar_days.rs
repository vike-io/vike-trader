//! `HistoryStore::series_days` of BARS: a day's stamp moves when that day's parts change and only then, and a day
//! written under provisional keys alone says so.

use vike_data::store::store_kind::AGGTRADES_LANE;
use vike_data::{BarDay, HistoryStore, TimestampRange};
use vike_model::MS_PER_DAY;
use vike_model::StoreKindId;
use vike_store::HistoryStoreDataFusion;

use crate::common::bar;

fn days(df: &HistoryStoreDataFusion, range: TimestampRange) -> Vec<BarDay> {
    // Through the TRAIT: the inherent method of the same name would win a method call.
    HistoryStore::series_days(df, StoreKindId::Bar, "oanda", None, "EURUSD", Some("1m"), range)
        .unwrap()
        .expect("this store knows its days")
}

fn put(df: &HistoryStoreDataFusion, ts: &[i64], key: &str) {
    let bars: Vec<_> = ts.iter().map(|&t| bar(t, 1.0, None)).collect();
    df.append_bars("oanda", "EURUSD", "1m", &bars, Some(key)).unwrap();
}

/// An append to a LATER day leaves every earlier day's stamp as it was; an append inside a day moves that
/// day's stamp alone; a range lists only its own days.
#[test]
fn a_day_stamp_moves_with_that_days_parts_alone() {
    let dir = tempfile::tempdir().unwrap();
    let df = HistoryStoreDataFusion::open(dir.path()).unwrap();
    put(&df, &[0, 60_000, MS_PER_DAY, MS_PER_DAY + 60_000], "first");
    let before = days(&df, TimestampRange::all());
    assert_eq!(before.iter().map(|d| d.day).collect::<Vec<_>>(), vec![0, 1]);
    assert!(before.iter().all(|d| !d.provisional));

    put(&df, &[2 * MS_PER_DAY], "later");
    let after = days(&df, TimestampRange::all());
    assert_eq!(&after[..2], &before[..], "a later day leaves the earlier stamps");
    assert_eq!(after[2].day, 2);

    put(&df, &[MS_PER_DAY + 120_000], "inside");
    let changed = days(&df, TimestampRange::all());
    assert_eq!(changed[0], before[0], "day 0 holds the same parts");
    assert_ne!(changed[1].stamp, before[1].stamp, "day 1 gained a part");

    let one = days(&df, TimestampRange::of(MS_PER_DAY, 2 * MS_PER_DAY - 1));
    assert_eq!(one, vec![changed[1]], "only the days the range reads");
    assert!(days(&df, TimestampRange::of(5 * MS_PER_DAY, 6 * MS_PER_DAY)).is_empty());
}

/// A day written under a provisional key alone is provisional; the settled commit that supersedes it
/// stamps its own key beside the provisional one and the day is no longer provisional, under a new stamp.
#[test]
fn a_provisional_day_says_so_until_it_is_settled() {
    let dir = tempfile::tempdir().unwrap();
    let df = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let provisional = "dukascopy-resample-provisional:EURUSD:1m:0-1";
    let early = vec![bar(0, 1.0, None)];
    df.append_bars_superseding("oanda", "EURUSD", "1m", &early, Some(provisional), None).unwrap();
    let open = days(&df, TimestampRange::all());
    assert!(open[0].provisional, "{open:?}");

    let settled = vec![bar(0, 1.0, None), bar(60_000, 1.1, None)];
    df.append_bars_superseding(
        "oanda",
        "EURUSD",
        "1m",
        &settled,
        Some("dukascopy-resample:EURUSD:1m:0-1"),
        Some(provisional),
    )
    .unwrap();
    let done = days(&df, TimestampRange::all());
    assert!(!done[0].provisional, "{done:?}");
    assert_ne!(done[0].stamp, open[0].stamp);
}

/// A lane's days are its own: the sourceless series' parts are not in them.
#[test]
fn a_lanes_days_are_the_lanes_own() {
    let dir = tempfile::tempdir().unwrap();
    let df = HistoryStoreDataFusion::open(dir.path()).unwrap();
    put(&df, &[0], "venue");
    let lane = vec![bar(MS_PER_DAY, 1.0, None)];
    df.append_bars_sourced("oanda", AGGTRADES_LANE, "EURUSD", "1m", &lane, Some("lane")).unwrap();
    // The inherent read (the trait's answers through it, `days` above).
    let got = df
        .series_days(
            StoreKindId::Bar,
            "oanda",
            Some(AGGTRADES_LANE),
            "EURUSD",
            Some("1m"),
            TimestampRange::all(),
        )
        .unwrap();
    assert_eq!(got.iter().map(|d| d.day).collect::<Vec<_>>(), vec![1]);
}
