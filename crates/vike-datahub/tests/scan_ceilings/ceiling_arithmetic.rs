//! Half 3: each `MIN_*_JSON_BYTES` floor measured against the smallest real row of its kind.

use vike_datahub::server::{
    MIN_BOOK_LEVEL_JSON_BYTES, MIN_COHORT_JSON_BYTES, MIN_EQUITY_JSON_BYTES,
    MIN_EXEC_FILL_JSON_BYTES, MIN_PERP_METRIC_JSON_BYTES, MIN_QUOTE_JSON_BYTES,
    MIN_TRADE_JSON_BYTES,
};

use super::*;

// ------------------------------------------------------------------------------------------------
// Half 3 — each ceiling's arithmetic
// ------------------------------------------------------------------------------------------------

/// One row's cost INSIDE a reply, its separating comma included: the difference between a reply of
/// two and a reply of one.
fn row_cost<T: Clone>(wrap: fn(Vec<T>) -> Response, row: T) -> usize {
    frame_of(&wrap(vec![row.clone(), row.clone()])).len() - frame_of(&wrap(vec![row])).len()
}

/// Each `MIN_*_JSON_BYTES` is a floor on what one real row of its kind costs in its reply, measured
/// with the encoder `write_frame` uses: the shortest real `ts` (13 digits), every float at its
/// shortest spelling (`0.0`), every `Option<f64>` at `Some(0.0)` (shorter than `null`), every string
/// empty, every integer `0`, every boolean `true`. If serde ever wrote a row shorter, a reply of more
/// than its ceiling could fit a frame and the ceiling would refuse something that works — so this
/// measures the figure each compile-time assertion takes on trust, and then tries the LONGER
/// spelling of each field, which must never come out shorter.
#[test]
fn the_smallest_real_row_of_every_kind_is_no_shorter_than_its_ceiling_assumes() {
    const TS: i64 = 1_000_000_000_000;

    let q = QuoteTick {
        ts: TS,
        local_ts: 0,
        bid: 0.0,
        ask: 0.0,
        bid_size: 0.0,
        ask_size: 0.0,
        symbol: String::new(),
    };
    assert_eq!(row_cost(Response::Quotes, q.clone()), MIN_QUOTE_JSON_BYTES);
    for v in [
        QuoteTick { local_ts: TS, ..q.clone() },
        QuoteTick { bid: f64::NAN, ..q.clone() },
        QuoteTick { symbol: "X".into(), ..q.clone() },
    ] {
        assert!(row_cost(Response::Quotes, v.clone()) >= MIN_QUOTE_JSON_BYTES, "{v:?}");
    }

    let t = TradeTick {
        ts: TS,
        local_ts: 0,
        price: 0.0,
        size: 0.0,
        is_buyer_maker: true,
        symbol: String::new(),
    };
    assert_eq!(row_cost(Response::Trades, t.clone()), MIN_TRADE_JSON_BYTES);
    for v in
        [TradeTick { is_buyer_maker: false, ..t.clone() }, TradeTick { price: -0.0, ..t.clone() }]
    {
        assert!(row_cost(Response::Trades, v.clone()) >= MIN_TRADE_JSON_BYTES, "{v:?}");
    }

    // A LEVEL's cost: an event of two levels against the same event of one.
    let one = |price: f64, qty: f64| BookUpdate {
        ts: TS,
        local_ts: 0,
        seq: 0,
        kind: BookUpdateKind::Delta,
        tick_size: 0.0,
        bids: vec![BookLevel { price, qty }],
        asks: Vec::new(),
        symbol: String::new(),
    };
    let level_cost = |price: f64, qty: f64| {
        let mut two = one(price, qty);
        two.bids.push(BookLevel { price, qty });
        frame_of(&Response::BookUpdates(vec![two])).len()
            - frame_of(&Response::BookUpdates(vec![one(price, qty)])).len()
    };
    assert_eq!(level_cost(0.0, 0.0), MIN_BOOK_LEVEL_JSON_BYTES);
    assert!(level_cost(f64::NAN, 1e-7) >= MIN_BOOK_LEVEL_JSON_BYTES);
    // ...and an event of NO levels — one placeholder row in the store — costs far more than a level.
    let mut empty = one(0.0, 0.0);
    empty.bids.clear();
    assert!(row_cost(Response::BookUpdates, empty) > MIN_BOOK_LEVEL_JSON_BYTES);

    let c = CohortRow {
        ts: TS,
        asset: String::new(),
        axis: String::new(),
        cohort: String::new(),
        grading: String::new(),
        label_basis: String::new(),
        long_usd: 0.0,
        total_usd: 0.0,
    };
    assert_eq!(row_cost(Response::Cohort, c.clone()), MIN_COHORT_JSON_BYTES);
    assert!(
        row_cost(Response::Cohort, CohortRow { asset: "BTC".into(), ..c }) >= MIN_COHORT_JSON_BYTES
    );

    let p = PerpMetricRow { ts: TS, premium: 0.0, open_interest: Some(0.0) };
    assert_eq!(row_cost(Response::PerpMetrics, p), MIN_PERP_METRIC_JSON_BYTES);
    let none = PerpMetricRow { open_interest: None, ..p };
    assert_eq!(
        row_cost(Response::PerpMetrics, none),
        MIN_PERP_METRIC_JSON_BYTES + 1,
        "`null` is one byte LONGER than `0.0` — which is why the floor takes Some(0.0)"
    );

    let e = EquitySample {
        ts: TS,
        venue: String::new(),
        equity: 0.0,
        realized: 0.0,
        unrealized: 0.0,
        missing_prices: 0,
    };
    assert_eq!(row_cost(Response::Equity, e.clone()), MIN_EQUITY_JSON_BYTES);
    assert!(
        row_cost(Response::Equity, EquitySample { missing_prices: 7, ..e })
            >= MIN_EQUITY_JSON_BYTES
    );

    let f = ExecFillRow {
        ts: TS,
        trade_id: String::new(),
        client_order_id: String::new(),
        venue: String::new(),
        symbol: String::new(),
        side: 0,
        qty: 0.0,
        px: 0.0,
        commission: 0.0,
        mark_price: Some(0.0),
        liquidity_side: String::new(),
        commission_asset: String::new(),
    };
    assert_eq!(row_cost(Response::ExecFills, f.clone()), MIN_EXEC_FILL_JSON_BYTES);
    for v in [ExecFillRow { mark_price: None, ..f.clone() }, ExecFillRow { side: -1, ..f.clone() }]
    {
        assert!(row_cost(Response::ExecFills, v.clone()) >= MIN_EXEC_FILL_JSON_BYTES, "{v:?}");
    }
}
