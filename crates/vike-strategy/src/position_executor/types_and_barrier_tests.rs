use super::*;

// --- from_bps convenience (golden) ---

#[test]
fn from_bps_converts_against_ref_price() {
    // ref 100: 100bps = 1% → 1.0 offset; 50bps → 0.5; 25bps → 0.25 trailing
    let b = TripleBarrier::from_bps(100.0, Some(100.0), Some(50.0), Some(500), Some(25.0));
    assert_eq!(b.take_profit, Some(1.0));
    assert_eq!(b.stop_loss, Some(0.5));
    assert_eq!(b.time_limit_ms, Some(500));
    assert_eq!(b.trailing, Some(0.25));
    // a None bps leg stays None
    let b2 = TripleBarrier::from_bps(100.0, None, Some(50.0), None, None);
    assert_eq!(b2.take_profit, None);
    assert_eq!(b2.stop_loss, Some(0.5));
    assert_eq!(b2.time_limit_ms, None);
    assert_eq!(b2.trailing, None);
}

// --- defaults ---

#[test]
fn defaults_are_unarmed_and_no_retry() {
    assert_eq!(TripleBarrier::default(), TripleBarrier::none());
    assert_eq!(TripleBarrier::default().take_profit, None);
    assert_eq!(
        PositionExecutorRetryPolicy::default(),
        PositionExecutorRetryPolicy { max_attempts: 1, backoff_ms: 0 }
    );
    assert_eq!(PositionExecutorEntryKind::default(), PositionExecutorEntryKind::Market);
    assert_eq!(PositionExecutorRefreshMode::default(), PositionExecutorRefreshMode::Reprice);
    // PositionExecutorIntent::default is a usable builder base
    let pi = PositionExecutorIntent::default();
    assert_eq!(pi.entry, PositionExecutorEntryKind::Market);
    assert_eq!(pi.retry, PositionExecutorRetryPolicy::default());
    assert_eq!(pi.refresh, None);
}

// --- serde (pure, additive) ---

#[test]
fn position_intent_round_trips() {
    let pi = PositionExecutorIntent {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 0.5,
        entry: PositionExecutorEntryKind::Limit { price: 65_000.0 },
        barriers: TripleBarrier::new(Some(500.0), Some(250.0), Some(60_000), Some(100.0)),
        refresh: Some(PositionExecutorRefreshPolicy::new(
            5_000,
            PositionExecutorRefreshMode::CancelReplace,
        )),
        retry: PositionExecutorRetryPolicy::new(3, 1_000),
    };
    let json = serde_json::to_string(&pi).unwrap();
    let back: PositionExecutorIntent = serde_json::from_str(&json).unwrap();
    assert_eq!(pi, back);
}

#[test]
fn position_intent_minimal_json_is_additive() {
    // only the required identity fields — policy fields fall back to defaults
    let pi: PositionExecutorIntent =
        serde_json::from_str(r#"{"venue":"binance","symbol":"BTCUSDT","side":-1,"qty":1.0}"#)
            .unwrap();
    assert_eq!(pi.entry, PositionExecutorEntryKind::Market);
    assert_eq!(pi.barriers, TripleBarrier::none());
    assert_eq!(pi.refresh, None);
    assert_eq!(pi.retry, PositionExecutorRetryPolicy::default());
}

#[test]
fn entry_kind_serializes_snake_case() {
    assert_eq!(serde_json::to_string(&PositionExecutorEntryKind::Market).unwrap(), "\"market\"");
    assert_eq!(
        serde_json::to_string(&PositionExecutorEntryKind::Limit { price: 100.0 }).unwrap(),
        r#"{"limit":{"price":100.0}}"#,
    );
}

#[test]
fn executor_state_and_barrier_kind_serialize_snake_case() {
    assert_eq!(
        serde_json::to_string(&PositionExecutorState::EntryWorking).unwrap(),
        "\"entry_working\"",
    );
    assert_eq!(serde_json::to_string(&PositionExecutorState::Closing).unwrap(), "\"closing\"");
    assert_eq!(serde_json::to_string(&TripleBarrierKind::TakeProfit).unwrap(), "\"take_profit\"");
    assert_eq!(serde_json::to_string(&TripleBarrierKind::Time).unwrap(), "\"time\"");
}

#[test]
fn triple_barrier_partial_json_is_additive() {
    // only take_profit present → the other legs default to None
    let b: TripleBarrier = serde_json::from_str(r#"{"take_profit":10.0}"#).unwrap();
    assert_eq!(b, TripleBarrier::new(Some(10.0), None, None, None));
}

#[test]
fn executor_outcome_round_trips() {
    let o = PositionExecutorOutcome {
        barrier_hit: Some(TripleBarrierKind::Time),
        entry_timestamp: 1_000,
        exit_timestamp: 61_000,
        entry_price: 100.0,
        exit_price: 100.5,
        realized_pnl: 0.5,
        fees: 0.1,
    };
    let json = serde_json::to_string(&o).unwrap();
    assert_eq!(o, serde_json::from_str::<PositionExecutorOutcome>(&json).unwrap());
    assert_eq!(o.net(), 0.4);
}
