use super::*;
// The flat closed/forming bar the direct-bar store tests seed and close with.
use vike_marketdata::L2BookLevel;
use vike_marketdata::test_support::flat_bar_unit_volume;

#[test]
fn tick_store_fifo_drain_and_cap() {
    let s = TickStore::default();
    let t = |timestamp| vike_marketdata::TickTrade {
        timestamp,
        local_timestamp: 0,
        price: 1.0,
        size: 1.0,
        is_buyer_maker: false,
        symbol: "BTCUSDT".into(),
    };
    s.push("binance", &t(1));
    s.push("binance", &t(2));
    let got = s.drain("binance", "BTCUSDT");
    assert_eq!(got.iter().map(|x| x.timestamp).collect::<Vec<_>>(), vec![1, 2]);
    assert!(s.drain("binance", "BTCUSDT").is_empty()); // drained
    for i in 0..50_100 {
        s.push("binance", &t(i));
    }
    let got = s.drain("binance", "BTCUSDT");
    assert_eq!(got.len(), 50_000);
    assert_eq!(got[0].timestamp, 100); // oldest 100 dropped
}

/// The GUI's sink (split-plane B2): trades and books land in the GUI stores — the tick/DOM plane
/// has no core behind it — and the bar lanes land in the [`StoredBarStore`] (the direct-bar
/// follow-up: they used to be explicit no-ops). The lanes only a core consumed (mark/close ticks,
/// quotes) stay consumer-less.
#[test]
fn gui_feed_sink_lands_trades_books_and_bars_and_drops_the_price_board_lanes() {
    use vike_data::DataClientSink;
    let books = std::sync::Arc::new(L2BookStore::default());
    let trades = std::sync::Arc::new(TickStore::default());
    let bars = std::sync::Arc::new(StoredBarStore::default());
    let sink =
        DataClientSinkGuiFeed { books: books.clone(), trades: trades.clone(), bars: bars.clone() };

    sink.trade(
        "binance",
        "BTCUSDT",
        vike_marketdata::TickTrade {
            timestamp: 7,
            local_timestamp: 0,
            price: 100.0,
            size: 1.0,
            is_buyer_maker: false,
            symbol: "BTCUSDT".into(),
        },
    );
    sink.l2_snapshot(
        "okx",
        "BTC-USDT",
        0.1,
        vec![L2BookLevel::new(99.9, 1.0)],
        vec![L2BookLevel::new(100.1, 2.0)],
        7,
    );
    sink.book("polymarket", "1071", std::sync::Arc::new(vike_marketdata::L2Book::new(0.01)));

    assert_eq!(trades.drain("binance", "BTCUSDT").len(), 1, "trade tape is client-direct");
    assert!(books.get("okx", "BTC-USDT").is_some(), "DOM books are client-direct");
    assert!(books.get("polymarket", "1071").is_some(), "cockpit books are client-direct");

    // The bar lanes land in the direct store — seed, then a live close, then a forming.
    sink.seed_bars("binance", "BTCUSDT", "1m", vec![flat_bar_unit_volume(60_000, 10.0)]);
    sink.close_bar("binance", "BTCUSDT", "1m", flat_bar_unit_volume(120_000, 11.0));
    sink.forming_bar("binance", "BTCUSDT", "1m", flat_bar_unit_volume(180_000, 12.0));
    let (closed, forming) = bars.series("binance", "BTCUSDT", "1m").expect("bars landed");
    assert_eq!(closed.len(), 2, "seed + live close are the closed history");
    assert_eq!(forming.map(|b| b.timestamp), Some(180_000), "the forming snapshot rides beside it");

    // The still-dropped lanes: callable (feeds emit them uniformly), landing nowhere.
    sink.mark_tick("binance", "BTCUSDT", 100.0, 7);
    sink.bar_close_tick("binance", "BTCUSDT", 100.0, 7);
    sink.quote(
        "binance",
        "BTCUSDT",
        vike_marketdata::TickQuote {
            timestamp: 7,
            local_timestamp: 0,
            bid: 99.0,
            ask: 101.0,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: "BTCUSDT".into(),
        },
    );
}

