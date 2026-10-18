use super::*;

/// Build an OHLC bar (ts unused by the price barriers).
fn bar(open: f64, high: f64, low: f64, close: f64) -> Bar {
    Bar {
        ts: 0,
        open,
        high,
        low,
        close,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

// --- price helpers (golden) ---

#[test]
fn barrier_prices_long_and_short() {
    // long entered at 100: TP is +10 above, SL is -5 below
    assert_eq!(take_profit_price(100.0, 1, 10.0), 110.0);
    assert_eq!(stop_loss_price(100.0, 1, 5.0), 95.0);
    // short entered at 100: TP is -10 below, SL is +5 above
    assert_eq!(take_profit_price(100.0, -1, 10.0), 90.0);
    assert_eq!(stop_loss_price(100.0, -1, 5.0), 105.0);
}

// --- take-profit ---

#[test]
fn tp_hit_long() {
    let b = TripleBarrier::new(Some(10.0), None, None, None); // tp @ 110
    let mut ext = None;
    // bar rallies through 110
    let hit = evaluate_barriers(100.0, 1, 0, &b, 0, &bar(105.0, 112.0, 104.0, 111.0), &mut ext);
    assert_eq!(hit, Some(BarrierHit { kind: BarrierKind::TakeProfit, exit_px: 110.0 }));
}

#[test]
fn tp_hit_short() {
    let b = TripleBarrier::new(Some(10.0), None, None, None); // tp @ 90
    let mut ext = None;
    // bar dips through 90
    let hit = evaluate_barriers(100.0, -1, 0, &b, 0, &bar(95.0, 96.0, 88.0, 89.0), &mut ext);
    assert_eq!(hit, Some(BarrierHit { kind: BarrierKind::TakeProfit, exit_px: 90.0 }));
}

#[test]
fn tp_gap_open_improves_fill_long() {
    // gap UP through the 110 target: a favourable limit fills at the better open (113), not 110
    let b = TripleBarrier::new(Some(10.0), None, None, None);
    let mut ext = None;
    let hit = evaluate_barriers(100.0, 1, 0, &b, 0, &bar(113.0, 114.0, 112.0, 113.5), &mut ext);
    assert_eq!(hit, Some(BarrierHit { kind: BarrierKind::TakeProfit, exit_px: 113.0 }));
}

// --- stop-loss ---

#[test]
fn sl_hit_long() {
    let b = TripleBarrier::new(None, Some(5.0), None, None); // sl @ 95
    let mut ext = None;
    let hit = evaluate_barriers(100.0, 1, 0, &b, 0, &bar(98.0, 99.0, 94.0, 94.5), &mut ext);
    assert_eq!(hit, Some(BarrierHit { kind: BarrierKind::StopLoss, exit_px: 95.0 }));
}

#[test]
fn sl_hit_short() {
    let b = TripleBarrier::new(None, Some(5.0), None, None); // sl @ 105
    let mut ext = None;
    let hit = evaluate_barriers(100.0, -1, 0, &b, 0, &bar(102.0, 106.0, 101.0, 105.5), &mut ext);
    assert_eq!(hit, Some(BarrierHit { kind: BarrierKind::StopLoss, exit_px: 105.0 }));
}

#[test]
fn sl_gap_open_worsens_fill_long() {
    // gap DOWN through the 95 stop: an adverse stop fills at the worse open (92), not 95
    // (mirrors ConditionalBook::gap_open_fills_adverse)
    let b = TripleBarrier::new(None, Some(5.0), None, None);
    let mut ext = None;
    let hit = evaluate_barriers(100.0, 1, 0, &b, 0, &bar(92.0, 93.0, 91.0, 92.5), &mut ext);
    assert_eq!(hit, Some(BarrierHit { kind: BarrierKind::StopLoss, exit_px: 92.0 }));
}

// --- time barrier (the new leg) ---

#[test]
fn time_barrier_hit_boundary() {
    // deadline = 1000 + 500 = 1500: not before, fires AT and after
    assert!(!time_barrier_hit(1000, 500, 1499));
    assert!(time_barrier_hit(1000, 500, 1500));
    assert!(time_barrier_hit(1000, 500, 1600));
}

#[test]
fn time_barrier_via_evaluate_fires_at_deadline() {
    let b = TripleBarrier::new(None, None, Some(500), None); // deadline entry_ts+500
    let mut ext = None;
    let quiet = bar(100.0, 101.0, 99.0, 100.5); // no price barrier could fire (none armed)
    // before the deadline: nothing
    assert_eq!(evaluate_barriers(100.0, 1, 1000, &b, 1499, &quiet, &mut ext), None);
    // at the deadline: Time fires, exit_px = current mark (bar.close)
    assert_eq!(
        evaluate_barriers(100.0, 1, 1000, &b, 1500, &quiet, &mut ext),
        Some(BarrierHit { kind: BarrierKind::Time, exit_px: 100.5 }),
    );
}

// --- none-hit ---

#[test]
fn none_hit_within_range() {
    let b = TripleBarrier::new(Some(10.0), Some(5.0), None, None); // tp 110, sl 95
    let mut ext = None;
    // bar stays between 95 and 110
    let hit = evaluate_barriers(100.0, 1, 0, &b, 0, &bar(100.0, 108.0, 96.0, 102.0), &mut ext);
    assert_eq!(hit, None);
}

// --- precedence ---

#[test]
fn precedence_stop_before_target_on_a_wide_bar() {
    // GOLDEN anchor: a bar straddling BOTH the 95 stop and the 110 target must resolve to the
    // STOP (assume the adverse leg hit first — no intrabar look-ahead optimism).
    let b = TripleBarrier::new(Some(10.0), Some(5.0), None, None); // tp 110, sl 95
    let mut ext = None;
    let hit = evaluate_barriers(100.0, 1, 0, &b, 0, &bar(100.0, 115.0, 90.0, 112.0), &mut ext);
    assert_eq!(hit, Some(BarrierHit { kind: BarrierKind::StopLoss, exit_px: 95.0 }));
}

#[test]
fn precedence_price_barrier_before_time() {
    // past the deadline AND the bar crosses the stop → report the price barrier, not the timeout.
    let b = TripleBarrier::new(None, Some(5.0), Some(500), None); // sl 95, deadline entry_ts+500
    let mut ext = None;
    let hit = evaluate_barriers(100.0, 1, 1000, &b, 1500, &bar(98.0, 99.0, 94.0, 94.5), &mut ext);
    assert_eq!(hit, Some(BarrierHit { kind: BarrierKind::StopLoss, exit_px: 95.0 }));
}

// --- trailing (ratchet parity vs ConditionalBook) ---

#[test]
fn trailing_ratchets_in_place_then_fires_long() {
    // Replicates vike-core ConditionalBook::trailing_ratchets_in_place_then_fires exactly:
    // long protection, trail 5, extreme seeded 100 → trigger 95.
    let b = TripleBarrier::new(None, None, None, Some(5.0));
    let mut ext = Some(100.0);
    // new high 110 ratchets the extreme; low 101 stays above the OLD trigger 95 → no fire
    assert_eq!(
        evaluate_barriers(100.0, 1, 0, &b, 0, &bar(105.0, 110.0, 101.0, 108.0), &mut ext),
        None,
    );
    assert_eq!(ext, Some(110.0)); // extreme ratcheted
    // trigger is now 105: a dip to 104 fires at 105 (GOLDEN — identical to ConditionalBook)
    assert_eq!(
        evaluate_barriers(100.0, 1, 0, &b, 0, &bar(106.0, 107.0, 104.0, 104.5), &mut ext),
        Some(BarrierHit { kind: BarrierKind::Trailing, exit_px: 105.0 }),
    );
}

#[test]
fn trailing_new_high_bar_cannot_stop_itself_out() {
    // the oracle checks the PRIOR extreme's trigger before ratcheting (mirrors ConditionalBook)
    let b = TripleBarrier::new(None, None, None, Some(5.0));
    let mut ext = Some(100.0);
    // high 120 would imply trigger 115 — but the low 103 is compared to the PRIOR trigger 95: no fire
    assert_eq!(
        evaluate_barriers(100.0, 1, 0, &b, 0, &bar(110.0, 120.0, 103.0, 118.0), &mut ext),
        None,
    );
    assert_eq!(ext, Some(120.0));
}

#[test]
fn trailing_lazy_seeds_from_entry_px() {
    // passing &mut None seeds the extreme from entry_px (100) → trigger 95; a dip to 94 fires at 95
    let b = TripleBarrier::new(None, None, None, Some(5.0));
    let mut ext = None;
    let hit = evaluate_barriers(100.0, 1, 0, &b, 0, &bar(96.0, 97.0, 94.0, 94.5), &mut ext);
    assert_eq!(hit, Some(BarrierHit { kind: BarrierKind::Trailing, exit_px: 95.0 }));
}

#[test]
fn trailing_short_ratchets_down_then_fires() {
    // short protection: buy-stop trailing the LOW. trail 5, extreme seeded 100 → trigger 105.
    let b = TripleBarrier::new(None, None, None, Some(5.0));
    let mut ext = Some(100.0);
    // low 90 ratchets the extreme down; high 96 stays below the OLD trigger 105 → no fire
    assert_eq!(
        evaluate_barriers(100.0, -1, 0, &b, 0, &bar(95.0, 96.0, 90.0, 94.0), &mut ext),
        None,
    );
    assert_eq!(ext, Some(90.0)); // extreme ratcheted DOWN
    // trigger is now 95: a rally to 96 fires at 95
    assert_eq!(
        evaluate_barriers(100.0, -1, 0, &b, 0, &bar(94.0, 96.0, 93.0, 95.0), &mut ext),
        Some(BarrierHit { kind: BarrierKind::Trailing, exit_px: 95.0 }),
    );
}

// --- tick convenience ---

#[test]
fn evaluate_at_price_tick_path() {
    // tp 110, sl 95
    let b = TripleBarrier::new(Some(10.0), Some(5.0), None, None);
    // a tick exactly AT the 110 take-profit fills there
    let mut ext = None;
    assert_eq!(
        evaluate_barriers_at_price(100.0, 1, 0, &b, 0, 110.0, &mut ext),
        Some(BarrierHit { kind: BarrierKind::TakeProfit, exit_px: 110.0 }),
    );
    // a tick BELOW the 95 stop fills at the tick itself — on a degenerate (single-price) bar the
    // crossing price IS the fill (same as ConditionalBook::check_price: min(95, open=94) = 94)
    let mut ext2 = None;
    assert_eq!(
        evaluate_barriers_at_price(100.0, 1, 0, &b, 0, 94.0, &mut ext2),
        Some(BarrierHit { kind: BarrierKind::StopLoss, exit_px: 94.0 }),
    );
    // a tick between the barriers: nothing
    let mut ext3 = None;
    assert_eq!(evaluate_barriers_at_price(100.0, 1, 0, &b, 0, 100.0, &mut ext3), None);
}

// --- from_bps convenience (golden) ---

#[test]
fn from_bps_converts_against_ref_px() {
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
    assert_eq!(RetryPolicy::default(), RetryPolicy { max_attempts: 1, backoff_ms: 0 });
    assert_eq!(EntryKind::default(), EntryKind::Market);
    assert_eq!(RefreshMode::default(), RefreshMode::Reprice);
    // PositionIntent::default is a usable builder base
    let pi = PositionIntent::default();
    assert_eq!(pi.entry, EntryKind::Market);
    assert_eq!(pi.retry, RetryPolicy::default());
    assert_eq!(pi.refresh, None);
}

// --- serde (pure, additive) ---

#[test]
fn position_intent_round_trips() {
    let pi = PositionIntent {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 0.5,
        entry: EntryKind::Limit { price: 65_000.0 },
        barriers: TripleBarrier::new(Some(500.0), Some(250.0), Some(60_000), Some(100.0)),
        refresh: Some(RefreshPolicy::new(5_000, RefreshMode::CancelReplace)),
        retry: RetryPolicy::new(3, 1_000),
    };
    let json = serde_json::to_string(&pi).unwrap();
    let back: PositionIntent = serde_json::from_str(&json).unwrap();
    assert_eq!(pi, back);
}

#[test]
fn position_intent_minimal_json_is_additive() {
    // only the required identity fields — policy fields fall back to defaults
    let pi: PositionIntent =
        serde_json::from_str(r#"{"venue":"binance","symbol":"BTCUSDT","side":-1,"qty":1.0}"#)
            .unwrap();
    assert_eq!(pi.entry, EntryKind::Market);
    assert_eq!(pi.barriers, TripleBarrier::none());
    assert_eq!(pi.refresh, None);
    assert_eq!(pi.retry, RetryPolicy::default());
}

#[test]
fn entry_kind_serializes_snake_case() {
    assert_eq!(serde_json::to_string(&EntryKind::Market).unwrap(), "\"market\"");
    assert_eq!(
        serde_json::to_string(&EntryKind::Limit { price: 100.0 }).unwrap(),
        r#"{"limit":{"price":100.0}}"#,
    );
}

#[test]
fn executor_state_and_barrier_kind_serialize_snake_case() {
    assert_eq!(serde_json::to_string(&ExecutorState::EntryWorking).unwrap(), "\"entry_working\"",);
    assert_eq!(serde_json::to_string(&ExecutorState::Closing).unwrap(), "\"closing\"");
    assert_eq!(serde_json::to_string(&BarrierKind::TakeProfit).unwrap(), "\"take_profit\"");
    assert_eq!(serde_json::to_string(&BarrierKind::Time).unwrap(), "\"time\"");
}

#[test]
fn triple_barrier_partial_json_is_additive() {
    // only take_profit present → the other legs default to None
    let b: TripleBarrier = serde_json::from_str(r#"{"take_profit":10.0}"#).unwrap();
    assert_eq!(b, TripleBarrier::new(Some(10.0), None, None, None));
}

#[test]
fn executor_outcome_round_trips() {
    let o = ExecutorOutcome {
        barrier_hit: Some(BarrierKind::Time),
        entry_ts: 1_000,
        exit_ts: 61_000,
        entry_px: 100.0,
        exit_px: 100.5,
        realized_pnl: 0.5,
        fees: 0.1,
    };
    let json = serde_json::to_string(&o).unwrap();
    assert_eq!(o, serde_json::from_str::<ExecutorOutcome>(&json).unwrap());
    assert_eq!(o.net(), 0.4);
}
