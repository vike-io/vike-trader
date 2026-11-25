use super::*;
use crate::recon::types::ReconcilePolicy;
use std::assert_matches;
use vike_model::FillReport;

/// An external (order-client-id-less) taker fill on binance BTCUSDT; `client.rs`'s unit tests use it too.
pub(in crate::recon) fn ext_fill(trade_id: &'static str) -> FillReport {
    FillReport {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        trade_id: trade_id.into(),
        venue_order_id: "v9".into(),
        order_client_id: None,
        side: 1,
        last_qty: 1.0,
        last_price: 100.0,
        commission: 0.0,
        commission_asset: "USDT".into(),
        liquidity_side: LiquiditySide::Taker,
        timestamp: 5,
    }
}

/// ⚠ **A synthesized fill snaps onto the tick the venue trades at that price.** The real Deribit
/// BTC-option grid: `0.01234` is above the `0.005` boundary, so it trades on `0.0005`; on the scalar
/// tick it read `0.0123`, a price the venue's own wire could not have carried. With no scheme the
/// rounding is the scalar tick, bit for bit.
#[test]
fn a_synthesized_fill_snaps_onto_the_tier_of_its_price() {
    let scheme = vike_model::GridTickScheme::new(
        0.0001,
        &[vike_model::GridTickTier { above_price: 0.005, tick_size: 0.0005 }],
    )
    .expect("valid grid");
    let flat =
        vike_model::SymbolProperties { tick_size: 0.0001, step_size: 0.1, ..Default::default() };
    let tiered = flat.with_tick_scheme(scheme);

    let pit = move |_: &str, _: &str, _: i64| Some(tiered);
    let (px, qty) = snap_pit(Some(&pit), "deribit", "BTC-C", 0, 0.01234, 1.07);
    assert!((px - 0.0125).abs() < 1e-12, "the 0.0005 tier: {px}");
    assert!((qty - 1.1).abs() < 1e-12, "the step is untouched: {qty}");

    let pit = move |_: &str, _: &str, _: i64| Some(flat);
    let (px, _) = snap_pit(Some(&pit), "deribit", "BTC-C", 0, 0.01234, 1.07);
    assert_eq!(px.to_bits(), vike_model::round_to(0.01234, Some(0.0001)).to_bits());
}

#[test]
fn external_missing_fill_synthesizes_accept_then_fill() {
    let d = vec![ReconcileDivergence::MissingFill(ext_fill("t1"))];
    let r = resolve(d, &ReconcilePolicy::default(), None, None);
    assert_eq!(r.events.len(), 2);
    assert_matches!(r.events[0], Event::OrderAccepted(_));
    assert_matches!(r.events[1], Event::Fill(_));
}

#[test]
fn journal_divergence_always_alerts_under_every_policy() {
    // A three-way persistence-bug signal never resolves to an event: under EVERY policy it must
    // surface as a single investigative alert with no proposed events (the always-surface rule).
    let policies = [
        ReconcilePolicy::default(), // Synthesize
        ReconcilePolicy { default: ReconcileMode::Quarantine, ..Default::default() }, // Quarantine
        ReconcilePolicy::hybrid(),  // Hybrid
    ];
    for policy in &policies {
        let d = vec![ReconcileDivergence::JournalDivergence {
            detail: "journal has fill t9, live account lacks it".into(),
            recover_order: None,
        }];
        let r = resolve(d, policy, None, None);
        assert!(r.events.is_empty(), "journal divergence synthesizes NO events ({policy:?})");
        assert_eq!(r.alerts.len(), 1, "exactly one alert ({policy:?})");
        assert_eq!(r.alerts[0].kind, ReconcileDivergenceKind::JournalDivergence);
        assert!(
            r.alerts[0].proposed_events.is_empty(),
            "investigative alert carries no proposed events (nothing to auto-apply)"
        );
        assert!(
            r.alerts[0].recover_orders.is_empty(),
            "a fill-loss divergence carries no re-registration ({policy:?})"
        );
        assert!(r.alerts[0].detail.contains("t9"), "detail is preserved");
    }
}

#[test]
fn order_loss_journal_divergence_carries_recovery_under_every_policy() {
    // The ORDER-loss case (recover_order = Some) always surfaces as an alert too, but now the
    // alert carries the venue order to RE-REGISTER on operator confirm — under EVERY policy.
    let report = vike_model::OrderStatusReport {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        venue_order_id: "v1".into(),
        order_client_id: Some("c-lost".into()),
        side: 1,
        order_type: vike_model::OrderType::Limit,
        qty: 1.0,
        filled_qty: 0.0,
        average_price: 0.0,
        status: vike_model::OrderStatus::Accepted,
        timestamp: 3,
        price: None,
        trigger_price: None,
        trail: None,
        trail_percent: None,
    };
    for policy in [ReconcilePolicy::default(), ReconcilePolicy::hybrid()] {
        let d = vec![ReconcileDivergence::JournalDivergence {
            detail: "order c-lost live in journal, local lost it".into(),
            recover_order: Some(Box::new(report.clone())),
        }];
        let r = resolve(d, &policy, None, None);
        assert!(r.events.is_empty(), "never auto-folds ({policy:?})");
        assert_eq!(r.alerts.len(), 1);
        assert_eq!(r.alerts[0].recover_orders.len(), 1, "carries the order to re-register");
        assert_eq!(r.alerts[0].recover_orders[0].order_client_id.as_deref(), Some("c-lost"));
    }
}

#[test]
fn quarantine_holds_events_as_alert() {
    let policy = ReconcilePolicy { default: ReconcileMode::Quarantine, ..Default::default() };
    let d = vec![ReconcileDivergence::MissingFill(ext_fill("t1"))];
    let r = resolve(d, &policy, None, None);
    assert!(r.events.is_empty());
    assert_eq!(r.alerts.len(), 1);
    assert_eq!(r.alerts[0].proposed_events.len(), 2);
}

#[test]
fn external_order_client_id_is_deterministic() {
    assert_eq!(external_order_client_id("binance", "v9"), "EXT-binance-v9");
}

fn pos_drift(local_quantity: f64, venue_qty: f64) -> ReconcileDivergence {
    ReconcileDivergence::PositionDrift {
        report: vike_model::PositionStatusReport {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            position_side: PositionSide::Both,
            qty: venue_qty,
            average_price: 100.0,
            timestamp: 7,
            margin_mode: Default::default(),
            isolated_margin: None,
            delta: None,
        },
        local_quantity,
    }
}

#[test]
fn same_sign_drift_is_one_fill() {
    // local +1, venue +3 → one synthetic buy of +2
    let r = resolve(vec![pos_drift(1.0, 3.0)], &ReconcilePolicy::default(), None, None);
    let fills: Vec<_> = r.events.iter().filter(|e| matches!(e, Event::Fill(_))).collect();
    assert_eq!(fills.len(), 1);
    if let Event::Fill(f) = fills[0] {
        assert_eq!(f.side, 1);
        assert_eq!(f.last_qty, 2.0);
    }
}

#[test]
fn zero_crossing_drift_is_two_fills() {
    // local +2, venue -1 → close +2 (sell 2), then open -1 (sell 1) = two legs
    let r = resolve(vec![pos_drift(2.0, -1.0)], &ReconcilePolicy::default(), None, None);
    let fills: Vec<_> = r
        .events
        .iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some(f),
            _ => None,
        })
        .collect();
    assert_eq!(fills.len(), 2);
    assert_eq!(fills[0].last_qty, 2.0); // close leg
    assert_eq!(fills[1].last_qty, 1.0); // open leg
    assert!(fills.iter().all(|f| f.side == -1));
}

