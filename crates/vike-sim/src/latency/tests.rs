use super::*;
use vike_model::OrderKind;

const D: LatencyOrder = LatencyOrder::NONE;

fn ms(x: i64) -> i64 {
    x * 1_000_000
}

#[test]
fn constant_latency_is_flat() {
    let m = ConstantLatency::new(1_500_000, 2_500_000);
    assert_eq!(m.entry(0, &D), 1_500_000);
    assert_eq!(m.entry(i64::MAX / 2, &D), 1_500_000);
    assert_eq!(m.response(42, &D), 2_500_000);
}

#[test]
fn empty_series_is_inert() {
    let m = IntpOrderLatency::new(Vec::<LatencyRow>::new());
    assert_eq!(m.entry(10, &D), 0);
    assert_eq!(m.response(10, &D), 0);
}

#[test]
fn intp_entry_interpolates_linearly() {
    // req at 1ms → entry 1_000_000ns; req at 3ms → entry 3_000_000ns.
    let rows = vec![
        LatencyRow::new(ms(1), ms(1) + 1_000_000, ms(1) + 2_000_000),
        LatencyRow::new(ms(3), ms(3) + 3_000_000, ms(3) + 4_000_000),
    ];
    let m = IntpOrderLatency::new(rows);
    assert_eq!(m.entry(1, &D), 1_000_000);
    assert_eq!(m.entry(3, &D), 3_000_000);
    // halfway between the two request stamps → halfway between the two latencies
    assert_eq!(m.entry(2, &D), 2_000_000);
}

#[test]
fn intp_entry_clamps_at_both_edges() {
    let rows = vec![
        LatencyRow::new(ms(10), ms(10) + 500, ms(10) + 900),
        LatencyRow::new(ms(20), ms(20) + 1_500, ms(20) + 2_900),
    ];
    let m = IntpOrderLatency::new(rows);
    // before the first row — first row's latency, NOT an extrapolation
    assert_eq!(m.entry(0, &D), 500);
    assert_eq!(m.entry(9, &D), 500);
    // after the last row — last row's latency
    assert_eq!(m.entry(20, &D), 1_500);
    assert_eq!(m.entry(1_000, &D), 1_500);
}

#[test]
fn intp_response_interpolates_on_exchange_stamp() {
    // exch at 1ms → response 400ns; exch at 5ms → response 2_400ns.
    let rows = vec![
        LatencyRow::new(ms(1) - 300, ms(1), ms(1) + 400),
        LatencyRow::new(ms(5) - 300, ms(5), ms(5) + 2_400),
    ];
    let m = IntpOrderLatency::new(rows);
    assert_eq!(m.response(1, &D), 400);
    assert_eq!(m.response(5, &D), 2_400);
    assert_eq!(m.response(3, &D), 1_400); // midpoint of the exch-stamp span
    assert_eq!(m.response(0, &D), 400); // clamped low
    assert_eq!(m.response(9, &D), 2_400); // clamped high
}

#[test]
fn rejection_row_reports_negative_entry_latency() {
    // exch_ts <= 0 marks "never reached the matching engine".
    let r = LatencyRow::new(ms(4), 0, ms(4) + 7_000);
    assert!(r.is_rejection());
    assert_eq!(r.entry_ns(), -7_000);
    assert_eq!(r.response_ns(), 7_000); // whole round trip; no exchange stamp to subtract
    let m = IntpOrderLatency::new(vec![r]);
    assert_eq!(m.entry(4, &D), -7_000);
    assert_eq!(m.entry(0, &D), -7_000); // single row clamps both ways
    assert_eq!(m.entry(99, &D), -7_000);
}

#[test]
fn rejection_is_never_blended_with_an_accept() {
    let ok = LatencyRow::new(ms(0), ms(0) + 1_000, ms(0) + 2_000);
    let rej = LatencyRow::new(ms(10), -1, ms(10) + 6_000);
    let m = IntpOrderLatency::new(vec![ok, rej]);
    // A naive blend at the midpoint would produce (1_000 + -6_000)/2 = -2_500 — a
    // *fabricated* rejection. Snap to the nearer row instead.
    assert_eq!(m.entry(0, &D), 1_000);
    assert_eq!(m.entry(4, &D), 1_000); // nearer the accept
    assert_eq!(m.entry(6, &D), -6_000); // nearer the rejection
    assert_eq!(m.entry(10, &D), -6_000);
}