/// The store mirrors `Ingest::BarSeed`: a seed REPLACES the series — closed becomes the
/// seed, forming cleared — so a reconnect re-seed repairs the lossless lane instead of
/// merging into stale history.
#[test]
fn direct_store_seed_replaces_the_series_and_clears_forming() {
    let s = StoredBarStore::default();
    s.seed(
        "binance",
        "BTCUSDT",
        "1m",
        vec![flat_bar_unit_volume(60_000, 10.0), flat_bar_unit_volume(120_000, 11.0)],
    );
    s.forming("binance", "BTCUSDT", "1m", flat_bar_unit_volume(180_000, 12.0));
    s.seed("binance", "BTCUSDT", "1m", vec![flat_bar_unit_volume(120_000, 11.5)]);
    let (closed, forming) = s.series("binance", "BTCUSDT", "1m").unwrap();
    assert_eq!(closed.len(), 1, "the re-seed replaced the whole closed history");
    assert_eq!(closed[0].close, 11.5);
    assert!(forming.is_none(), "a seed clears forming — the live stream refreshes it");
}

/// THE BOUNDARY-TIMESTAMP DEDUP RULE (`Ingest::BarClose`'s): a close at the last held ts replaces
/// that bar idempotently — the seed's last bar re-closed live must never appear twice — a
/// strictly older close is dropped, and a newer one appends.
#[test]
fn direct_store_close_dedups_at_the_boundary_timestamp() {
    let s = StoredBarStore::default();
    s.seed(
        "binance",
        "BTCUSDT",
        "1m",
        vec![flat_bar_unit_volume(60_000, 10.0), flat_bar_unit_volume(120_000, 11.0)],
    );

    // The boundary case: the venue re-closes the window the seed already carried.
    s.close("binance", "BTCUSDT", "1m", flat_bar_unit_volume(120_000, 11.9));
    let (closed, _) = s.series("binance", "BTCUSDT", "1m").unwrap();
    assert_eq!(closed.len(), 2, "same-ts close must REPLACE, never duplicate");
    assert_eq!(closed[1].close, 11.9, "…and the replacement is the venue's newer bar");

    // Stale replay: dropped.
    s.close("binance", "BTCUSDT", "1m", flat_bar_unit_volume(60_000, 9.0));
    let (closed, _) = s.series("binance", "BTCUSDT", "1m").unwrap();
    assert_eq!(closed.len(), 2);
    assert_eq!(closed[0].close, 10.0, "an older close is a stale replay — dropped");

    // The live append.
    s.close("binance", "BTCUSDT", "1m", flat_bar_unit_volume(180_000, 12.0));
    let (closed, _) = s.series("binance", "BTCUSDT", "1m").unwrap();
    assert_eq!(
        closed.iter().map(|b| b.timestamp).collect::<Vec<_>>(),
        vec![60_000, 120_000, 180_000]
    );
}

/// The forming lane is latest-wins and only ever STRICTLY ABOVE the last closed ts: a close
/// supersedes the forming it closes, and a late forming snapshot of an already-closed window
/// is dropped (the double-paint at the boundary this rule exists for).
#[test]
fn direct_store_forming_is_superseded_by_its_close_and_never_resurrects() {
    let s = StoredBarStore::default();
    s.forming("binance", "BTCUSDT", "1m", flat_bar_unit_volume(60_000, 10.0));
    let (_, forming) = s.series("binance", "BTCUSDT", "1m").unwrap();
    assert_eq!(forming.map(|b| b.timestamp), Some(60_000), "forming lands even before any close");

    s.close("binance", "BTCUSDT", "1m", flat_bar_unit_volume(60_000, 10.5));
    let (closed, forming) = s.series("binance", "BTCUSDT", "1m").unwrap();
    assert_eq!(closed.len(), 1);
    assert!(forming.is_none(), "the close supersedes the forming snapshot of its window");

    // A late conflated snapshot of the closed window arrives after the close: dropped.
    s.forming("binance", "BTCUSDT", "1m", flat_bar_unit_volume(60_000, 10.4));
    let (_, forming) = s.series("binance", "BTCUSDT", "1m").unwrap();
    assert!(forming.is_none(), "a forming at or below the last closed ts must not repaint");

    // The next window's forming is kept, latest-wins.
    s.forming("binance", "BTCUSDT", "1m", flat_bar_unit_volume(120_000, 11.0));
    s.forming("binance", "BTCUSDT", "1m", flat_bar_unit_volume(120_000, 11.2));
    let (_, forming) = s.series("binance", "BTCUSDT", "1m").unwrap();
    assert_eq!(forming.map(|b| b.close), Some(11.2));
}