// --- within-pass double-count netting (fills vs position-drift legs) ---

fn missing_fill(side: i32, qty: f64, trade_id: &'static str) -> ReconcileDivergence {
    ReconcileDivergence::MissingFill(vike_model::FillReport {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        trade_id: trade_id.into(),
        venue_order_id: "v-mf".into(),
        order_client_id: None,
        side,
        last_qty: qty,
        last_price: 100.0,
        commission: 0.0,
        commission_asset: "USDT".into(),
        liquidity_side: LiquiditySide::Taker,
        timestamp: 5,
    })
}

fn pos_drift_side(local_quantity: f64, venue_qty: f64, side: PositionSide) -> ReconcileDivergence {
    ReconcileDivergence::PositionDrift {
        report: vike_model::PositionStatusReport {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            position_side: side,
            qty: venue_qty,
            average_price: 100.0,
            timestamp: 7,
            margin_mode: Default::default(),
            isolated_margin: None,
            delta: None,
        },
        local_quantity,
    }
}

#[test]
fn missing_fill_fully_explains_position_drift_no_extra_leg() {
    // venue holds +1 BTC; local is flat; the +1 is entirely explained by one missed fill.
    // resolve must emit ONLY the fill's own events (accept + fill) — no EXT-POS-* leg.
    let d = vec![missing_fill(1, 1.0, "t-mf1"), pos_drift(0.0, 1.0)];
    let r = resolve(d, &ReconcilePolicy::default(), None, None);
    let fills: Vec<_> = r
        .events
        .iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some(f),
            _ => None,
        })
        .collect();
    assert_eq!(fills.len(), 1, "expected only the MissingFill's own Fill, got {fills:?}");
    assert_eq!(fills[0].trade_id.as_str(), "t-mf1");
    assert!(!fills.iter().any(|f| f.trade_id.starts_with("EXT-POS-")));
    // OrderAccepted (for the external fill's order) + Fill = 2 total events.
    assert_eq!(r.events.len(), 2, "{:?}", r.events);
}

#[test]
fn missing_fill_partially_explains_position_drift_residual_leg() {
    // venue holds +3 BTC; local is flat; a +1 missed fill explains part of it, leaving a
    // residual of +2 that must still be synthesized as a position leg.
    let d = vec![missing_fill(1, 1.0, "t-mf1"), pos_drift(0.0, 3.0)];
    let r = resolve(d, &ReconcilePolicy::default(), None, None);
    let fills: Vec<_> = r
        .events
        .iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some(f),
            _ => None,
        })
        .collect();
    // The MissingFill's own Fill, plus exactly one residual position leg.
    assert_eq!(fills.len(), 2, "{fills:?}");
    let pos_legs: Vec<_> = fills.iter().filter(|f| f.trade_id.starts_with("EXT-POS-")).collect();
    assert_eq!(pos_legs.len(), 1);
    assert_eq!(pos_legs[0].side, 1);
    assert_eq!(pos_legs[0].last_qty, 2.0);
}

// --- pre-window residual sequencing (partial-lookback lifecycle truncation) ---
//
// The UNEXPLAINED residual is older than every recovered fill, so its legs anchor BEFORE the
// earliest one and fold FIRST (`position_events`'s doc).

#[test]
fn truncated_lifecycle_close_leg_folds_before_recovered_fills() {
    // local +10; window recovered a buy +5 (ts 5); venue net +5. The unseen −10 close happened
    // before the window: one pre-anchored close leg (side −1, qty 10, ts 4) folds FIRST, then
    // the recovered fill opens the fresh +5.
    let d = vec![missing_fill(1, 5.0, "t-mf1"), pos_drift(10.0, 5.0)];
    let r = resolve(d, &ReconcilePolicy::default(), None, None);
    let fills: Vec<_> = r
        .events
        .iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some(f),
            _ => None,
        })
        .collect();
    assert_eq!(fills.len(), 2, "{fills:?}");
    assert!(
        fills[0].trade_id.starts_with("EXT-POS-"),
        "pre-window close leg must fold before the recovered fill: {fills:?}"
    );
    assert_eq!(fills[0].side, -1);
    assert_eq!(fills[0].last_qty, 10.0, "full close to flat, not a blended remainder");
    assert_eq!(fills[0].timestamp, 4, "anchored before the earliest recovered fill (ts 5)");
    assert_eq!(fills[1].trade_id.as_str(), "t-mf1");
}

#[test]
fn pre_window_residual_crossing_splits_at_flat_before_fills() {
    // local +2; window recovered a buy +2 (ts 5); venue net −1. Pre-window truth: +2 crossed
    // flat to −3 (then the recovered +2 brings it to −1). TWO pre-anchored legs — close 2,
    // open 3, both ts 4, both sells — fold before the recovered fill.
    let d = vec![missing_fill(1, 2.0, "t-mf2"), pos_drift(2.0, -1.0)];
    let r = resolve(d, &ReconcilePolicy::default(), None, None);
    let fills: Vec<_> = r
        .events
        .iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some(f),
            _ => None,
        })
        .collect();
    assert_eq!(fills.len(), 3, "{fills:?}");
    assert!(fills[0].trade_id.starts_with("EXT-POS-"));
    assert_eq!(
        (fills[0].side, fills[0].last_qty, fills[0].timestamp),
        (-1, 2.0, 4),
        "close to flat"
    );
    assert!(fills[1].trade_id.starts_with("EXT-POS-"));
    assert_eq!((fills[1].side, fills[1].last_qty, fills[1].timestamp), (-1, 3.0, 4), "open short");
    assert_eq!(fills[2].trade_id.as_str(), "t-mf2", "recovered fill folds last: −3 + 2 = −1");
}

#[test]
fn hedge_mode_position_report_is_not_netted_against_fills() {
    // A `Both`-side MissingFill does not cleanly map onto a hedge-mode Long/Short bucket, so
    // netting is intentionally skipped: the position leg is synthesized against the RAW
    // local_quantity.
    let d = vec![missing_fill(1, 1.0, "t-mf1"), pos_drift_side(0.0, 1.0, PositionSide::Long)];
    let r = resolve(d, &ReconcilePolicy::default(), None, None);
    let fills: Vec<_> = r
        .events
        .iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some(f),
            _ => None,
        })
        .collect();
    // The MissingFill's Fill, PLUS an un-netted position leg for the full venue_qty (1.0).
    assert_eq!(fills.len(), 2, "{fills:?}");
    let pos_legs: Vec<_> = fills.iter().filter(|f| f.trade_id.starts_with("EXT-POS-")).collect();
    assert_eq!(pos_legs.len(), 1);
    assert_eq!(pos_legs[0].last_qty, 1.0);
}

// --- generate_missing_orders / UnknownOrder adoption ---

/// Owned backing for an [`ReconcileAdoptContext`] (which borrows its two sets).
#[derive(Default)]
struct Adopt {
    fills: HashSet<String>,
    seen: HashSet<String>,
}

impl Adopt {
    fn ctx(&self) -> ReconcileAdoptContext<'_> {
        ReconcileAdoptContext { pass_fill_order_ids: &self.fills, seen_trade_ids: &self.seen }
    }
}