#[test]
fn response_index_excludes_a_mid_series_rejection() {
    // req-sorted rows whose MIDDLE row is a rejection (exch_ts <= 0). Searching the response
    // leg over the req-sorted series would run `partition_point` over the non-monotone key
    // sequence [1ms, -1, 3ms] and could bracket onto the rejection, handing back its whole
    // 8ms round trip as the response latency of an unrelated ACCEPTED event.
    let rows = vec![
        LatencyRow::new(ms(0), ms(1), ms(1) + 1_400),
        LatencyRow::new(ms(2), -1, ms(2) + 8_000), // rejection: no exchange stamp at all
        LatencyRow::new(ms(3), ms(3), ms(3) + 3_400),
    ];
    let m = IntpOrderLatency::new(rows);
    // the response index drops the rejection and is sorted by exch_ts
    assert_eq!(m.response_rows().len(), 2);
    assert_eq!(m.response_rows()[0].exch_ts, ms(1));
    assert_eq!(m.response_rows()[1].exch_ts, ms(3));
    // the rejection's 8_000 never appears: exch 1ms → 1_400, exch 3ms → 3_400, midpoint 2ms
    assert_eq!(m.response(1, &D), 1_400);
    assert_eq!(m.response(3, &D), 3_400);
    assert_eq!(m.response(2, &D), 2_400);
    // ...while the ENTRY leg still sees the rejection (and refuses to blend it)
    assert_eq!(m.entry(2, &D), -8_000);
}

#[test]
fn response_index_is_sorted_by_exchange_stamp_not_request_stamp() {
    // A pipelined pair with NO rejection: the request sent FIRST is acked LAST, so `exch_ts`
    // is not monotone in `req_ts` and the req-sorted series is the wrong search index.
    let rows = vec![
        LatencyRow::new(ms(0), ms(100), ms(100) + 5_000), // sent first, acked last
        LatencyRow::new(ms(1), ms(2), ms(2) + 1_000),     // sent second, acked first
    ];
    let m = IntpOrderLatency::new(rows);
    assert_eq!(m.rows()[0].req_ts, ms(0)); // entry index keeps req order
    assert_eq!(m.response_rows()[0].exch_ts, ms(2)); // response index is exch-sorted
    assert_eq!(m.response(2, &D), 1_000);
    assert_eq!(m.response(100, &D), 5_000);
    // exch 51ms is the midpoint of the [2ms, 100ms] span → midpoint of [1_000, 5_000]
    assert_eq!(m.response(51, &D), 3_000);
    // before the series → the exch-FIRST row (1_000), not the req-first row's 5_000
    assert_eq!(m.response(0, &D), 1_000);
}

#[test]
fn an_all_rejection_series_has_an_inert_response_leg() {
    let m = IntpOrderLatency::new(vec![
        LatencyRow::new(ms(1), 0, ms(1) + 9_000),
        LatencyRow::new(ms(2), -1, ms(2) + 9_000),
    ]);
    assert!(m.response_rows().is_empty());
    assert_eq!(m.response(1, &D), 0); // empty index is inert
    assert_eq!(m.entry(1, &D), -9_000); // the entry leg still reports the rejections
}

#[test]
fn unsorted_rows_are_sorted_on_construction() {
    let m = IntpOrderLatency::new(vec![
        LatencyRow::new(ms(3), ms(3) + 3_000, ms(3) + 4_000),
        LatencyRow::new(ms(1), ms(1) + 1_000, ms(1) + 2_000),
    ]);
    assert_eq!(m.rows()[0].req_ts, ms(1));
    assert_eq!(m.entry(2, &D), 2_000);
}

#[test]
fn duplicate_request_stamps_have_no_gradient() {
    let m = IntpOrderLatency::new(vec![
        LatencyRow::new(ms(2), ms(2) + 100, ms(2) + 200),
        LatencyRow::new(ms(2), ms(2) + 900, ms(2) + 999),
    ]);
    // Both stamps equal: `partition_point` puts ts=2ms past both → high clamp.
    assert_eq!(m.entry(2, &D), 900);
    assert_eq!(m.entry(1, &D), 100); // before the series → first row
}

#[test]
fn gate_defers_then_delivers_in_stamp_then_seq_order() {
    let mut g = LatencyGate::new(&LatencyModelKind::constant(2_000_000, 0)); // 2ms entry
    assert!(g.submit(10, &D, InFlightAction::CancelAll { si: 0 }));
    assert!(g.submit(10, &D, InFlightAction::CancelTagged { tag: "a".into() }));
    assert_eq!(g.in_flight_len(), 2);
    assert!(g.drain_due(11).is_empty()); // 10ms + 2ms = 12ms, not yet
    let due = g.drain_due(12);
    assert_eq!(due.len(), 2);
    assert_eq!(due[0].seq, 0); // submission order preserved within one delivery stamp
    assert_eq!(due[1].seq, 1);
    assert_eq!(g.in_flight_len(), 0);
}

#[test]
fn gate_drops_a_rejected_action() {
    let mut g = LatencyGate::new(&LatencyModelKind::constant(-1, 0));
    assert!(!g.submit(10, &D, InFlightAction::CancelAll { si: 0 }));
    assert_eq!(g.in_flight_len(), 0);
    assert!(g.drain_due(i64::MAX / 2).is_empty());
}

