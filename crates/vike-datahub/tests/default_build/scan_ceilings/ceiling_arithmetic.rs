//! Half 3: each `MIN_*_JSON_BYTES` floor measured against the smallest real row of its kind.

use vike_datahub::server::{
    MIN_BOOK_LEVEL_JSON_BYTES, MIN_CHAIN_JSON_BYTES, MIN_COHORT_JSON_BYTES, MIN_EQUITY_JSON_BYTES,
    MIN_EXECUTION_FILL_JSON_BYTES, MIN_EXECUTION_ORDER_JSON_BYTES, MIN_FINANCIAL_FACT_JSON_BYTES,
    MIN_FORECAST_JSON_BYTES, MIN_INSTITUTIONAL_HOLDING_JSON_BYTES, MIN_PERP_METRIC_JSON_BYTES,
    MIN_QUOTE_JSON_BYTES, MIN_TEXT_JSON_BYTES, MIN_TEXT_SCORE_JSON_BYTES, MIN_TRADE_JSON_BYTES,
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
    const TIMESTAMP: i64 = 1_000_000_000_000;

    let q = TickQuote {
        timestamp: TIMESTAMP,
        local_timestamp: 0,
        bid: 0.0,
        ask: 0.0,
        bid_size: 0.0,
        ask_size: 0.0,
        symbol: Default::default(),
    };
    assert_eq!(row_cost(Response::Quotes, q.clone()), MIN_QUOTE_JSON_BYTES);
    for v in [
        TickQuote { local_timestamp: TIMESTAMP, ..q.clone() },
        TickQuote { bid: f64::NAN, ..q.clone() },
        TickQuote { symbol: "X".into(), ..q.clone() },
    ] {
        assert!(row_cost(Response::Quotes, v.clone()) >= MIN_QUOTE_JSON_BYTES, "{v:?}");
    }

    let t = TickTrade {
        timestamp: TIMESTAMP,
        local_timestamp: 0,
        price: 0.0,
        size: 0.0,
        is_buyer_maker: true,
        symbol: Default::default(),
    };
    assert_eq!(row_cost(Response::Trades, t.clone()), MIN_TRADE_JSON_BYTES);
    for v in
        [TickTrade { is_buyer_maker: false, ..t.clone() }, TickTrade { price: -0.0, ..t.clone() }]
    {
        assert!(row_cost(Response::Trades, v.clone()) >= MIN_TRADE_JSON_BYTES, "{v:?}");
    }

    // A LEVEL's cost: an event of two levels against the same event of one.
    let one = |price: f64, qty: f64| L2BookUpdate {
        timestamp: TIMESTAMP,
        local_timestamp: 0,
        sequence: 0,
        kind: L2BookUpdateKind::Delta,
        tick_size: 0.0,
        bids: vec![L2BookLevel { price, qty }],
        asks: Vec::new(),
        symbol: String::new(),
    };
    let level_cost = |price: f64, qty: f64| {
        let mut two = one(price, qty);
        two.bids.push(L2BookLevel { price, qty });
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
        ts: TIMESTAMP,
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

    // Every string empty and `published_ms` 0: the shortest text a recorder could store.
    let text = vike_data::TextRow::new("", "", TIMESTAMP);
    assert_eq!(row_cost(Response::Texts, text.clone()), MIN_TEXT_JSON_BYTES);
    for v in [
        vike_data::TextRow { published_ms: TIMESTAMP, ..text.clone() },
        vike_data::TextRow { kind: "news".into(), ..text.clone() },
    ] {
        assert!(row_cost(Response::Texts, v.clone()) >= MIN_TEXT_JSON_BYTES, "{v:?}");
    }

    // The SEC tables: every string empty, every number zero.
    let fact = vike_data::FinancialFactRow {
        filed_ms: TIMESTAMP,
        cik: String::new(),
        entity: String::new(),
        taxonomy: String::new(),
        concept: String::new(),
        unit: String::new(),
        period_start: String::new(),
        period_end: String::new(),
        value: 0.0,
        accession: String::new(),
        fiscal_year: String::new(),
        fiscal_period: String::new(),
        form: String::new(),
        frame: String::new(),
    };
    assert_eq!(row_cost(Response::FinancialFacts, fact.clone()), MIN_FINANCIAL_FACT_JSON_BYTES);
    let wider = vike_data::FinancialFactRow { value: 1.5e10, concept: "Revenues".into(), ..fact };
    assert!(row_cost(Response::FinancialFacts, wider) >= MIN_FINANCIAL_FACT_JSON_BYTES);
    let line = vike_data::InstitutionalHoldingRow {
        filed_ms: TIMESTAMP,
        accession: String::new(),
        filer_cik: String::new(),
        filer_name: String::new(),
        period: String::new(),
        submission_type: String::new(),
        amendment_type: String::new(),
        issuer: String::new(),
        class_title: String::new(),
        cusip: String::new(),
        value_usd: 0,
        shares: 0,
        shares_type: String::new(),
        put_call: String::new(),
    };
    assert_eq!(
        row_cost(Response::InstitutionalHoldings, line.clone()),
        MIN_INSTITUTIONAL_HOLDING_JSON_BYTES
    );
    let wider =
        vike_data::InstitutionalHoldingRow { shares: 1_000, cusip: "037833100".into(), ..line };
    assert!(
        row_cost(Response::InstitutionalHoldings, wider) >= MIN_INSTITUTIONAL_HOLDING_JSON_BYTES
    );

    // Every string empty, `chunk` and `scored_ms` 0: the shortest score a `text-scores` import could
    // store.
    let score = vike_data::TextScoreRow {
        received_ms: TIMESTAMP,
        text_id: String::new(),
        chunk: 0,
        scored_ms: 0,
        model: String::new(),
        values: String::new(),
    };
    assert_eq!(row_cost(Response::TextScores, score.clone()), MIN_TEXT_SCORE_JSON_BYTES);
    let scored = vike_data::TextScoreRow { scored_ms: TIMESTAMP, values: "0.5".into(), ..score };
    assert!(
        row_cost(Response::TextScores, scored.clone()) >= MIN_TEXT_SCORE_JSON_BYTES,
        "{scored:?}"
    );

    let p = PerpMetricRow { ts: TIMESTAMP, premium: 0.0, open_interest: Some(0.0) };
    assert_eq!(row_cost(Response::PerpMetrics, p), MIN_PERP_METRIC_JSON_BYTES);
    let none = PerpMetricRow { open_interest: None, ..p };
    assert_eq!(
        row_cost(Response::PerpMetrics, none),
        MIN_PERP_METRIC_JSON_BYTES + 1,
        "`null` is one byte LONGER than `0.0` — which is why the floor takes Some(0.0)"
    );

    let chain = vike_data::ChainRow {
        ts: TIMESTAMP,
        underlying: String::new(),
        instrument: String::new(),
        expiry_ms: 0,
        strike: 0.0,
        kind: vike_marketdata::OptionKind::Put,
        bid: Some(0.0),
        ask: Some(0.0),
        mark: Some(0.0),
        iv: Some(0.0),
        open_interest: Some(0.0),
        volume: Some(0.0),
        delta: Some(0.0),
        gamma: Some(0.0),
        theta: Some(0.0),
        vega: Some(0.0),
    };
    assert_eq!(row_cost(Response::Chain, chain.clone()), MIN_CHAIN_JSON_BYTES);
    for v in [
        vike_data::ChainRow { kind: vike_marketdata::OptionKind::Call, ..chain.clone() },
        vike_data::ChainRow { bid: None, expiry_ms: TIMESTAMP, ..chain.clone() },
    ] {
        assert!(row_cost(Response::Chain, v.clone()) >= MIN_CHAIN_JSON_BYTES, "{v:?}");
    }

    let e = EquitySample {
        timestamp: TIMESTAMP,
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
        ts: TIMESTAMP,
        trade_id: String::new(),
        order_client_id: String::new(),
        venue: String::new(),
        symbol: String::new(),
        side: 0,
        qty: 0.0,
        px: 0.0,
        commission: 0.0,
        mark_price: Some(0.0),
        liquidity_side: String::new(),
        commission_asset: String::new(),
        venue_order_id: None,
        account: None,
        position_side: None,
        corrects_trade_id: None,
        correction_seq: None,
        mount_id: None,
        received_ms: None,
        ownership: None,
        journal_sequence: None,
        journal_generation: None,
    };
    assert_eq!(row_cost(Response::ExecFills, f.clone()), MIN_EXECUTION_FILL_JSON_BYTES);
    for v in [ExecFillRow { mark_price: None, ..f.clone() }, ExecFillRow { side: -1, ..f.clone() }]
    {
        assert!(row_cost(Response::ExecFills, v.clone()) >= MIN_EXECUTION_FILL_JSON_BYTES, "{v:?}");
    }
    // …and the additive columns only ever LENGTHEN a fill: `None` is skipped, `Some` adds a key.
    for v in [
        ExecFillRow { venue_order_id: Some(String::new()), ..f.clone() },
        ExecFillRow { account: Some(String::new()), ..f.clone() },
        ExecFillRow { position_side: Some(String::new()), ..f.clone() },
        ExecFillRow { corrects_trade_id: Some(String::new()), ..f.clone() },
        ExecFillRow { correction_seq: Some(0), ..f.clone() },
        ExecFillRow { mount_id: Some(String::new()), ..f.clone() },
        ExecFillRow { received_ms: Some(0), ..f.clone() },
    ] {
        assert!(row_cost(Response::ExecFills, v.clone()) > MIN_EXECUTION_FILL_JSON_BYTES, "{v:?}");
    }

    let o = ExecOrderRow {
        ts: TIMESTAMP,
        order_client_id: String::new(),
        venue: String::new(),
        symbol: String::new(),
        side: 0,
        qty: 0.0,
        order_type: String::new(),
        status: String::new(),
        price: Some(0.0),
        trigger_price: Some(0.0),
        venue_order_id: Some(String::new()),
        filled_qty: 0.0,
        avg_fill_px: 0.0,
        account: None,
        mount_id: None,
        ownership: None,
        journal_sequence: None,
        journal_generation: None,
        reason: None,
    };
    assert_eq!(row_cost(Response::ExecOrders, o.clone()), MIN_EXECUTION_ORDER_JSON_BYTES);
    for v in [
        ExecOrderRow { price: None, ..o.clone() },
        ExecOrderRow { trigger_price: None, ..o.clone() },
        ExecOrderRow { venue_order_id: None, ..o.clone() },
        ExecOrderRow { side: -1, ..o.clone() },
        ExecOrderRow { account: Some(String::new()), ..o.clone() },
        ExecOrderRow { mount_id: Some(String::new()), ..o.clone() },
    ] {
        assert!(
            row_cost(Response::ExecOrders, v.clone()) >= MIN_EXECUTION_ORDER_JSON_BYTES,
            "{v:?}"
        );
    }

    let p = vike_data::ExecForecastRow {
        ts: TIMESTAMP,
        mount_id: String::new(),
        value: 0.0,
        horizon_ms: 0,
        confidence: Some(0.0),
    };
    assert_eq!(row_cost(Response::Forecasts, p.clone()), MIN_FORECAST_JSON_BYTES);
    for v in [
        vike_data::ExecForecastRow { confidence: None, ..p.clone() },
        vike_data::ExecForecastRow { value: -0.0, ..p.clone() },
        vike_data::ExecForecastRow { horizon_ms: 60_000, ..p.clone() },
    ] {
        assert!(row_cost(Response::Forecasts, v.clone()) >= MIN_FORECAST_JSON_BYTES, "{v:?}");
    }
}
