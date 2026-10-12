use super::*;
use std::assert_matches;
use vike_model::BookLevel;
use vike_model::BookUpdateKind;

fn quote(ts: i64, bid: f64) -> QuoteTick {
    QuoteTick {
        ts,
        local_ts: 0,
        bid,
        ask: bid + 1.0,
        bid_size: 1.0,
        ask_size: 1.0,
        symbol: "TKN".to_string(),
    }
}

fn trade(ts: i64, price: f64) -> TradeTick {
    TradeTick {
        ts,
        local_ts: 0,
        price,
        size: 1.0,
        is_buyer_maker: false,
        symbol: "TKN".to_string(),
    }
}

#[test]
fn merge_interleaves_by_ts() {
    let quotes = vec![quote(1, 10.0), quote(3, 30.0)];
    let trades = vec![trade(2, 20.0), trade(4, 40.0)];
    let merged = merge_quote_trade(quotes, trades);
    assert_eq!(merged.len(), 4);
    match &merged[0] {
        Tick::Quote(q) => {
            assert_eq!(q.ts, 1);
            assert_eq!(q.bid, 10.0);
        }
        other => panic!("expected Quote at index 0, got {other:?}"),
    }
    match &merged[1] {
        Tick::Trade(t) => {
            assert_eq!(t.ts, 2);
            assert_eq!(t.price, 20.0);
        }
        other => panic!("expected Trade at index 1, got {other:?}"),
    }
    match &merged[2] {
        Tick::Quote(q) => {
            assert_eq!(q.ts, 3);
            assert_eq!(q.bid, 30.0);
        }
        other => panic!("expected Quote at index 2, got {other:?}"),
    }
    match &merged[3] {
        Tick::Trade(t) => {
            assert_eq!(t.ts, 4);
            assert_eq!(t.price, 40.0);
        }
        other => panic!("expected Trade at index 3, got {other:?}"),
    }
}

#[test]
fn merge_equal_ts_quote_first() {
    let quotes = vec![quote(5, 99.0)];
    let trades = vec![trade(5, 100.0)];
    let merged = merge_quote_trade(quotes, trades);
    assert_eq!(merged.len(), 2);
    match &merged[0] {
        Tick::Quote(q) => {
            assert_eq!(q.ts, 5);
            assert_eq!(q.bid, 99.0);
        }
        other => panic!("expected Quote to precede Trade at equal ts, got {other:?}"),
    }
    match &merged[1] {
        Tick::Trade(t) => {
            assert_eq!(t.ts, 5);
            assert_eq!(t.price, 100.0);
        }
        other => panic!("expected Trade second at equal ts, got {other:?}"),
    }
}

#[test]
fn merge_one_empty() {
    // empty trades -> all quotes, order preserved
    let quotes = vec![quote(1, 10.0), quote(2, 20.0)];
    let merged = merge_quote_trade(quotes, vec![]);
    assert_eq!(merged.len(), 2);
    match &merged[0] {
        Tick::Quote(q) => assert_eq!(q.ts, 1),
        other => panic!("expected Quote, got {other:?}"),
    }
    match &merged[1] {
        Tick::Quote(q) => assert_eq!(q.ts, 2),
        other => panic!("expected Quote, got {other:?}"),
    }

    // empty quotes -> all trades, order preserved
    let trades = vec![trade(3, 30.0), trade(4, 40.0)];
    let merged = merge_quote_trade(vec![], trades);
    assert_eq!(merged.len(), 2);
    match &merged[0] {
        Tick::Trade(t) => assert_eq!(t.ts, 3),
        other => panic!("expected Trade, got {other:?}"),
    }
    match &merged[1] {
        Tick::Trade(t) => assert_eq!(t.ts, 4),
        other => panic!("expected Trade, got {other:?}"),
    }
}

#[test]
fn merge_both_empty() {
    let merged = merge_quote_trade(vec![], vec![]);
    assert!(merged.is_empty());
}

/// Same helpers as `quote`/`trade` but with an explicit machine receive stamp.
fn quote_at(ts: i64, local_ts: i64, bid: f64) -> QuoteTick {
    QuoteTick { local_ts, ..quote(ts, bid) }
}

fn trade_at(ts: i64, local_ts: i64, price: f64) -> TradeTick {
    TradeTick { local_ts, ..trade(ts, price) }
}

/// The arrival clock's three documented cases, at the source.
#[test]
fn arrival_clock_falls_back_clamps_and_lags() {
    // unstamped -> venue ts (today's position)
    let unstamped = Tick::Quote(quote_at(1_000, 0, 10.0));
    assert_eq!(tick_venue_ts(&unstamped), 1_000);
    assert_eq!(tick_local_ts(&unstamped), 0);
    assert_eq!(tick_arrival_ts(&unstamped), 1_000);

    // a real lag -> the recorded receive stamp
    let lagged = Tick::Trade(trade_at(1_000, 1_250, 10.0));
    assert_eq!(tick_arrival_ts(&lagged), 1_250);

    // CLOCK SKEW (local before venue — physically impossible) -> clamped UP to the venue ts,
    // so the modelled latency is never negative and never a look-ahead.
    let skewed = Tick::Quote(quote_at(1_000, 400, 10.0));
    assert_eq!(tick_arrival_ts(&skewed), 1_000, "skewed local_ts clamps up to the venue ts");
    assert!(tick_arrival_ts(&skewed) - tick_venue_ts(&skewed) >= 0);

    // a negative stamp is treated as "never stamped", not as a huge negative arrival
    let negative = Tick::Quote(quote_at(1_000, -5, 10.0));
    assert_eq!(tick_arrival_ts(&negative), 1_000);
}