fn unknown_order_report(
    order_client_id: Option<&str>,
    filled_qty: f64,
    side: i32,
    status: vike_model::OrderStatus,
) -> vike_model::OrderStatusReport {
    vike_model::OrderStatusReport {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        venue_order_id: "v-unk".into(),
        order_client_id: order_client_id.map(String::from),
        side,
        order_type: vike_model::OrderType::Limit,
        qty: 2.0,
        filled_qty,
        average_price: if filled_qty != 0.0 { 100.0 } else { 0.0 },
        status,
        timestamp: 11,
        price: None,
        trigger_price: None,
        trail: None,
        trail_percent: None,
    }
}

fn unknown_order(order_client_id: Option<&str>, filled_qty: f64, side: i32) -> ReconcileDivergence {
    // Terminal FILLED report — the adoption case (when no fill reports co-occur in-pass).
    ReconcileDivergence::UnknownOrder(unknown_order_report(
        order_client_id,
        filled_qty,
        side,
        vike_model::OrderStatus::Filled,
    ))
}

fn fill_events(r: &ReconcilePassOutcome) -> Vec<&FillEvent> {
    r.events
        .iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some(f),
            _ => None,
        })
        .collect()
}

#[test]
fn unknown_order_flag_off_synthesizes_nothing() {
    // Default (adopt: None): UnknownOrder falls into the empty catch-all, even for an
    // already-filled terminal report.
    let d = vec![unknown_order(None, 1.0, 1)];
    let r = resolve(d, &ReconcilePolicy::default(), None, None);
    assert!(r.events.is_empty());
}

#[test]
fn live_unknown_order_synthesizes_nothing_that_folds() {
    // A non-terminal (still-working) unknown order NEVER folds anything: its executions
    // arrive as MissingFill divergences with real venue trade-ids while it stays inside the
    // lookback. Under Synthesize mode that means a true no-op (no events, no alert).
    let a = Adopt::default();
    for filled in [0.0, 1.0] {
        let d = vec![ReconcileDivergence::UnknownOrder(unknown_order_report(
            None,
            filled,
            1,
            vike_model::OrderStatus::Accepted,
        ))];
        let r = resolve(d, &ReconcilePolicy::default(), None, Some(a.ctx()));
        assert!(r.events.is_empty(), "filled={filled}: {:?}", r.events);
        assert!(r.alerts.is_empty(), "synthesize mode never alerts");
    }
}

#[test]
fn live_unknown_order_under_hybrid_holds_a_display_accept_only() {
    // Held modes still surface the live external order for operator visibility/adoption, but
    // the proposed events carry AT MOST the decorative accept — never a synthesized fill that
    // could double-book against the fill lane at confirm time.
    let a = Adopt::default();
    let d = vec![ReconcileDivergence::UnknownOrder(unknown_order_report(
        None,
        1.0,
        1,
        vike_model::OrderStatus::Accepted,
    ))];
    let r = resolve(d, &ReconcilePolicy::hybrid(), None, Some(a.ctx()));
    assert!(r.events.is_empty());
    assert_eq!(r.alerts.len(), 1);
    assert_eq!(r.alerts[0].kind, ReconcileDivergenceKind::UnknownOrder);
    assert_eq!(r.alerts[0].proposed_events.len(), 1, "{:?}", r.alerts[0].proposed_events);
    assert_matches!(r.alerts[0].proposed_events[0], Event::OrderAccepted(_));
    assert_eq!(r.alerts[0].dedup_key.as_deref(), Some("v-unk"), "keyed for runtime dedup");
}

/// Measured on the CI box 2026-11-10: alpaca's order HISTORY read lists old CANCELED/FILLED orders the registry
/// never held, each was held as an `UnknownOrder` whose confirm folded nothing, and the next pass raised it
/// again under the same id. A terminal one that folds nothing now raises no row; a LIVE one still does.
#[test]
fn a_terminal_unknown_order_that_folds_nothing_raises_no_row() {
    use vike_model::OrderStatus::{Accepted, Canceled, Expired, Filled, Rejected};
    let quarantine = ReconcilePolicy { default: ReconcileMode::Quarantine, ..Default::default() };
    let mut a = Adopt::default();
    a.fills.insert("v-unk".into()); // the FILLED one's executions ride this pass's fill lane
    for adopt in [None, Some(a.ctx())] {
        for policy in [&quarantine, &ReconcilePolicy::hybrid()] {
            for (status, filled) in
                [(Canceled, 0.0), (Filled, 1.0), (Expired, 0.0), (Rejected, 0.0)]
            {
                let report = unknown_order_report(Some("probe-15332"), filled, 1, status);
                let r =
                    resolve(vec![ReconcileDivergence::UnknownOrder(report)], policy, None, adopt);
                assert!(r.events.is_empty(), "{status:?}: {:?}", r.events);
                assert!(r.alerts.is_empty(), "{status:?} raised a row: {:?}", r.alerts);
            }
            let live = unknown_order_report(Some("probe-15332"), 0.0, 1, Accepted);
            let r = resolve(vec![ReconcileDivergence::UnknownOrder(live)], policy, None, adopt);
            assert_eq!(r.alerts.len(), 1, "a LIVE unknown order keeps its row: {:?}", r.alerts);
        }
    }
}

#[test]
fn terminal_unknown_order_beyond_the_fill_window_is_adopted() {
    // THE adoption case: terminal, executed, and no fill report in-pass (the executions fell
    // outside the lookback) — accept + one cumulative fill.
    let a = Adopt::default();
    let d = vec![unknown_order(None, 1.0, 1)];
    let r = resolve(d, &ReconcilePolicy::default(), None, Some(a.ctx()));
    assert_eq!(r.events.len(), 2, "{:?}", r.events);
    assert_matches!(r.events[0], Event::OrderAccepted(_));
    match &r.events[1] {
        Event::Fill(f) => {
            assert_eq!(f.side, 1);
            assert_eq!(f.last_qty, 1.0);
            assert_eq!(f.last_price, 100.0);
            assert!(f.trade_id.starts_with("EXT-ORD-"));
        }
        other => panic!("expected Fill, got {other:?}"),
    }
}

#[test]
fn terminal_unknown_order_with_in_pass_fill_reports_defers_to_the_fill_lane() {
    // A recently-executed external order surfaces BOTH a MissingFill (real venue trade-id) and
    // an UnknownOrder (order client id absent from the registry) in the same pass. The adoption arm must
    // synthesize NOTHING — the fill lane books the qty exactly once under its real trade-id; a
    // same-pass PositionDrift nets against that fill alone.
    let mut a = Adopt::default();
    a.fills.insert("v-unk".into()); // this pass's fill reports cover order v-unk
    let mut mf = ext_fill("t-real-1");
    mf.venue_order_id = "v-unk".into();
    let d = vec![
        ReconcileDivergence::MissingFill(mf),
        unknown_order(None, 1.0, 1),
        pos_drift(0.0, 1.0),
    ];
    let r = resolve(d, &ReconcilePolicy::default(), None, Some(a.ctx()));
    let fills = fill_events(&r);
    assert_eq!(fills.len(), 1, "exactly one booking of the external qty: {fills:?}");
    assert_eq!(fills[0].trade_id.as_str(), "t-real-1", "the REAL venue trade-id");
    assert!(!fills.iter().any(|f| f.trade_id.starts_with("EXT-ORD-")));
    assert!(!fills.iter().any(|f| f.trade_id.starts_with("EXT-POS-")));
}