/// The bounded-history contract: closed bars never exceed [`STORED_BAR_CLOSED_CAP`], with
/// oldest-first eviction — on the live append path and on an oversized seed alike.
#[test]
fn direct_store_closed_history_is_bounded_with_oldest_first_eviction() {
    let s = StoredBarStore::default();
    s.seed(
        "binance",
        "BTCUSDT",
        "1m",
        (0..STORED_BAR_CLOSED_CAP as i64 + 7)
            .map(|i| flat_bar_unit_volume(i * 60_000, 10.0))
            .collect(),
    );
    let (closed, _) = s.series("binance", "BTCUSDT", "1m").unwrap();
    assert_eq!(closed.len(), STORED_BAR_CLOSED_CAP, "an oversized seed is trimmed to the cap");
    assert_eq!(closed[0].timestamp, 7 * 60_000, "…dropping the OLDEST bars");

    let next_timestamp = (STORED_BAR_CLOSED_CAP as i64 + 7) * 60_000;
    s.close("binance", "BTCUSDT", "1m", flat_bar_unit_volume(next_timestamp, 11.0));
    let (closed, _) = s.series("binance", "BTCUSDT", "1m").unwrap();
    assert_eq!(closed.len(), STORED_BAR_CLOSED_CAP, "a live append evicts one from the front");
    assert_eq!(closed[0].timestamp, 8 * 60_000);
    assert_eq!(closed.last().unwrap().timestamp, next_timestamp);
}

/// `generation` is the fold's dirty probe: every state-changing write bumps it, a refused
/// write does not, and `clear` (the backend-switch clear) both empties the store and bumps —
/// so the fold that trusts an unchanged generation can never miss a change.
#[test]
fn direct_store_generation_tracks_every_state_change_and_only_those() {
    let s = StoredBarStore::default();
    let g0 = s.generation();
    s.seed("binance", "BTCUSDT", "1m", vec![flat_bar_unit_volume(60_000, 10.0)]);
    let g1 = s.generation();
    assert!(g1 > g0, "seed bumps");
    s.close("binance", "BTCUSDT", "1m", flat_bar_unit_volume(30_000, 9.0)); // stale — dropped
    assert_eq!(s.generation(), g1, "a dropped stale close changes nothing and must not bump");
    s.forming("binance", "BTCUSDT", "1m", flat_bar_unit_volume(60_000, 10.0)); // at the closed ts — dropped
    assert_eq!(s.generation(), g1, "a dropped stale forming must not bump");
    s.close("binance", "BTCUSDT", "1m", flat_bar_unit_volume(120_000, 11.0));
    let g2 = s.generation();
    assert!(g2 > g1, "a live close bumps");
    s.clear();
    assert!(s.generation() > g2, "clear bumps — the fold refolds the now-empty store");
    assert!(s.keys().is_empty());
}

/// The tick chart's tape ([`TickStore::recent`]): a reader that does NOT drain, so a Trade window
/// beside a chart window on the same symbol takes nothing from it; two minutes deep; the aggressor
/// read off the maker flag; the newest N on request; and empty for a key nobody printed on.
#[test]
fn tick_store_recent_is_a_non_draining_two_minute_tape() {
    let s = TickStore::default();
    let t = |local_timestamp, price, maker| vike_marketdata::TickTrade {
        timestamp: local_timestamp,
        local_timestamp,
        price,
        size: 1.0,
        is_buyer_maker: maker,
        symbol: "BTCUSDT".into(),
    };
    s.push("binance", &t(1_000, 100.0, false));
    s.push("binance", &t(2_000, 101.0, true));
    assert_eq!(s.recent("binance", "BTCUSDT", 10).len(), 2);
    assert_eq!(s.recent("binance", "BTCUSDT", 10).len(), 2, "reading takes nothing");
    assert_eq!(s.drain("binance", "BTCUSDT").len(), 2, "and the FIFO is the chart windows'");
    let tape = s.recent("binance", "BTCUSDT", 10);
    assert_eq!(tape.len(), 2, "a drain leaves the tape");
    assert!(tape[0].buy, "a taker bought where the buyer was not the maker");
    assert!(!tape[1].buy, "a buyer-maker print is a SELL");
    s.push("binance", &t(1_000 + RECENT_MS + 1, 102.0, false));
    let prices: Vec<f64> = s.recent("binance", "BTCUSDT", 10).iter().map(|p| p.price).collect();
    assert_eq!(prices, vec![101.0, 102.0], "what is older than two minutes is gone");
    assert_eq!(s.recent("binance", "BTCUSDT", 1).len(), 1, "the newest N");
    assert_eq!(s.recent("binance", "BTCUSDT", 1)[0].price, 102.0);
    assert!(s.recent("bybit", "BTCUSDT", 10).is_empty());
}