/// OFF-PATH PROOF at the merge level: with every `local_ts` unstamped (the whole existing
/// fixture corpus), the arrival merge IS the venue merge — same ticks, same order, same
/// tie-break. A stable sort whose key equals the already-ascending venue ts is the identity.
#[test]
fn arrival_merge_equals_venue_merge_when_unstamped() {
    let quotes = vec![quote(1, 10.0), quote(3, 30.0), quote(3, 31.0)];
    let trades = vec![trade(2, 20.0), trade(3, 33.0)];
    let books = vec![BookUpdate {
        ts: 3,
        local_ts: 0,
        seq: 1,
        kind: BookUpdateKind::Snapshot,
        tick_size: 0.01,
        bids: vec![BookLevel::new(0.4, 1.0)],
        asks: vec![BookLevel::new(0.6, 1.0)],
        symbol: "TKN".to_string(),
    }];

    let venue = merge_ticks(quotes.clone(), trades.clone(), books.clone());
    let arrival = merge_ticks_by_arrival(quotes, trades, books);
    assert_eq!(venue.len(), arrival.len());
    for (v, a) in venue.iter().zip(&arrival) {
        assert_eq!(tick_venue_ts(v), tick_venue_ts(a));
        assert_eq!(
            std::mem::discriminant(v),
            std::mem::discriminant(a),
            "same tick KIND at the same position (Book/Quote/Trade tie-break preserved)"
        );
    }
}

/// ON with a lagging stamp: the LATE tick is delivered after the one that arrived first, even
/// though the venue stamped it earlier — and its own venue `ts` is untouched (matching is still
/// venue time).
#[test]
fn arrival_merge_reorders_a_lagging_tick() {
    // Venue order: q@1000, q@2000. Arrival order: q@2000 (recv 2100) BEFORE q@1000 (recv 5000).
    let quotes = vec![quote_at(1_000, 5_000, 10.0), quote_at(2_000, 2_100, 20.0)];
    let merged = merge_ticks_by_arrival(quotes, vec![], vec![]);
    assert_eq!(merged.len(), 2);
    assert_eq!(tick_venue_ts(&merged[0]), 2_000, "the tick that ARRIVED first goes first");
    assert_eq!(tick_arrival_ts(&merged[0]), 2_100);
    assert_eq!(tick_venue_ts(&merged[1]), 1_000, "venue ts is NOT rewritten by the re-order");
    assert_eq!(tick_arrival_ts(&merged[1]), 5_000);
}

/// A PARTLY stamped tape: the unstamped tick falls back to its venue ts and keeps its place;
/// only the stamped, genuinely-late tick moves.
#[test]
fn arrival_merge_mixes_stamped_and_unstamped_cleanly() {
    let quotes = vec![
        quote_at(1_000, 0, 10.0),     // unstamped -> arrival 1000
        quote_at(2_000, 9_000, 20.0), // 7s late   -> arrival 9000
        quote_at(3_000, 0, 30.0),     // unstamped -> arrival 3000
    ];
    let merged = merge_ticks_by_arrival(quotes, vec![], vec![]);
    let order: Vec<i64> = merged.iter().map(tick_venue_ts).collect();
    assert_eq!(order, vec![1_000, 3_000, 2_000], "only the late stamped tick moves");
}

/// A SKEWED stamp must not jump the queue: clamped to its venue ts, the tick stays exactly
/// where the venue-ordered merge put it.
#[test]
fn arrival_merge_clamps_skew_instead_of_delivering_early() {
    let quotes = vec![
        quote_at(1_000, 0, 10.0),
        // local_ts claims it arrived at 500, BEFORE the 1000-stamped tick above; clamping to
        // its own venue ts (2000) keeps it second instead of promoting it to first.
        quote_at(2_000, 500, 20.0),
    ];
    let merged = merge_ticks_by_arrival(quotes, vec![], vec![]);
    let order: Vec<i64> = merged.iter().map(tick_venue_ts).collect();
    assert_eq!(order, vec![1_000, 2_000], "a skewed stamp never delivers a tick early");
}

#[test]
fn merge_ticks_equal_ts_book_quote_trade_order() {
    let books = vec![BookUpdate {
        ts: 5,
        local_ts: 0,
        seq: 1,
        kind: BookUpdateKind::Snapshot,
        tick_size: 0.01,
        bids: vec![BookLevel::new(0.4, 1.0)],
        asks: vec![BookLevel::new(0.6, 1.0)],
        symbol: "TKN".to_string(),
    }];
    let merged = merge_ticks(vec![quote(5, 99.0)], vec![trade(5, 100.0)], books);
    assert_matches!(merged[0], Tick::Book(_), "book first at equal ts");
    assert_matches!(merged[1], Tick::Quote(_));
    assert_matches!(merged[2], Tick::Trade(_));
}