#[test]
fn progressively_filling_unknown_order_converges_via_the_fill_lane() {
    // Pass 2 of a progressively-filling external order — local already booked fill 1 (qty 1),
    // the venue now reports filled_qty 2 with the second fill as a MissingFill, and
    // PositionDrift local 1 → venue 2. The live UnknownOrder
    // must feed NEITHER events NOR the fill window: only the new real fill folds, the drift is
    // fully explained by it (no phantom EXT-POS close leg), and local converges to 2.
    let mut a = Adopt::default();
    a.fills.insert("v-unk".into());
    a.seen.insert("t-real-1".into()); // fill 1 already folded in a prior pass
    let mut mf = ext_fill("t-real-2");
    mf.venue_order_id = "v-unk".into();
    let d = vec![
        ReconcileDivergence::MissingFill(mf),
        ReconcileDivergence::UnknownOrder(unknown_order_report(
            None,
            2.0,
            1,
            vike_model::OrderStatus::PartiallyFilled,
        )),
        pos_drift(1.0, 2.0),
    ];
    let r = resolve(d, &ReconcilePolicy::default(), None, Some(a.ctx()));
    let fills = fill_events(&r);
    assert_eq!(fills.len(), 1, "only the new real fill: {fills:?}");
    assert_eq!(fills[0].trade_id.as_str(), "t-real-2");
    assert_eq!(fills[0].last_qty, 1.0);
    assert!(!fills.iter().any(|f| f.trade_id.starts_with("EXT-")), "no synthetic legs");
}

#[test]
fn already_adopted_unknown_order_is_a_true_no_op() {
    // The recurring-pass invariant: once the adoption fill's deterministic trade_id is in the
    // seen set, the divergence resolves to NOTHING — no events (nothing re-folds, so no
    // events_dropped_order_unknown inflation either) and no alert row, under both auto-apply and held
    // policies.
    let mut a = Adopt::default();
    a.seen.insert("EXT-ORD-binance-v-unk".into());
    for policy in [ReconcilePolicy::default(), ReconcilePolicy::hybrid()] {
        let d = vec![unknown_order(None, 1.0, 1)];
        let r = resolve(d, &policy, None, Some(a.ctx()));
        assert!(r.events.is_empty(), "{policy:?}: {:?}", r.events);
        assert!(r.alerts.is_empty(), "{policy:?}: {:?}", r.alerts);
    }
}

#[test]
fn unknown_order_with_client_id_skips_the_synthesized_accept() {
    // A report that DOES carry an order_client_id (just absent from the local registry) is not
    // externally-placed in the MissingFill sense — no accept is minted, mirroring the
    // MissingFill arm's own `order_client_id.is_none()` gate. Only the Fill rides the report's
    // own order client id.
    let a = Adopt::default();
    let d = vec![unknown_order(Some("c-known"), 2.0, -1)];
    let r = resolve(d, &ReconcilePolicy::default(), None, Some(a.ctx()));
    assert_eq!(r.events.len(), 1, "{:?}", r.events);
    match &r.events[0] {
        Event::Fill(f) => {
            assert_eq!(f.order_client_id, "c-known");
            assert_eq!(f.side, -1);
            assert_eq!(f.last_qty, 2.0);
        }
        other => panic!("expected Fill, got {other:?}"),
    }
}

#[test]
fn unknown_order_hybrid_policy_still_quarantines_but_now_carries_proposed_events() {
    // The point of the flag: under `hybrid` (no-local-origin -> Quarantine), an adoptable
    // UnknownOrder is still held for operator confirm, but its alert is no longer empty — it
    // carries the adoption events an operator's one-click confirm would fold.
    let a = Adopt::default();
    let d = vec![unknown_order(None, 1.0, 1)];
    let r = resolve(d, &ReconcilePolicy::hybrid(), None, Some(a.ctx()));
    assert!(r.events.is_empty(), "no-local-origin kind stays held under hybrid");
    assert_eq!(r.alerts.len(), 1);
    assert_eq!(r.alerts[0].kind, ReconcileDivergenceKind::UnknownOrder);
    assert_eq!(
        r.alerts[0].proposed_events.len(),
        2,
        "adoptable: accept + fill, not empty ({:?})",
        r.alerts[0].proposed_events
    );
    assert_eq!(r.alerts[0].dedup_key.as_deref(), Some("v-unk"));
}

#[test]
fn adopted_unknown_order_fill_nets_against_same_pass_position_drift() {
    // Mirrors `missing_fill_fully_explains_position_drift_no_extra_leg`: an adoption fill that
    // WILL fold this pass feeds the SAME fill_window a MissingFill would, so a co-occurring
    // PositionDrift report on the identical (venue, symbol) nets against it instead of
    // double-booking the same external activity twice in one pass.
    //
    // ⚠ The quantities are 2.0, not 1.0, ON PURPOSE. The netting term is
    // `side as f64 * filled_qty`, and at qty 1.0 `1.0 * 1.0` and `1.0 / 1.0` are both 1.0, so a
    // `*` → `/` mutant stays green. At 2.0 the operator is observable: a division yields 0.5, the
    // drift no longer nets to zero, and a phantom EXT-POS residual appears beside the real fill.
    let a = Adopt::default();
    let d = vec![unknown_order(None, 2.0, 1), pos_drift(0.0, 2.0)];
    let r = resolve(d, &ReconcilePolicy::default(), None, Some(a.ctx()));
    let fills = fill_events(&r);
    assert_eq!(
        fills.len(),
        1,
        "expected only the UnknownOrder's own Fill, no EXT-POS residual: {fills:?}"
    );
    assert_eq!(fills[0].last_qty, 2.0, "the adopted leg is the order's own size: {fills:?}");
    assert!(fills[0].trade_id.starts_with("EXT-ORD-"));
}

#[test]
fn held_unknown_order_adoption_does_not_feed_the_fill_window() {
    // Mode consistency: under hybrid the adoption is HELD — its fill does NOT fold this pass —
    // so a co-occurring PositionDrift (auto-applied under hybrid) must synthesize its FULL
    // healing leg, un-netted. Netting against held events would silently suppress the
    // drift's self-heal.
    let a = Adopt::default();
    let d = vec![unknown_order(None, 1.0, 1), pos_drift(0.0, 1.0)];
    let r = resolve(d, &ReconcilePolicy::hybrid(), None, Some(a.ctx()));
    let fills = fill_events(&r);
    assert_eq!(fills.len(), 1, "the drift's own healing leg folds: {fills:?}");
    assert!(fills[0].trade_id.starts_with("EXT-POS-"), "full un-netted drift leg");
    assert_eq!(fills[0].last_qty, 1.0);
    assert_eq!(r.alerts.len(), 1, "the adoption itself stays held");
    assert_eq!(r.alerts[0].proposed_events.len(), 2);
}

