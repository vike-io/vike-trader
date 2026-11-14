use super::*;

/// Every field the wire renders comes back as it went out.
#[test]
fn a_result_round_trips_through_the_wire() {
    let r = BacktestResult {
        equity_curve: vec![100.0, 101.5, 99.0],
        equity_timestamp: vec![1, 2, 3],
        final_equity: 99.0,
        n_trades: 1,
        per_symbol_pnl: vec![("BTCUSDT".to_string(), -1.0)],
        trades: vec![vike_model::Trade {
            entry_price: 100.0,
            exit_price: 99.0,
            size: 1.0,
            pnl: -1.0,
            fees: 0.1,
            entry_timestamp: 1,
            exit_timestamp: 3,
            symbol: "BTCUSDT".to_string(),
            mae: 0.01,
            mfe: 0.015,
            is_long: true,
        }],
        stale_deferrals: 2,
        session_deferrals: 3,
        data_warnings: vec!["a hole".to_string()],
        funding_paid: -0.25,
        settle_price: Some(66_000.0),
        ..Default::default()
    };
    let back = from_wire_result(to_wire_result(&r));
    assert_eq!(back.equity_curve, r.equity_curve);
    assert_eq!(back.equity_timestamp, r.equity_timestamp);
    assert_eq!(back.final_equity, r.final_equity);
    assert_eq!(back.n_trades, r.n_trades);
    assert_eq!(back.per_symbol_pnl, r.per_symbol_pnl);
    assert_eq!(back.trades, r.trades);
    assert_eq!(back.stale_deferrals, r.stale_deferrals);
    assert_eq!(back.session_deferrals, r.session_deferrals);
    assert_eq!(back.data_warnings, r.data_warnings);
    assert_eq!(back.funding_paid, r.funding_paid);
    assert_eq!(back.settle_price, r.settle_price, "a coin ledger keeps the price that values it");
}

fn drop_of(reason: &str, size: f64) -> vike_model::OrderDrop {
    vike_model::OrderDrop {
        symbol: "BTCUSDT".to_string(),
        reason: reason.to_string(),
        size,
        weight: 1.0,
    }
}

/// A refused order reaches the Studio: the wire groups the drops (one row per distinct drop, its
/// count beside it, first-seen order) and the decode gives back the same drops, so every reader of
/// `dropped` (the report's denials, the Studio's line) counts what the run refused.
#[test]
fn the_refused_orders_cross_grouped_and_come_back_whole() {
    let spot =
        "a spot account cannot sell more than it holds: it has 1, this sells 2; nothing was sent";
    let dropped = vec![
        drop_of(spot, 2.0),
        drop_of("insufficient-margin", 1.0),
        drop_of(spot, 2.0),
        drop_of(spot, 3.0),
    ];
    let r = BacktestResult { dropped: dropped.clone(), ..Default::default() };

    let wire = to_wire_result(&r);
    assert_eq!(
        wire.refused_orders,
        vec![
            (drop_of(spot, 2.0), 2),
            (drop_of("insufficient-margin", 1.0), 1),
            (drop_of(spot, 3.0), 1),
        ],
        "one row per distinct drop, in first-seen order"
    );

    let back = from_wire_result(wire);
    assert_eq!(back.dropped.len(), dropped.len(), "every refused order comes back");
    assert_eq!(
        vike_analytics::zero_trade::aggregate_denials(&back.dropped),
        vike_analytics::zero_trade::aggregate_denials(&dropped),
        "the reasons and their counts are the run's"
    );
    assert!(to_wire_result(&BacktestResult::default()).refused_orders.is_empty());
}

/// The decode allocates no more drops than [`MAX_EXPANDED_REFUSALS`]: a count summed past it (a faulty
/// or hostile server's word) leaves each row once, its reason still shown, while a total at the cap is
/// expanded whole.
#[test]
fn a_refusal_count_past_the_cap_is_not_expanded() {
    let wire = |rows| BacktestResultWire { refused_orders: rows, ..Default::default() };
    let at_cap = from_wire_result(wire(vec![
        (drop_of("a", 1.0), MAX_EXPANDED_REFUSALS as u64 - 1),
        (drop_of("b", 1.0), 1),
    ]));
    assert_eq!(at_cap.dropped.len(), MAX_EXPANDED_REFUSALS, "a total at the cap comes back whole");

    let huge = from_wire_result(wire(vec![(drop_of("a", 1.0), u64::MAX), (drop_of("b", 1.0), 2)]));
    assert_eq!(
        huge.dropped,
        vec![drop_of("a", 1.0), drop_of("b", 1.0)],
        "past the cap: each row once"
    );
}
