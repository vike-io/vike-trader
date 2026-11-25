//! `HistoryStoreSynthetic`: range reads of bars and books under its venue, its inventory, and every
//! refusal.

use vike_marketdata::L2BookLevel;

use super::*;

const HOUR: i64 = 3_600_000;
const T0: i64 = 1_735_689_600_000; // 2025-01-01T00:00:00Z

/// `n` hourly bars from `T0`, bar `i` priced `i + 1`.
fn path(n: i64) -> Vec<Bar> {
    (0..n)
        .map(|i| {
            let p = (i + 1) as f64;
            Bar::ohlcv(T0 + i * HOUR, p, p + 0.5, p - 0.5, p, 1.0)
        })
        .collect()
}

fn store() -> HistoryStoreSynthetic {
    HistoryStoreSynthetic::default()
        .with_series("SYNUSD", "1h", path(5))
        .unwrap()
        .with_series("OTHERUSD", "1h", path(2))
        .unwrap()
}

fn read(s: &HistoryStoreSynthetic, symbol: &str, range: TimestampRange) -> Vec<i64> {
    s.load_bars(SYNTHETIC_VENUE, symbol, "1h", range).unwrap().iter().map(|b| b.timestamp).collect()
}

fn update(timestamp: i64, sequence: u64, kind: L2BookUpdateKind, bid: f64) -> L2BookUpdate {
    L2BookUpdate {
        timestamp,
        local_timestamp: 0,
        sequence,
        kind,
        tick_size: 0.5,
        bids: vec![L2BookLevel::new(bid, 1.0)],
        asks: vec![],
        symbol: String::new(),
    }
}

/// A snapshot at `T0` and two deltas, one quote, one trade.
fn book() -> (Vec<L2BookUpdate>, Vec<TickQuote>, Vec<TickTrade>) {
    let updates = vec![
        update(T0, 7, L2BookUpdateKind::Snapshot, 99.0),
        update(T0 + 10, 8, L2BookUpdateKind::Delta, 99.5),
        update(T0 + 10, 9, L2BookUpdateKind::Delta, 99.0),
    ];
    let quote = TickQuote {
        timestamp: T0 + 10,
        local_timestamp: 0,
        bid: 99.5,
        ask: 100.0,
        bid_size: 1.0,
        ask_size: 2.0,
        symbol: Default::default(),
    };
    let trade = TickTrade {
        timestamp: T0 + 20,
        local_timestamp: 0,
        price: 100.0,
        size: 1.0,
        is_buyer_maker: false,
        symbol: Default::default(),
    };
    (updates, vec![quote], vec![trade])
}

fn with_book() -> HistoryStoreSynthetic {
    let (updates, quotes, trades) = book();
    store().with_book("SYNUSD", updates, quotes, trades).unwrap()
}

#[test]
fn each_series_is_read_over_a_range_under_the_synthetic_venue() {
    let s = store();
    assert_eq!(
        s.load_bars(SYNTHETIC_VENUE, "SYNUSD", "1h", TimestampRange::all()).unwrap(),
        path(5)
    );
    assert_eq!(
        read(&s, "SYNUSD", TimestampRange::of(T0 + HOUR, T0 + 2 * HOUR)),
        [T0 + HOUR, T0 + 2 * HOUR]
    );
    assert_eq!(
        read(&s, "SYNUSD", TimestampRange { start: Some(T0 + 4 * HOUR), end: None }),
        [T0 + 4 * HOUR]
    );
    assert_eq!(read(&s, "OTHERUSD", TimestampRange::all()), [T0, T0 + HOUR]);
    // Another series of the venue is the missing series' answer: empty.
    assert!(read(&s, "NONEUSD", TimestampRange::all()).is_empty());
    assert!(
        s.load_bars(SYNTHETIC_VENUE, "SYNUSD", "1m", TimestampRange::all()).unwrap().is_empty()
    );
}

#[test]
fn a_book_is_read_over_a_range_with_its_symbol_stamped_on_every_row() {
    let s = with_book();
    let all = TimestampRange::all();
    let updates = s.scan_book_updates(SYNTHETIC_VENUE, "SYNUSD", all).unwrap();
    assert_eq!(updates.iter().map(|u| u.sequence).collect::<Vec<_>>(), [7, 8, 9]);
    assert!(updates.iter().all(|u| u.symbol == "SYNUSD"));
    let quotes = s.scan_quotes(SYNTHETIC_VENUE, "SYNUSD", all).unwrap();
    assert_eq!((quotes.len(), quotes[0].symbol.as_str(), quotes[0].bid), (1, "SYNUSD", 99.5));
    let trades = s.scan_trades(SYNTHETIC_VENUE, "SYNUSD", all).unwrap();
    assert_eq!((trades.len(), trades[0].symbol.as_str()), (1, "SYNUSD"));
    let late = TimestampRange { start: Some(T0 + 10), end: Some(T0 + 15) };
    assert_eq!(s.scan_book_updates(SYNTHETIC_VENUE, "SYNUSD", late).unwrap().len(), 2);
    assert!(s.scan_trades(SYNTHETIC_VENUE, "SYNUSD", late).unwrap().is_empty());
    // A symbol with no book is the missing series' answer: empty.
    assert!(s.scan_quotes(SYNTHETIC_VENUE, "OTHERUSD", all).unwrap().is_empty());
    assert!(s.scan_book_updates(SYNTHETIC_VENUE, "OTHERUSD", all).unwrap().is_empty());
    // Its bars are untouched by its book.
    assert_eq!(read(&s, "SYNUSD", all).len(), 5);
}