#[test]
fn unknown_order_trade_id_is_deterministic_for_idempotent_refold() {
    // Same report resolved twice (e.g. a re-run pass over an unchanged venue snapshot,
    // BEFORE the first fold lands in seen_trade_ids) synthesizes the IDENTICAL trade_id both
    // times, so a racing second fold dedupes on `Engine::on_event`'s
    // `seen_trade_ids` guard instead of double-booking — the structural half of idempotency
    // `external_order_client_id`'s own doc describes for MissingFill.
    let a = Adopt::default();
    let r1 = resolve(
        vec![unknown_order(None, 1.0, 1)],
        &ReconcilePolicy::default(),
        None,
        Some(a.ctx()),
    );
    let r2 = resolve(
        vec![unknown_order(None, 1.0, 1)],
        &ReconcilePolicy::default(),
        None,
        Some(a.ctx()),
    );
    let tid = |r: &ReconcilePassOutcome| {
        r.events
            .iter()
            .find_map(|e| match e {
                Event::Fill(f) => Some(f.trade_id.clone()),
                _ => None,
            })
            .unwrap()
    };
    assert_eq!(tid(&r1), tid(&r2));
}

// --- BalanceDrift resolution (first-class cash reconcile) ---

fn balance_drift() -> ReconcileDivergence {
    ReconcileDivergence::BalanceDrift {
        venue: "binance".into(),
        asset: "USDT".into(),
        local: 10_000.0,
        venue_balance: 10_150.0,
        timestamp: 42,
    }
}

#[test]
fn balance_drift_synthesize_folds_one_account_state_no_alert() {
    // synthesize ⇒ adopt venue truth immediately: one AccountState folds, no alert.
    let r = resolve(vec![balance_drift()], &ReconcilePolicy::default(), None, None);
    assert!(r.alerts.is_empty());
    assert_eq!(r.events.len(), 1);
    match &r.events[0] {
        Event::AccountState(a) => {
            assert_eq!(a.venue.as_str(), "binance");
            assert_eq!(a.balances, vec![("USDT".to_string(), 10_150.0)]);
            assert_eq!(a.timestamp, 42);
        }
        other => panic!("expected AccountState, got {other:?}"),
    }
}

#[test]
fn balance_drift_hybrid_and_quarantine_hold_the_account_state_for_confirm() {
    // The load-bearing safety choice: under BOTH hybrid (no-local-origin ⇒ Quarantine) and
    // explicit quarantine, a surprise cash move NEVER auto-folds — it is held with the
    // correcting AccountState carried in proposed_events, keyed for in-place refresh.
    for policy in [
        ReconcilePolicy::hybrid(),
        ReconcilePolicy { default: ReconcileMode::Quarantine, ..Default::default() },
    ] {
        let r = resolve(vec![balance_drift()], &policy, None, None);
        assert!(r.events.is_empty(), "never auto-folds a surprise ({policy:?})");
        assert_eq!(r.alerts.len(), 1);
        let a = &r.alerts[0];
        assert_eq!(a.kind, ReconcileDivergenceKind::BalanceDrift);
        assert_eq!(a.dedup_key.as_deref(), Some("balance:USDT"), "one row per (venue, asset)");
        assert_eq!(a.proposed_events.len(), 1, "the AccountState an operator confirm folds");
        assert_matches!(a.proposed_events[0], Event::AccountState(_));
        assert!(a.detail.contains("+150"), "unexplained delta surfaced: {}", a.detail);
    }
}

// --- OrphanLocalPosition resolution (diff's local-side position sweep) ---

fn orphan_local_position(symbol: &str, side: PositionSide, qty: f64) -> ReconcileDivergence {
    ReconcileDivergence::OrphanLocalPosition {
        venue: "binance".into(),
        symbol: symbol.into(),
        position_side: side,
        local_quantity: qty,
    }
}

#[test]
fn orphan_local_position_never_folds_an_event_under_any_policy() {
    // The load-bearing safety property: there is no defensible price for a synthetic close
    // (ReconcileLocalView carries qty only), so this kind resolves to ZERO events under EVERY policy —
    // including `synthesize`, which folds everything else it can.
    for policy in [
        ReconcilePolicy::default(), // Synthesize
        ReconcilePolicy::hybrid(),
        ReconcilePolicy { default: ReconcileMode::Quarantine, ..Default::default() },
        ReconcilePolicy { default: ReconcileMode::Hybrid, ..Default::default() },
    ] {
        let r = resolve(
            vec![orphan_local_position("BTCUSDT", PositionSide::Both, 1.0)],
            &policy,
            None,
            None,
        );
        assert!(r.events.is_empty(), "never auto-flattens a position ({policy:?}): {:?}", r.events);
        assert!(
            r.alerts.iter().all(|a| a.proposed_events.is_empty()),
            "investigate-only: a confirm must not flatten at a guessed price either ({policy:?})"
        );
    }
}

#[test]
fn orphan_local_position_is_a_silent_no_op_under_synthesize() {
    // NAMED RESIDUAL (module doc): `synthesize` folds this kind's empty event list, so nothing
    // folds AND nothing alerts — the same shape `AdoptionCase::SurfaceOnly` takes under an
    // auto-apply mode. An operator who wants the blind spot surfaced runs hybrid/quarantine.
    let r = resolve(
        vec![orphan_local_position("BTCUSDT", PositionSide::Both, 1.0)],
        &ReconcilePolicy::default(),
        None,
        None,
    );
    assert_eq!(r, ReconcilePassOutcome::default(), "synthesize: a true no-op");
}

#[test]
fn orphan_local_position_holds_one_dedup_keyed_alert_under_hybrid_and_quarantine() {
    // hybrid classifies it no-local-origin (an absent venue row is evidence-by-absence, which
    // an incomplete/symbol-scoped fetch produces just as readily as a genuinely closed
    // position), so BOTH held policies surface exactly one operator alert — and it is
    // dedup-keyed, because nothing heals this divergence and an un-keyed alert would append a
    // new row every pass.
    for policy in [
        ReconcilePolicy::hybrid(),
        ReconcilePolicy { default: ReconcileMode::Quarantine, ..Default::default() },
    ] {
        let r = resolve(
            vec![orphan_local_position("BTCUSDT", PositionSide::Both, -2.5)],
            &policy,
            None,
            None,
        );
        assert!(r.events.is_empty(), "{policy:?}");
        assert_eq!(r.alerts.len(), 1, "{policy:?}");
        let a = &r.alerts[0];
        assert_eq!(a.kind, ReconcileDivergenceKind::OrphanLocalPosition);
        assert_eq!(
            a.dedup_key.as_deref(),
            Some("position:BTCUSDT:BOTH"),
            "one row per (venue, symbol, side)"
        );
        assert!(a.proposed_events.is_empty(), "nothing an operator confirm could fold");
        assert!(a.recover_orders.is_empty());
        assert!(a.detail.contains("BTCUSDT"), "operator-readable: {}", a.detail);
        assert!(a.detail.contains("-2.5"), "carries the local qty: {}", a.detail);
        assert!(a.detail.contains("binance"), "names the venue: {}", a.detail);
    }
}

#[test]
fn orphan_local_position_bare_hybrid_default_matches_the_preset() {
    // The two classification sites must agree: `ReconcilePolicy::hybrid()`'s per-kind preset and
    // the bare `ReconcileMode::Hybrid` fallback (`is_local_origin`). Both must HOLD this kind.
    let bare = ReconcilePolicy { default: ReconcileMode::Hybrid, ..Default::default() };
    let preset = ReconcilePolicy::hybrid();
    let d = || vec![orphan_local_position("BTCUSDT", PositionSide::Both, 1.0)];
    assert_eq!(resolve(d(), &bare, None, None), resolve(d(), &preset, None, None));
    assert_eq!(resolve(d(), &bare, None, None).alerts.len(), 1);
}