/// **A quote's step keeps the finest spelling seen**: one price that happens to end in a zero says
/// nothing about the grid, and a coarser step for that one frame would print the mid wrong and refuse a
/// price the venue takes. A gap forgets the key, and a key forgotten starts again.
#[test]
fn a_quote_keeps_the_finest_spelling_it_has_seen() {
    let s = TickStore::default();
    s.set_quote("oanda", "EUR_USD", (1.07123, 1.07131), 1, None);
    assert_eq!(s.quote("oanda", "EUR_USD").map(|q| q.tick), Some(0.00001));
    s.set_quote("oanda", "EUR_USD", (1.0712, 1.0713), 2, None);
    let q = s.quote("oanda", "EUR_USD").expect("held");
    assert_eq!((q.bid, q.ask, q.at_ms, q.tick), (1.0712, 1.0713, 2, 0.00001));

    s.forget_quote("oanda", "EUR_USD");
    assert_eq!(s.quote("oanda", "EUR_USD"), None);
    s.set_quote("oanda", "USD_JPY", (150.0, 151.0), 3, None);
    assert_eq!(s.quote("oanda", "USD_JPY").map(|q| q.tick), Some(1.0), "whole prices, whole step");
}

/// Poison `m` the way a dying feed thread does: a thread that panics while it holds the lock.
fn poison<T: Send>(m: &std::sync::Mutex<T>) {
    std::thread::scope(|s| {
        let died = s
            .spawn(|| {
                let _held = m.lock();
                panic!("a feed thread dies holding the store's lock");
            })
            .join();
        assert!(died.is_err());
    });
    assert!(m.is_poisoned());
}

/// **A feed thread that panics holding a store's lock does not take the GUI with it** (`vike_utility::mutex::lock`):
/// every store the feed threads write still reads and writes after the lock is poisoned. Each read
/// used to `unwrap` the poisoned lock, so the next frame panicked the GUI thread.
#[test]
fn a_store_poisoned_by_a_dying_feed_thread_still_reads_and_writes() {
    let books = L2BookStore::default();
    poison(&books.books);
    books.insert_book("binance", "BTCUSDT", vike_marketdata::L2Book::new(0.1), 7);
    assert_eq!(books.get("binance", "BTCUSDT").map(|(_, timestamp)| timestamp), Some(7));
    books.remove("binance", "BTCUSDT");
    assert!(books.get("binance", "BTCUSDT").is_none());

    let tape = TickStore::default();
    poison(&tape.trades);
    poison(&tape.recent);
    poison(&tape.quotes);
    let trade = vike_marketdata::TickTrade {
        timestamp: 1,
        local_timestamp: 1,
        price: 100.0,
        size: 1.0,
        is_buyer_maker: false,
        symbol: "BTCUSDT".into(),
    };
    tape.push("binance", &trade);
    assert_eq!(tape.recent("binance", "BTCUSDT", 10).len(), 1);
    assert_eq!(tape.drain("binance", "BTCUSDT").len(), 1);
    tape.set_quote("binance", "BTCUSDT", (99.5, 100.5), 2, None);
    assert_eq!(tape.quote("binance", "BTCUSDT").map(|q| q.at_ms), Some(2));
    tape.forget_quote("binance", "BTCUSDT");

    let bars = StoredBarStore::default();
    poison(&bars.series);
    bars.seed("binance", "BTCUSDT", "1m", vec![flat_bar_unit_volume(60_000, 10.0)]);
    bars.close("binance", "BTCUSDT", "1m", flat_bar_unit_volume(120_000, 11.0));
    bars.forming("binance", "BTCUSDT", "1m", flat_bar_unit_volume(180_000, 12.0));
    let (closed, forming) = bars.series("binance", "BTCUSDT", "1m").expect("held");
    assert_eq!((closed.len(), forming.is_some()), (2, true));
    assert_eq!(bars.keys().len(), 1);
    bars.clear();
    assert!(bars.keys().is_empty());
}