#[test]
fn it_lists_every_series_it_holds_with_its_coverage() {
    let s = with_book();
    let bar = |symbol: &str| {
        SeriesId::per_symbol(StoreKindId::Bar, SYNTHETIC_VENUE, symbol, Some("1h".into()))
    };
    let tick = |kind| SeriesId::per_symbol(kind, SYNTHETIC_VENUE, "SYNUSD", None);
    assert_eq!(
        s.list_series().unwrap(),
        [
            bar("SYNUSD"),
            bar("OTHERUSD"),
            tick(StoreKindId::Book),
            tick(StoreKindId::Quote),
            tick(StoreKindId::Trade)
        ]
    );
    let inventory = s.inventory().unwrap();
    let facts = |i: usize| {
        (inventory[i].1.first_timestamp, inventory[i].1.last_timestamp, inventory[i].1.rows)
    };
    assert_eq!(facts(0), (T0, T0 + 4 * HOUR, 5));
    assert_eq!(facts(2), (T0, T0 + 10, 3));
    assert_eq!(facts(4), (T0 + 20, T0 + 20, 1));
    assert!(s.series_gaps(&bar("SYNUSD")).unwrap().is_empty());
    assert!(HistoryStoreSynthetic::default().list_series().unwrap().is_empty());
}

#[test]
fn a_real_venue_a_lane_and_every_write_are_refused() {
    let s = with_book();
    let r = TimestampRange::all();
    let why = s.load_bars("binance", "SYNUSD", "1h", r).unwrap_err().to_string();
    assert!(why.contains("never \"binance\"'s"), "{why}");
    for read in [
        s.scan_quotes("binance", "SYNUSD", r).map(|v| v.len()),
        s.scan_trades("binance", "SYNUSD", r).map(|v| v.len()),
        s.scan_book_updates("binance", "SYNUSD", r).map(|v| v.len()),
    ] {
        assert!(read.unwrap_err().to_string().contains("never \"binance\"'s"));
    }
    assert!(s.bar_edges("binance", "SYNUSD", "1h", r).is_err());
    let lane = s.load_bars_sourced_capped(SYNTHETIC_VENUE, "file", "SYNUSD", "1h", r, None);
    assert!(lane.is_err());
    assert!(s.append_bars(SYNTHETIC_VENUE, "SYNUSD", "1h", &path(1), None).is_err());
    assert!(s.append_quotes(SYNTHETIC_VENUE, "SYNUSD", &[], None).is_err());
    for refusal in [
        s.scan_symbol_properties(SYNTHETIC_VENUE, "SYNUSD", r).map(|v| v.len()),
        s.scan_equity(SYNTHETIC_VENUE, "SYNUSD", r).map(|v| v.len()),
    ] {
        let why = refusal.unwrap_err().to_string();
        assert!(why.contains("holds generated bar series and order books only"), "{why}");
    }
}

#[test]
fn a_series_off_its_grid_or_held_twice_is_refused() {
    let mut holed = path(4);
    holed.remove(2);
    let why = HistoryStoreSynthetic::default().with_series("SYNUSD", "1h", holed).unwrap_err();
    assert!(why.to_string().contains("bars 1 and 2"), "{why}");
    let mut reversed = path(3);
    reversed.reverse();
    assert!(HistoryStoreSynthetic::default().with_series("SYNUSD", "1h", reversed).is_err());
    assert!(HistoryStoreSynthetic::default().with_series("SYNUSD", "1h", path(3)).is_ok());
    assert!(HistoryStoreSynthetic::default().with_series("SYNUSD", "5m", path(3)).is_err());
    let twice = store().with_series("SYNUSD", "1h", path(1)).unwrap_err().to_string();
    assert!(twice.contains("already holds SYNUSD 1h"), "{twice}");
    // An interval with no fixed width cannot hold a path.
    let why = HistoryStoreSynthetic::default().with_series("SYNUSD", "1w", path(3)).unwrap_err();
    assert!(why.to_string().contains("no fixed width"), "{why}");
}

#[test]
fn a_book_with_no_snapshot_a_gap_a_step_back_or_held_twice_is_refused() {
    let refused = |updates: Vec<L2BookUpdate>, quotes: Vec<TickQuote>, words: &str| {
        let why = store().with_book("SYNUSD", updates, quotes, vec![]).unwrap_err().to_string();
        assert!(why.contains(words), "{why}");
    };
    let (updates, quotes, trades) = book();
    refused(vec![], vec![], "at least its snapshot");
    refused(updates[1..].to_vec(), vec![], "opens with a snapshot");
    let mut gap = updates.clone();
    gap[2].sequence = 11;
    refused(gap, vec![], "update 2 breaks the contiguous sequence from 7");
    let mut back = updates.clone();
    back[2].timestamp = T0 - 1;
    refused(back, vec![], "updates go back in time after row 1");
    let mut late_first = quotes.clone();
    late_first.push(TickQuote { timestamp: T0, ..quotes[0].clone() });
    refused(updates.clone(), late_first, "quotes go back in time");
    let twice = with_book().with_book("SYNUSD", updates, quotes, trades).unwrap_err();
    assert!(twice.to_string().contains("already holds its book"), "{twice}");
}