#[test]
fn orphan_local_position_dedup_key_separates_symbol_and_side() {
    // Hedge mode / multi-symbol: each (symbol, side) gets its own held row, so refreshing one
    // never overwrites another.
    let policy = ReconcilePolicy::hybrid();
    let d = vec![
        orphan_local_position("BTCUSDT", PositionSide::Long, 1.0),
        orphan_local_position("BTCUSDT", PositionSide::Short, -2.0),
        orphan_local_position("ETHUSDT", PositionSide::Both, 3.0),
    ];
    let r = resolve(d, &policy, None, None);
    let keys: Vec<_> = r.alerts.iter().filter_map(|a| a.dedup_key.clone()).collect();
    assert_eq!(
        keys,
        vec!["position:BTCUSDT:LONG", "position:BTCUSDT:SHORT", "position:ETHUSDT:BOTH"]
    );
}

#[test]
fn orphan_local_position_does_not_disturb_a_co_occurring_missing_fill() {
    // Non-interference: the sweep contributes nothing to the fill window and nothing to the
    // event stream, so a MissingFill in the same pass resolves byte-identically to a pass
    // without it.
    let policy = ReconcilePolicy::hybrid();
    let without =
        resolve(vec![ReconcileDivergence::MissingFill(ext_fill("t1"))], &policy, None, None);
    let with = resolve(
        vec![
            ReconcileDivergence::MissingFill(ext_fill("t1")),
            orphan_local_position("ETHUSDT", PositionSide::Both, 4.0),
        ],
        &policy,
        None,
        None,
    );
    assert_eq!(with.events, without.events, "the fill's own events are untouched");
    assert_eq!(with.alerts.len(), without.alerts.len() + 1, "exactly one added alert");
}

// --- OrphanLocalOrder resolution (diff's local-side ORDER sweep; see the module doc) ---

fn orphan_order(order_client_id: &str) -> ReconcileDivergence {
    ReconcileDivergence::OrphanLocalOrder { order_client_id: order_client_id.into() }
}

fn quarantine_policy() -> ReconcilePolicy {
    ReconcilePolicy { default: ReconcileMode::Quarantine, ..Default::default() }
}

#[test]
fn orphan_local_order_never_folds_an_event_under_any_policy() {
    // The load-bearing safety property, unchanged by making the kind visible: a synthesized
    // cancel would terminalize an order that may still be RESTING at the venue (this module
    // folds locally and calls no venue), so the kind resolves to ZERO events under EVERY
    // policy — and its alert proposes zero events, so an operator confirm cannot fold one
    // either.
    for policy in [
        ReconcilePolicy::default(), // Synthesize
        ReconcilePolicy::hybrid(),
        quarantine_policy(),
        ReconcilePolicy { default: ReconcileMode::Hybrid, ..Default::default() },
    ] {
        let r = resolve(vec![orphan_order("c-1")], &policy, None, None);
        assert!(r.events.is_empty(), "never folds ({policy:?}): {:?}", r.events);
        assert!(
            !r.events.iter().any(|e| matches!(e, Event::OrderCanceled(_))),
            "no policy may synthesize a cancel ({policy:?})"
        );
        assert!(
            r.alerts.iter().all(|a| a.proposed_events.is_empty() && a.recover_orders.is_empty()),
            "confirm is a pure acknowledgement — nothing to fold, nothing to re-register \
                 ({policy:?})"
        );
    }
}

#[test]
fn orphan_local_order_is_a_silent_no_op_under_synthesize() {
    // NAMED RESIDUAL (module doc), the same one `OrphanLocalPosition` carries: `synthesize`
    // folds this kind's empty event list, so nothing folds AND nothing alerts. An operator who
    // wants the blind spot surfaced runs hybrid/quarantine.
    let r = resolve(vec![orphan_order("c-1")], &ReconcilePolicy::default(), None, None);
    assert_eq!(r, ReconcilePassOutcome::default(), "synthesize: a true no-op");
}

#[test]
fn orphan_local_order_holds_one_dedup_keyed_alert_under_hybrid_and_quarantine() {
    // Both held policies surface ONE dedup-keyed, operator-readable, event-free row.
    for policy in [ReconcilePolicy::hybrid(), quarantine_policy()] {
        let r = resolve(vec![orphan_order("c-1")], &policy, None, None);
        assert!(r.events.is_empty(), "{policy:?}");
        assert_eq!(r.alerts.len(), 1, "{policy:?}: {:?}", r.alerts);
        let a = &r.alerts[0];
        assert_eq!(a.kind, ReconcileDivergenceKind::OrphanLocalOrder);
        assert_eq!(
            a.dedup_key.as_deref(),
            Some(ORPHAN_LOCAL_ORDER_KEY),
            "one held row per (venue, kind) — NOT one per order client id, see the module doc"
        );
        assert!(a.proposed_events.is_empty(), "nothing an operator confirm could fold");
        assert!(a.recover_orders.is_empty());
        assert!(a.detail.contains("c-1"), "names the order: {}", a.detail);
        assert!(a.detail.contains('1'), "carries the count: {}", a.detail);
    }
}

#[test]
fn orphan_local_order_bare_hybrid_default_matches_the_preset() {
    // The two classification sites must agree: `ReconcilePolicy::hybrid()`'s per-kind preset and
    // the bare `ReconcileMode::Hybrid` fallback (`is_local_origin`). Both must HOLD this kind —
    // the twin of `orphan_local_position_bare_hybrid_default_matches_the_preset`.
    let bare = ReconcilePolicy { default: ReconcileMode::Hybrid, ..Default::default() };
    let preset = ReconcilePolicy::hybrid();
    let d = || vec![orphan_order("c-1")];
    assert_eq!(resolve(d(), &bare, None, None), resolve(d(), &preset, None, None));
    assert_eq!(resolve(d(), &bare, None, None).alerts.len(), 1);
}

#[test]
fn many_orphan_local_orders_aggregate_into_exactly_one_alert() {
    // The whole reason the order client id is NOT the dedup key: the held-alert store never self-clears
    // and its rows are cloned into every published snapshot, so a pass in which the venue
    // report echoes no client ids at all (every live order orphans at once) must still cost
    // ONE row. The count is exact; only the NAMED sample truncates.
    let d: Vec<ReconcileDivergence> =
        (0..ORPHAN_SAMPLE + 5).map(|i| orphan_order(&format!("c-{i:02}"))).collect();
    let n = d.len();
    let r = resolve(d, &ReconcilePolicy::hybrid(), None, None);
    assert_eq!(r.alerts.len(), 1, "{:?}", r.alerts);
    let detail = &r.alerts[0].detail;
    assert!(detail.starts_with(&format!("{n} live LOCAL order(s)")), "exact count: {detail}");
    assert!(detail.contains("(+5 more)"), "sample truncated with a remainder: {detail}");
    assert!(
        detail.contains("c-00"),
        "sorted sample starts at the lowest order client id: {detail}"
    );
    assert!(!detail.contains("c-12"), "beyond the sample cap: {detail}");
}

