//! `HistoryStore::series_days` of a TICK read: a tick day's stamp moves when a part a tick read of that day reads
//! changes (the symbol's own series OR a `group=` series of its kind and venue), and only then; a tick read that names
//! an interval or a source, a bar read without its interval, and a kind with no stamped read are refused in words.

use vike_data::{BarDay, HistoryStore, TimestampRange};
use vike_marketdata::TickTrade;
use vike_model::MS_PER_DAY;
use vike_model::StoreKindId;
use vike_store::HistoryStoreDataFusion;

const VENUE: &str = "binance";
const SYMBOL: &str = "BTCUSDT";

/// `kind`'s stamped days of `range`, through the TRAIT (the inherent method of the same name would win a method call).
fn days_of(df: &HistoryStoreDataFusion, kind: StoreKindId, range: TimestampRange) -> Vec<BarDay> {
    HistoryStore::series_days(df, kind, VENUE, None, SYMBOL, None, range)
        .unwrap()
        .expect("this store knows its days")
}

fn days(df: &HistoryStoreDataFusion, range: TimestampRange) -> Vec<BarDay> {
    days_of(df, StoreKindId::Trade, range)
}

fn trade(symbol: &str, timestamp: i64) -> TickTrade {
    TickTrade {
        timestamp,
        local_timestamp: 0,
        price: 60_000.0,
        size: 1.0,
        is_buyer_maker: false,
        symbol: symbol.into(),
    }
}

fn put(df: &HistoryStoreDataFusion, ts: &[i64], key: &str) {
    let trades: Vec<_> = ts.iter().map(|&t| trade(SYMBOL, t)).collect();
    df.append_trades(VENUE, SYMBOL, &trades, Some(key)).unwrap();
}

/// An append to a LATER day leaves every earlier day's stamp; an append inside a day moves that day's alone; a
/// GROUPED part meeting a day moves that day's stamp too (the read takes rows from it); a range lists its own days.
#[test]
fn a_tick_day_stamp_moves_with_every_layout_the_read_reads() {
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

    let grouped = [trade(SYMBOL, 2 * MS_PER_DAY + 5), trade("ETHUSDT", 2 * MS_PER_DAY + 6)];
    df.append_trades_grouped(VENUE, "majors", &grouped, Some("group")).unwrap();
    let joined = days(&df, TimestampRange::all());
    assert_eq!(&joined[..2], &changed[..2], "a grouped part of day 2 leaves days 0 and 1");
    assert_ne!(joined[2].stamp, changed[2].stamp, "the grouped part is read for day 2");

    let one = days(&df, TimestampRange::of(MS_PER_DAY, 2 * MS_PER_DAY - 1));
    assert_eq!(one, vec![joined[1]], "only the days the range reads");
    assert!(days(&df, TimestampRange::of(5 * MS_PER_DAY, 6 * MS_PER_DAY)).is_empty());
    // The quote lane of the same symbol holds nothing: its days are its own.
    assert_eq!(days_of(&df, StoreKindId::Quote, TimestampRange::all()), Vec::new());
}

/// Bit for bit, memo on and off: a tick read's days over its own series AND a group, from a memo a narrow range
/// filled (most parts never stat-ed), are what a store with no memo answers, range by range, twice. A repeat is the
/// memo's: a part touched behind the store's back moves only the memo-less store's stamp.
#[test]
fn a_memo_answers_tick_days_as_a_store_without_one() {
    let dir = tempfile::tempdir().unwrap();
    let df = HistoryStoreDataFusion::open(dir.path()).unwrap();
    put(&df, &[0, 60_000, MS_PER_DAY, MS_PER_DAY + 60_000, 2 * MS_PER_DAY], "first");
    put(&df, &[MS_PER_DAY + 120_000], "inside");
    let grouped = [trade(SYMBOL, MS_PER_DAY + 5), trade("ETHUSDT", MS_PER_DAY + 6)];
    df.append_trades_grouped(VENUE, "majors", &grouped, Some("group")).unwrap();
    let fresh = || HistoryStoreDataFusion::open_read_only(dir.path()).unwrap();
    let ranges = [
        TimestampRange::of(MS_PER_DAY, MS_PER_DAY),
        TimestampRange::all(),
        TimestampRange::of(60_000, 2 * MS_PER_DAY),
        TimestampRange { start: Some(MS_PER_DAY + 1), end: None },
        TimestampRange::of(5 * MS_PER_DAY, 6 * MS_PER_DAY),
    ];
    for _ in 0..2 {
        for range in ranges {
            assert_eq!(days(&df, range), days(&fresh(), range), "{range:?}");
        }
    }

    let first = days(&df, TimestampRange::all());
    let day_zero =
        dir.path().join(format!("kind=trade/venue={VENUE}/symbol={SYMBOL}/date=1970-01-01"));
    let part = std::fs::read_dir(day_zero).unwrap().next().unwrap().unwrap().path();
    let file = std::fs::File::options().write(true).open(part).unwrap();
    file.set_modified(
        std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000),
    )
    .unwrap();
    assert_eq!(days(&df, TimestampRange::all()), first, "the repeat is the memo's");
    let cold = days(&fresh(), TimestampRange::all());
    assert_ne!(cold[0].stamp, first[0].stamp, "a store without the memo stats the touched part");
    assert_eq!(cold[1..], first[1..]);
}

/// A read whose shape is not one of the stamped reads is refused in words, never answered as an empty day list.
#[test]
fn a_read_the_kind_does_not_make_is_refused_in_words() {
    let dir = tempfile::tempdir().unwrap();
    let df = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let all = TimestampRange::all();
    let cases = [
        (StoreKindId::Trade, None, Some("1m"), "names no interval and no source"),
        (StoreKindId::Quote, Some("user-x"), None, "names no interval and no source"),
        (StoreKindId::Bar, None, None, "a bar read names its interval"),
        (StoreKindId::Depth, None, None, "has no stamped read"),
    ];
    for (kind, source, interval, words) in cases {
        let refused = df.series_days(kind, VENUE, source, SYMBOL, interval, all);
        let why = refused.expect_err("a read of no stamped shape").to_string();
        assert!(why.contains(words), "{kind} {source:?} {interval:?}: {why}");
    }
}