#[test]
fn gate_clamps_negative_response_to_zero() {
    let g = LatencyGate::new(&LatencyModelKind::constant(0, -5));
    assert_eq!(g.response_ns(1, &D), 0);
}

// ---- per-symbol venue hold ----------------------------------------------------------------

fn push(si: usize) -> InFlightAction {
    InFlightAction::Push { si, order: vike_model::WorkingOrder::new(OrderKind::Market, 1, 1.0) }
}

/// THE acceptance bar: an empty hold table (no properties source, or no venue declaring a
/// hold) must produce the EXACT delivery stamps the pre-hold gate produced. Proven by running
/// both constructors over the same submissions and comparing stamps, not by assertion.
#[test]
fn an_empty_hold_table_is_byte_identical_to_no_table() {
    for holds in [Vec::new(), vec![0i64; 3]] {
        let kind = LatencyModelKind::constant(2_000_000, 0);
        let (mut base, mut held) =
            (LatencyGate::new(&kind), LatencyGate::with_holds(&kind, holds.clone()));
        for g in [&mut base, &mut held] {
            assert!(g.submit(10, &D, push(0)));
            assert!(g.submit(11, &D, push(2)));
            assert!(g.submit(12, &D, InFlightAction::CancelTagged { tag: "t".into() }));
        }
        let stamps = |g: &LatencyGate| -> Vec<(i64, u64)> {
            g.queue.iter().map(|f| (f.delivery_ns, f.seq)).collect()
        };
        assert_eq!(stamps(&base), stamps(&held), "holds={holds:?} must not move a stamp");
    }
}

/// The hold is PER SYMBOL and rides ON TOP of the model's entry leg — a two-market tape
/// (crypto up/down at 250 ms, a sports game at 3 s) delays each order by its own market's rule.
#[test]
fn each_symbol_serves_its_own_venue_hold() {
    let holds = vec![
        i64::from(vike_model::POLYMARKET_ITODE_HOLD_MS) * 1_000_000,
        0,
        i64::from(vike_model::POLYMARKET_SPORTS_GAME_HOLD_MS) * 1_000_000,
    ];
    let mut g = LatencyGate::with_holds(&LatencyModelKind::constant(1_000_000, 0), holds);
    assert!(g.submit(1_000, &D, push(0))); // 1ms wire + 250ms hold
    assert!(g.submit(1_000, &D, push(1))); // 1ms wire, no hold
    assert!(g.submit(1_000, &D, push(2))); // 1ms wire + 3000ms hold
    let at = |i: usize| g.queue[i].delivery_ns;
    assert_eq!(at(0), ms(1_000) + 1_000_000 + 250_000_000);
    assert_eq!(at(1), ms(1_000) + 1_000_000);
    assert_eq!(at(2), ms(1_000) + 1_000_000 + 3_000_000_000);
}

/// The symbol-LESS (tagged) variants resolve the single-symbol `HftBroker` market, index 0 —
/// the market a tagged quote actually rests on. See `LatencyGate::hold_ns`'s doc.
#[test]
fn tagged_actions_take_the_hft_symbols_hold() {
    let mut g = LatencyGate::with_holds(&LatencyModelKind::constant(0, 0), vec![250_000_000, 0]);
    let o = vike_model::WorkingOrder::new(OrderKind::Limit, 1, 1.0);
    assert!(g.submit(0, &D, InFlightAction::SubmitTagged { tag: "a".into(), order: o }));
    assert!(g.submit(
        0,
        &D,
        InFlightAction::ModifyTagged { tag: "a".into(), new_qty: None, new_price: None }
    ));
    assert!(g.submit(0, &D, InFlightAction::CancelTagged { tag: "a".into() }));
    for f in &g.queue {
        assert_eq!(f.delivery_ns, 250_000_000, "index 0's hold, not zero");
    }
}

/// A recorded REJECTION is decided before the hold: the venue never saw the request, so it
/// cannot have held it — the action is dropped, not queued 250 ms later.
#[test]
fn a_rejection_is_not_held() {
    let mut g = LatencyGate::with_holds(&LatencyModelKind::constant(-1, 0), vec![3_000_000_000]);
    assert!(!g.submit(10, &D, push(0)));
    assert_eq!(g.in_flight_len(), 0);
}

/// An out-of-range symbol index (defensive — the table is built over the same `symbols` the
/// engine indexes) resolves to no hold rather than panicking on the order path.
#[test]
fn an_unknown_symbol_index_serves_no_hold() {
    let mut g = LatencyGate::with_holds(&LatencyModelKind::constant(0, 0), vec![250_000_000]);
    assert!(g.submit(7, &D, push(9)));
    assert_eq!(g.queue[0].delivery_ns, ms(7));
}