#[test]
fn a_steady_orphan_set_renders_a_byte_identical_detail() {
    // Sorting is load-bearing, not tidiness: the runtime REFRESHES a dedup-keyed row only when
    // the detail changed, so an unchanged orphan set arriving in a different registry order
    // must render the identical string (otherwise every pass writes a ring note and churns the
    // held row). A CHANGED set must move it.
    let p = ReconcilePolicy::hybrid();
    let fwd = resolve(vec![orphan_order("c-a"), orphan_order("c-b")], &p, None, None);
    let rev = resolve(vec![orphan_order("c-b"), orphan_order("c-a")], &p, None, None);
    assert_eq!(fwd.alerts[0].detail, rev.alerts[0].detail, "order-independent");
    let grown = resolve(
        vec![orphan_order("c-a"), orphan_order("c-b"), orphan_order("c-c")],
        &p,
        None,
        None,
    );
    assert_ne!(fwd.alerts[0].detail, grown.alerts[0].detail, "a changed set moves the detail");
}

#[test]
fn orphan_local_order_does_not_disturb_a_co_occurring_missing_fill() {
    // Non-interference: the sweep contributes nothing to the fill window and nothing to the
    // event stream, so a MissingFill in the same pass resolves byte-identically to a pass
    // without it. Its own alert is appended LAST (the aggregate summarizes the pass).
    let policy = ReconcilePolicy::hybrid();
    let without =
        resolve(vec![ReconcileDivergence::MissingFill(ext_fill("t1"))], &policy, None, None);
    let with = resolve(
        vec![orphan_order("c-1"), ReconcileDivergence::MissingFill(ext_fill("t1"))],
        &policy,
        None,
        None,
    );
    assert_eq!(with.events, without.events, "the fill's own events are untouched");
    assert_eq!(with.alerts.len(), without.alerts.len() + 1, "exactly one added alert");
    assert_eq!(
        with.alerts.last().map(|a| a.kind),
        Some(ReconcileDivergenceKind::OrphanLocalOrder),
        "aggregate lands last"
    );
}

// --- a PositionDrift on a FLAT, UNPRICED venue row, held under EVERY policy (`flat_unpriced_drift`) ---

fn drift_at(local_quantity: f64, venue_qty: f64, avg_px: f64) -> ReconcileDivergence {
    let ReconcileDivergence::PositionDrift { mut report, local_quantity } =
        pos_drift(local_quantity, venue_qty)
    else {
        unreachable!()
    };
    report.average_price = avg_px;
    ReconcileDivergence::PositionDrift { report, local_quantity }
}

fn quarantine() -> ReconcilePolicy {
    ReconcilePolicy { default: ReconcileMode::Quarantine, ..Default::default() }
}

#[test]
fn a_held_flat_unpriced_drift_proposes_no_fill_and_names_the_local_drop() {
    for px in [0.0, f64::NAN, -1.0] {
        let r = resolve(vec![drift_at(8.34, 0.0, px)], &quarantine(), None, None);
        assert!(r.events.is_empty());
        let [a] = r.alerts.as_slice() else { panic!("one alert: {:?}", r.alerts) };
        assert_eq!(a.kind, ReconcileDivergenceKind::PositionDrift);
        assert!(a.proposed_events.is_empty(), "no fill at a price the venue did not report");
        assert_eq!(
            a.orphan_position,
            Some(ReconcileOrphanPosition {
                symbol: "BTCUSDT".into(),
                position_side: PositionSide::Both,
                local_quantity: 8.34,
                venue_qty: 0.0,
            })
        );
        assert!(a.detail.contains("local position's own average price"), "{}", a.detail);
        // The detail note must not move the dedup identity.
        assert_eq!(a.identity_detail, Some("binance BTCUSDT Both".to_string()));
    }
}

#[test]
fn a_held_drift_on_a_priced_row_is_unchanged() {
    // A flat row WITH a real price, and an ordinary resize: both keep their legs.
    for d in [drift_at(8.34, 0.0, 90.0), drift_at(1.0, 3.0, 100.0)] {
        let r = resolve(vec![d], &quarantine(), None, None);
        let [a] = r.alerts.as_slice() else { panic!("one alert") };
        assert_eq!(a.proposed_events.len(), 1, "the venue-priced leg is still proposed");
        assert_eq!(a.orphan_position, None);
        assert!(!a.confirm_refused);
        assert!(!a.detail.contains("local position's own average price"), "{}", a.detail);
    }
}

/// A row that is not flat but carries no usable price: every leg `synth_position_legs` makes is
/// priced at it. Local long 5 reduced to 3 sells 2 at `0` (a fake loss of 200 at a 100 basis), a sign
/// flip closes AND opens at `0`, a local-flat drift opens at `0`.
fn unpriced_non_flat_cases() -> Vec<ReconcileDivergence> {
    vec![
        drift_at(5.0, 3.0, 0.0),
        drift_at(5.0, -3.0, 0.0),
        drift_at(0.0, 3.0, 0.0),
        drift_at(5.0, 3.0, f64::NAN),
        drift_at(5.0, 3.0, -1.0),
        drift_at(5.0, 3.0, f64::INFINITY),
    ]
}

#[test]
fn a_non_flat_unpriced_drift_is_held_and_its_confirm_refuses_under_every_policy() {
    for policy in every_policy() {
        for d in unpriced_non_flat_cases() {
            let r = resolve(vec![d.clone()], &policy, None, None);
            assert!(r.events.is_empty(), "nothing folds at an unusable price: {:?}", r.events);
            let [a] = r.alerts.as_slice() else { panic!("one held alert: {:?}", r.alerts) };
            assert_eq!(a.kind, ReconcileDivergenceKind::PositionDrift);
            assert!(a.proposed_events.is_empty(), "no leg priced at the venue's figure: {d:?}");
            assert_eq!(a.orphan_position, None, "a local drop would not reach the venue's size");
            assert!(a.confirm_refused, "{d:?} under {policy:?}");
            assert!(a.detail.contains("REFUSES"), "{}", a.detail);
            // The detail note must not move the dedup identity.
            assert_eq!(a.identity_detail, Some("binance BTCUSDT Both".to_string()));
        }
    }
}

/// **A spot wallet holding LESS than the strategy bought** (coins sold or withdrawn by hand; the spot rule,
/// owner 2026-11-11): held under EVERY policy, never folded by itself, and its confirm SHRINKS the local
/// position to the wallet's figure at the strategy's own average price (zero PnL), instead of refusing.
#[test]
fn a_spot_wallet_below_the_book_is_held_and_its_confirm_shrinks_to_the_wallet() {
    let spot = |d: ReconcileDivergence| {
        let ReconcileDivergence::PositionDrift { mut report, local_quantity } = d else {
            unreachable!()
        };
        report.margin_mode = vike_model::MarginMode::Cash;
        ReconcileDivergence::PositionDrift { report, local_quantity }
    };
    for policy in every_policy() {
        let r = resolve(vec![spot(drift_at(5.0, 3.0, 0.0))], &policy, None, None);
        assert!(r.events.is_empty(), "never folded by itself: {:?}", r.events);
        let [a] = r.alerts.as_slice() else { panic!("one held alert: {:?}", r.alerts) };
        assert_eq!(a.kind, ReconcileDivergenceKind::PositionDrift);
        assert!(a.proposed_events.is_empty(), "no fill at the wallet's 0.0");
        assert!(!a.confirm_refused, "under {policy:?}");
        assert_eq!(
            a.orphan_position.as_ref().map(|o| o.local_quantity),
            Some(5.0),
            "under {policy:?}"
        );
        assert_eq!(a.orphan_position.as_ref().map(|o| o.venue_qty), Some(3.0), "to the wallet's 3");
        assert!(a.detail.contains("spot wallet"), "{}", a.detail);
        assert_eq!(a.identity_detail, Some("binance BTCUSDT Both".to_string()));
    }
}

