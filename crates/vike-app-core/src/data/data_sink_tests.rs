use super::*;
use vike_model::BookLevel;

#[test]
fn trade_store_fifo_drain_and_cap() {
    let s = TradeStore::default();
    let t = |ts| vike_model::TradeTick {
        ts,
        local_ts: 0,
        price: 1.0,
        size: 1.0,
        is_buyer_maker: false,
        symbol: "BTCUSDT".into(),
    };
    s.push("binance", &t(1));
    s.push("binance", &t(2));
    let got = s.drain("binance", "BTCUSDT");
    assert_eq!(got.iter().map(|x| x.ts).collect::<Vec<_>>(), vec![1, 2]);
    assert!(s.drain("binance", "BTCUSDT").is_empty()); // drained
    for i in 0..50_100 {
        s.push("binance", &t(i));
    }
    let got = s.drain("binance", "BTCUSDT");
    assert_eq!(got.len(), 50_000);
    assert_eq!(got[0].ts, 100); // oldest 100 dropped
}

/// The third mode's sink (split-plane B2): trades and books land in the SAME GUI stores
/// `CoreSinkAdapter` fills — the tick/DOM plane is client-direct with no core — and the bar
/// lanes land in the [`DirectBarStore`] (the direct-bar follow-up: they used to be explicit
/// no-ops). The remaining core-bound lanes (mark/close ticks, quotes) stay consumer-less.
#[test]
fn gui_feed_sink_lands_trades_books_and_bars_and_drops_the_price_board_lanes() {
    use vike_data::LiveDataSink;
    let books = std::sync::Arc::new(BookStore::default());
    let trades = std::sync::Arc::new(TradeStore::default());
    let bars = std::sync::Arc::new(DirectBarStore::default());
    let sink = GuiFeedSink { books: books.clone(), trades: trades.clone(), bars: bars.clone() };

    sink.trade(
        "binance",
        "BTCUSDT",
        vike_model::TradeTick {
            ts: 7,
            local_ts: 0,
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
        vec![BookLevel::new(99.9, 1.0)],
        vec![BookLevel::new(100.1, 2.0)],
        7,
    );
    sink.book("polymarket", "1071", std::sync::Arc::new(vike_model::L2Book::new(0.01)));

    assert_eq!(trades.drain("binance", "BTCUSDT").len(), 1, "trade tape is client-direct");
    assert!(books.get("okx", "BTC-USDT").is_some(), "DOM books are client-direct");
    assert!(books.get("polymarket", "1071").is_some(), "cockpit books are client-direct");

    // The bar lanes land in the direct store — seed, then a live close, then a forming.
    sink.seed_bars("binance", "BTCUSDT", "1m", vec![dbar(60_000, 10.0)]);
    sink.close_bar("binance", "BTCUSDT", "1m", dbar(120_000, 11.0));
    sink.forming_bar("binance", "BTCUSDT", "1m", dbar(180_000, 12.0));
    let (closed, forming) = bars.series("binance", "BTCUSDT", "1m").expect("bars landed");
    assert_eq!(closed.len(), 2, "seed + live close are the closed history");
    assert_eq!(forming.map(|b| b.ts), Some(180_000), "the forming snapshot rides beside it");

    // The still-dropped lanes: callable (feeds emit them uniformly), landing nowhere.
    sink.mark_tick("binance", "BTCUSDT", 100.0, 7);
    sink.bar_close_tick("binance", "BTCUSDT", 100.0, 7);
    sink.quote(
        "binance",
        "BTCUSDT",
        vike_model::QuoteTick {
            ts: 7,
            local_ts: 0,
            bid: 99.0,
            ask: 101.0,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: "BTCUSDT".into(),
        },
    );
}

/// One flat closed/forming bar at `ts` for the direct-bar store tests.
fn dbar(ts: i64, px: f64) -> vike_model::Bar {
    vike_model::Bar {
        ts,
        open: px,
        high: px,
        low: px,
        close: px,
        volume: 1.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// The store mirrors `Ingest::BarSeed`: a seed REPLACES the series — closed becomes the
/// seed, forming cleared — so a reconnect re-seed repairs the lossless lane instead of
/// merging into stale history.
#[test]
fn direct_store_seed_replaces_the_series_and_clears_forming() {
    let s = DirectBarStore::default();
    s.seed("binance", "BTCUSDT", "1m", vec![dbar(60_000, 10.0), dbar(120_000, 11.0)]);
    s.forming("binance", "BTCUSDT", "1m", dbar(180_000, 12.0));
    s.seed("binance", "BTCUSDT", "1m", vec![dbar(120_000, 11.5)]);
    let (closed, forming) = s.series("binance", "BTCUSDT", "1m").unwrap();
    assert_eq!(closed.len(), 1, "the re-seed replaced the whole closed history");
    assert_eq!(closed[0].close, 11.5);
    assert!(forming.is_none(), "a seed clears forming — the live stream refreshes it");
}

/// THE BOUNDARY-TS DEDUP RULE (`Ingest::BarClose`'s): a close at the last held ts replaces
/// that bar idempotently — the seed's last bar re-closed live must never appear twice — a
/// strictly older close is dropped, and a newer one appends.
#[test]
fn direct_store_close_dedups_at_the_boundary_ts() {
    let s = DirectBarStore::default();
    s.seed("binance", "BTCUSDT", "1m", vec![dbar(60_000, 10.0), dbar(120_000, 11.0)]);

    // The boundary case: the venue re-closes the window the seed already carried.
    s.close("binance", "BTCUSDT", "1m", dbar(120_000, 11.9));
    let (closed, _) = s.series("binance", "BTCUSDT", "1m").unwrap();
    assert_eq!(closed.len(), 2, "same-ts close must REPLACE, never duplicate");
    assert_eq!(closed[1].close, 11.9, "…and the replacement is the venue's newer bar");

    // Stale replay: dropped.
    s.close("binance", "BTCUSDT", "1m", dbar(60_000, 9.0));
    let (closed, _) = s.series("binance", "BTCUSDT", "1m").unwrap();
    assert_eq!(closed.len(), 2);
    assert_eq!(closed[0].close, 10.0, "an older close is a stale replay — dropped");

    // The live append.
    s.close("binance", "BTCUSDT", "1m", dbar(180_000, 12.0));
    let (closed, _) = s.series("binance", "BTCUSDT", "1m").unwrap();
    assert_eq!(closed.iter().map(|b| b.ts).collect::<Vec<_>>(), vec![60_000, 120_000, 180_000]);
}

/// The forming lane is latest-wins and only ever STRICTLY ABOVE the last closed ts: a close
/// supersedes the forming it closes, and a late forming snapshot of an already-closed window
/// is dropped (the double-paint at the boundary this rule exists for).
#[test]
fn direct_store_forming_is_superseded_by_its_close_and_never_resurrects() {
    let s = DirectBarStore::default();
    s.forming("binance", "BTCUSDT", "1m", dbar(60_000, 10.0));
    let (_, forming) = s.series("binance", "BTCUSDT", "1m").unwrap();
    assert_eq!(forming.map(|b| b.ts), Some(60_000), "forming lands even before any close");

    s.close("binance", "BTCUSDT", "1m", dbar(60_000, 10.5));
    let (closed, forming) = s.series("binance", "BTCUSDT", "1m").unwrap();
    assert_eq!(closed.len(), 1);
    assert!(forming.is_none(), "the close supersedes the forming snapshot of its window");

    // A late conflated snapshot of the closed window arrives after the close: dropped.
    s.forming("binance", "BTCUSDT", "1m", dbar(60_000, 10.4));
    let (_, forming) = s.series("binance", "BTCUSDT", "1m").unwrap();
    assert!(forming.is_none(), "a forming at or below the last closed ts must not repaint");

    // The next window's forming is kept, latest-wins.
    s.forming("binance", "BTCUSDT", "1m", dbar(120_000, 11.0));
    s.forming("binance", "BTCUSDT", "1m", dbar(120_000, 11.2));
    let (_, forming) = s.series("binance", "BTCUSDT", "1m").unwrap();
    assert_eq!(forming.map(|b| b.close), Some(11.2));
}

/// The bounded-history contract: closed bars never exceed [`DIRECT_BAR_CLOSED_CAP`], with
/// oldest-first eviction — on the live append path and on an oversized seed alike.
#[test]
fn direct_store_closed_history_is_bounded_with_oldest_first_eviction() {
    let s = DirectBarStore::default();
    s.seed(
        "binance",
        "BTCUSDT",
        "1m",
        (0..DIRECT_BAR_CLOSED_CAP as i64 + 7).map(|i| dbar(i * 60_000, 10.0)).collect(),
    );
    let (closed, _) = s.series("binance", "BTCUSDT", "1m").unwrap();
    assert_eq!(closed.len(), DIRECT_BAR_CLOSED_CAP, "an oversized seed is trimmed to the cap");
    assert_eq!(closed[0].ts, 7 * 60_000, "…dropping the OLDEST bars");

    let next_ts = (DIRECT_BAR_CLOSED_CAP as i64 + 7) * 60_000;
    s.close("binance", "BTCUSDT", "1m", dbar(next_ts, 11.0));
    let (closed, _) = s.series("binance", "BTCUSDT", "1m").unwrap();
    assert_eq!(closed.len(), DIRECT_BAR_CLOSED_CAP, "a live append evicts one from the front");
    assert_eq!(closed[0].ts, 8 * 60_000);
    assert_eq!(closed.last().unwrap().ts, next_ts);
}

/// THE HISTORY SEAM: the backend tail seeds a series exactly ONCE — only while it holds no
/// closed bars — live closes then append through the boundary-ts rule, and a series that
/// already holds venue bars refuses the tail outright (a backend switch can never re-import
/// another backend's tail under accumulated venue truth).
#[test]
fn direct_store_backend_tail_seeds_once_and_never_over_venue_bars() {
    let s = DirectBarStore::default();
    // A forming snapshot from the venue does NOT block the tail seed (hyperliquid's pump
    // emits forming frames from the first second, long before any close exists)…
    s.forming("hyperliquid", "BTC", "1m", dbar(180_000, 12.0));
    let tail = vec![dbar(60_000, 10.0), dbar(120_000, 11.0)];
    assert!(s.seed_backend_tail("hyperliquid", "BTC", "1m", &tail), "empty series: seeded");
    let (closed, forming) = s.series("hyperliquid", "BTC", "1m").unwrap();
    assert_eq!(closed.len(), 2);
    assert_eq!(forming.map(|b| b.ts), Some(180_000), "…and the newer forming survives it");

    // ONCE: a second offer (a later snapshot, or backend B's tail after a switch) refuses.
    assert!(!s.seed_backend_tail("hyperliquid", "BTC", "1m", &[dbar(60_000, 99.0)]));
    let (closed, _) = s.series("hyperliquid", "BTC", "1m").unwrap();
    assert_eq!(closed[0].close, 10.0, "the held history is untouched by the refused offer");

    // The boundary: the venue re-closes the tail's last window — replaced, never duplicated.
    s.close("hyperliquid", "BTC", "1m", dbar(120_000, 11.5));
    let (closed, _) = s.series("hyperliquid", "BTC", "1m").unwrap();
    assert_eq!(closed.len(), 2);
    assert_eq!(closed[1].close, 11.5);

    // A series whose venue seed already landed refuses the tail too.
    s.seed("binance", "BTCUSDT", "1m", vec![dbar(60_000, 20.0)]);
    assert!(!s.seed_backend_tail("binance", "BTCUSDT", "1m", &tail));
    // …and an empty tail seeds nothing (no phantom entry, no generation churn).
    assert!(!s.seed_backend_tail("okx", "BTC-USDT", "1m", &[]));
    assert!(s.series("okx", "BTC-USDT", "1m").is_none());
}

/// `generation` is the fold's dirty probe: every state-changing write bumps it, a refused
/// write does not, and `clear` (the feed-plane teardown) both empties the store and bumps —
/// so the fold that trusts an unchanged generation can never miss a change.
#[test]
fn direct_store_generation_tracks_every_state_change_and_only_those() {
    let s = DirectBarStore::default();
    let g0 = s.generation();
    s.seed("binance", "BTCUSDT", "1m", vec![dbar(60_000, 10.0)]);
    let g1 = s.generation();
    assert!(g1 > g0, "seed bumps");
    s.close("binance", "BTCUSDT", "1m", dbar(30_000, 9.0)); // stale — dropped
    assert_eq!(s.generation(), g1, "a dropped stale close changes nothing and must not bump");
    s.forming("binance", "BTCUSDT", "1m", dbar(60_000, 10.0)); // at the closed ts — dropped
    assert_eq!(s.generation(), g1, "a dropped stale forming must not bump");
    s.close("binance", "BTCUSDT", "1m", dbar(120_000, 11.0));
    let g2 = s.generation();
    assert!(g2 > g1, "a live close bumps");
    s.clear();
    assert!(s.generation() > g2, "clear bumps — the fold refolds the now-empty store");
    assert!(s.keys().is_empty());
}