#[test]
fn a_dust_venue_row_counts_as_flat_and_a_wider_one_does_not() {
    for policy in every_policy() {
        let r = resolve(vec![drift_at(5.0, 1e-12, 0.0)], &policy, None, None);
        assert!(r.events.is_empty());
        let [a] = r.alerts.as_slice() else { panic!("one held alert: {:?}", r.alerts) };
        assert_eq!(a.orphan_position.as_ref().map(|o| o.local_quantity), Some(5.0), "dust is flat");
        assert!(!a.confirm_refused);
        assert!(a.detail.contains("local position's own average price"), "{}", a.detail);

        let r = resolve(vec![drift_at(5.0, 1e-6, 0.0)], &policy, None, None);
        let [a] = r.alerts.as_slice() else { panic!("one held alert: {:?}", r.alerts) };
        assert!(a.confirm_refused && a.orphan_position.is_none(), "1e-6 is a real quantity");
    }
}

fn external_at(venue_qty: f64, avg_px: f64) -> ReconcileDivergence {
    let ReconcileDivergence::PositionDrift { report, .. } = drift_at(0.0, venue_qty, avg_px) else {
        unreachable!()
    };
    ReconcileDivergence::PositionOnlyExternal(report)
}

#[test]
fn an_unpriced_external_position_is_held_and_never_adopted_at_cost_basis_zero() {
    for policy in every_policy() {
        let r = resolve(vec![external_at(3.0, 0.0)], &policy, None, None);
        assert!(r.events.is_empty(), "nothing is adopted at cost basis 0: {:?}", r.events);
        let [a] = r.alerts.as_slice() else { panic!("one held alert: {:?}", r.alerts) };
        assert_eq!(a.kind, ReconcileDivergenceKind::PositionOnlyExternal);
        assert!(a.proposed_events.is_empty() && a.orphan_position.is_none());
        assert!(a.confirm_refused, "under {policy:?}");
    }
    // A PRICED external position resolves exactly as before: adopted under synthesize, a held
    // alert with its leg under quarantine.
    let r = resolve(vec![external_at(3.0, 100.0)], &ReconcilePolicy::default(), None, None);
    assert_eq!(r.events.len(), 1);
    assert!(r.alerts.is_empty());
    let r = resolve(vec![external_at(3.0, 100.0)], &quarantine(), None, None);
    let [a] = r.alerts.as_slice() else { panic!("one alert") };
    assert_eq!(a.proposed_events.len(), 1);
    assert!(!a.confirm_refused);
}

#[test]
fn a_non_flat_unpriced_drift_netted_against_a_missing_fill_acknowledges_and_does_not_refuse() {
    for policy in every_policy() {
        // local 5, venue 2 at 0, a missed sell of 1: the residual leg (5 -> 3) is priced at 0.
        let d = vec![missing_fill(-1, 1.0, "t-mf"), drift_at(5.0, 2.0, 0.0)];
        let r = resolve(d, &policy, None, None);
        let drift = r
            .alerts
            .iter()
            .find(|a| a.kind == ReconcileDivergenceKind::PositionDrift)
            .expect("drift");
        assert!(drift.proposed_events.is_empty() && drift.orphan_position.is_none());
        assert!(!drift.confirm_refused, "the next pass re-checks what is left");
        assert!(drift.detail.contains("nets into it"), "{}", drift.detail);
        for e in &r.events {
            if let Event::Fill(f) = e {
                assert_eq!(f.last_price, 100.0, "nothing is priced at the row's 0.0: {policy:?}");
            }
        }
    }
}

#[test]
fn an_unpriced_row_whose_resolution_prices_no_leg_is_not_held() {
    // The missed sell of 1 explains the whole 3 -> 2 move: no residual leg, so no price is used.
    let d = vec![missing_fill(-1, 1.0, "t-mf"), drift_at(3.0, 2.0, 0.0)];
    let r = resolve(d, &ReconcilePolicy::hybrid(), None, None);
    assert!(r.alerts.is_empty(), "{:?}", r.alerts);
    assert_eq!(r.events.len(), 2, "the external accept and the fill, as ever");
}

/// The policies that fold a `PositionDrift` blind (`synthesize`, the default; `hybrid`) and the ones
/// that hold it, as one list: a flat-unpriced drift must come out the same under all of them.
fn every_policy() -> [ReconcilePolicy; 4] {
    [
        ReconcilePolicy::default(),
        ReconcilePolicy::hybrid(),
        quarantine(),
        ReconcilePolicy::external_quarantine(),
    ]
}

#[test]
fn a_flat_unpriced_drift_is_held_with_the_local_drop_under_every_policy() {
    for policy in every_policy() {
        let r = resolve(vec![drift_at(2.0, 0.0, 0.0)], &policy, None, None);
        assert!(r.events.is_empty(), "nothing folds at the venue's 0.0: {:?}", r.events);
        let [a] = r.alerts.as_slice() else { panic!("one held alert: {:?}", r.alerts) };
        assert!(a.proposed_events.is_empty());
        assert_eq!(a.orphan_position.as_ref().map(|o| o.local_quantity), Some(2.0));
        assert!(a.detail.contains("local position's own average price"), "{}", a.detail);
    }
}

#[test]
fn a_flat_unpriced_drift_netted_against_a_missing_fill_is_held_with_no_event_under_every_policy() {
    for policy in every_policy() {
        let d = vec![missing_fill(-1, 1.0, "t-mf"), drift_at(3.0, 0.0, 0.0)];
        let r = resolve(d, &policy, None, None);
        let drift = r
            .alerts
            .iter()
            .find(|a| a.kind == ReconcileDivergenceKind::PositionDrift)
            .expect("drift");
        assert!(drift.proposed_events.is_empty(), "no leg at the venue's 0.0 to fold at a confirm");
        assert_eq!(drift.orphan_position, None, "a drop would end at the wrong size");
        assert!(drift.detail.contains("nets into it"), "{}", drift.detail);
        // Nothing the pass folds is priced at the row's 0.0: only the missed fill's own events.
        for e in &r.events {
            if let Event::Fill(f) = e {
                assert_eq!(f.last_price, 100.0, "under {policy:?}");
            }
        }
    }
    // The missed fill itself still folds under hybrid, exactly as before.
    let d = vec![missing_fill(-1, 1.0, "t-mf"), drift_at(3.0, 0.0, 0.0)];
    let r = resolve(d, &ReconcilePolicy::hybrid(), None, None);
    assert_eq!(r.events.len(), 2, "the external accept and the fill");
}

#[test]
fn a_priced_drift_still_folds_under_the_folding_policies() {
    for policy in [ReconcilePolicy::default(), ReconcilePolicy::hybrid()] {
        // flat but PRICED, an ordinary resize: each still folds its leg
        for d in [drift_at(8.34, 0.0, 90.0), drift_at(1.0, 3.0, 100.0)] {
            let r = resolve(vec![d], &policy, None, None);
            assert!(r.alerts.is_empty(), "{:?}", r.alerts);
            assert_eq!(r.events.len(), 1, "unchanged under {policy:?}");
        }
        // a sign flip with a price still closes AND opens
        let r = resolve(vec![drift_at(5.0, -3.0, 100.0)], &policy, None, None);
        assert!(r.alerts.is_empty());
        assert_eq!(r.events.len(), 2, "unchanged under {policy:?}");
    }
}
