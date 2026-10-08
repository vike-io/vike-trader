use super::*;
use crate::recon::types::ReconPolicy;
use vike_model::FillReport;

fn ext_fill(trade_id: &'static str) -> FillReport {
    FillReport {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        trade_id: trade_id.into(),
        venue_order_id: "v9".into(),
        client_order_id: None,
        side: 1,
        last_qty: 1.0,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: "USDT".into(),
        liquidity_side: LiquiditySide::Taker,
        ts: 5,
    }
}

#[test]
fn external_missing_fill_synthesizes_accept_then_fill() {
    let d = vec![Divergence::MissingFill(ext_fill("t1"))];
    let r = resolve(d, &ReconPolicy::default(), None, None);
    assert_eq!(r.events.len(), 2);
    assert!(matches!(r.events[0], Event::OrderAccepted(_)));
    assert!(matches!(r.events[1], Event::Fill(_)));
}

#[test]
fn journal_divergence_always_alerts_under_every_policy() {
    // A three-way persistence-bug signal never resolves to an event: under EVERY policy it must
    // surface as a single investigative alert with no proposed events (the always-surface rule).
    let policies = [
        ReconPolicy::default(), // Synthesize
        ReconPolicy { default: ReconMode::Quarantine, ..Default::default() }, // Quarantine
        ReconPolicy::hybrid(),  // Hybrid
    ];
    for policy in &policies {
        let d = vec![Divergence::JournalDivergence {
            detail: "journal has fill t9, live account lacks it".into(),
            recover_order: None,
        }];
        let r = resolve(d, policy, None, None);
        assert!(r.events.is_empty(), "journal divergence synthesizes NO events ({policy:?})");
        assert_eq!(r.alerts.len(), 1, "exactly one alert ({policy:?})");
        assert_eq!(r.alerts[0].kind, DivergenceKind::JournalDivergence);
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
        client_order_id: Some("c-lost".into()),
        side: 1,
        order_type: "LIMIT".into(),
        qty: 1.0,
        filled_qty: 0.0,
        avg_px: 0.0,
        status: "ACCEPTED".into(),
        ts: 3,
    };
    for policy in [ReconPolicy::default(), ReconPolicy::hybrid()] {
        let d = vec![Divergence::JournalDivergence {
            detail: "order c-lost live in journal, local lost it".into(),
            recover_order: Some(Box::new(report.clone())),
        }];
        let r = resolve(d, &policy, None, None);
        assert!(r.events.is_empty(), "never auto-folds ({policy:?})");
        assert_eq!(r.alerts.len(), 1);
        assert_eq!(r.alerts[0].recover_orders.len(), 1, "carries the order to re-register");
        assert_eq!(r.alerts[0].recover_orders[0].client_order_id.as_deref(), Some("c-lost"));
    }
}

#[test]
fn quarantine_holds_events_as_alert() {
    let policy = ReconPolicy { default: ReconMode::Quarantine, ..Default::default() };
    let d = vec![Divergence::MissingFill(ext_fill("t1"))];
    let r = resolve(d, &policy, None, None);
    assert!(r.events.is_empty());
    assert_eq!(r.alerts.len(), 1);
    assert_eq!(r.alerts[0].proposed_events.len(), 2);
}

#[test]
fn external_coid_is_deterministic() {
    assert_eq!(external_coid("binance", "v9"), "EXT-binance-v9");
}

fn pos_drift(local_qty: f64, venue_qty: f64) -> Divergence {
    Divergence::PositionDrift {
        report: vike_model::PositionStatusReport {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            position_side: PositionSide::Both,
            qty: venue_qty,
            avg_px: 100.0,
            ts: 7,
            margin_mode: Default::default(),
            isolated_margin: None,
            delta: None,
        },
        local_qty,
    }
}

#[test]
fn same_sign_drift_is_one_fill() {
    // local +1, venue +3 → one synthetic buy of +2
    let r = resolve(vec![pos_drift(1.0, 3.0)], &ReconPolicy::default(), None, None);
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
    let r = resolve(vec![pos_drift(2.0, -1.0)], &ReconPolicy::default(), None, None);
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

fn missing_fill(side: i32, qty: f64, trade_id: &'static str) -> Divergence {
    Divergence::MissingFill(vike_model::FillReport {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        trade_id: trade_id.into(),
        venue_order_id: "v-mf".into(),
        client_order_id: None,
        side,
        last_qty: qty,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: "USDT".into(),
        liquidity_side: LiquiditySide::Taker,
        ts: 5,
    })
}

fn pos_drift_side(local_qty: f64, venue_qty: f64, side: PositionSide) -> Divergence {
    Divergence::PositionDrift {
        report: vike_model::PositionStatusReport {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            position_side: side,
            qty: venue_qty,
            avg_px: 100.0,
            ts: 7,
            margin_mode: Default::default(),
            isolated_margin: None,
            delta: None,
        },
        local_qty,
    }
}

#[test]
fn missing_fill_fully_explains_position_drift_no_extra_leg() {
    // venue holds +1 BTC; local is flat; the +1 is entirely explained by one missed fill.
    // resolve must emit ONLY the fill's own events (accept + fill) — no EXT-POS-* leg.
    let d = vec![missing_fill(1, 1.0, "t-mf1"), pos_drift(0.0, 1.0)];
    let r = resolve(d, &ReconPolicy::default(), None, None);
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
    let r = resolve(d, &ReconPolicy::default(), None, None);
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
    let r = resolve(d, &ReconPolicy::default(), None, None);
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
    assert_eq!(fills[0].ts, 4, "anchored before the earliest recovered fill (ts 5)");
    assert_eq!(fills[1].trade_id.as_str(), "t-mf1");
}

#[test]
fn pre_window_residual_crossing_splits_at_flat_before_fills() {
    // local +2; window recovered a buy +2 (ts 5); venue net −1. Pre-window truth: +2 crossed
    // flat to −3 (then the recovered +2 brings it to −1). TWO pre-anchored legs — close 2,
    // open 3, both ts 4, both sells — fold before the recovered fill.
    let d = vec![missing_fill(1, 2.0, "t-mf2"), pos_drift(2.0, -1.0)];
    let r = resolve(d, &ReconPolicy::default(), None, None);
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
    assert_eq!((fills[0].side, fills[0].last_qty, fills[0].ts), (-1, 2.0, 4), "close to flat");
    assert!(fills[1].trade_id.starts_with("EXT-POS-"));
    assert_eq!((fills[1].side, fills[1].last_qty, fills[1].ts), (-1, 3.0, 4), "open short");
    assert_eq!(fills[2].trade_id.as_str(), "t-mf2", "recovered fill folds last: −3 + 2 = −1");
}

#[test]
fn hedge_mode_position_report_is_not_netted_against_fills() {
    // A `Both`-side MissingFill does not cleanly map onto a hedge-mode Long/Short bucket, so
    // netting is intentionally skipped: the position leg is synthesized against the RAW
    // local_qty.
    let d = vec![missing_fill(1, 1.0, "t-mf1"), pos_drift_side(0.0, 1.0, PositionSide::Long)];
    let r = resolve(d, &ReconPolicy::default(), None, None);
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

/// Owned backing for an [`AdoptContext`] (which borrows its two sets).
#[derive(Default)]
struct Adopt {
    fills: HashSet<String>,
    seen: HashSet<String>,
}

impl Adopt {
    fn ctx(&self) -> AdoptContext<'_> {
        AdoptContext { pass_fill_order_ids: &self.fills, seen_trade_ids: &self.seen }
    }
}

fn unknown_order_report(
    client_order_id: Option<&str>,
    filled_qty: f64,
    side: i32,
    status: &str,
) -> vike_model::OrderStatusReport {
    vike_model::OrderStatusReport {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        venue_order_id: "v-unk".into(),
        client_order_id: client_order_id.map(String::from),
        side,
        order_type: "LIMIT".into(),
        qty: 2.0,
        filled_qty,
        avg_px: if filled_qty != 0.0 { 100.0 } else { 0.0 },
        status: status.into(),
        ts: 11,
    }
}

fn unknown_order(client_order_id: Option<&str>, filled_qty: f64, side: i32) -> Divergence {
    // Terminal FILLED report — the adoption case (when no fill reports co-occur in-pass).
    Divergence::UnknownOrder(unknown_order_report(client_order_id, filled_qty, side, "FILLED"))
}

fn fill_events(r: &Recon) -> Vec<&FillEvent> {
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
    let r = resolve(d, &ReconPolicy::default(), None, None);
    assert!(r.events.is_empty());
}

#[test]
fn live_unknown_order_synthesizes_nothing_that_folds() {
    // A non-terminal (still-working) unknown order NEVER folds anything: its executions
    // arrive as MissingFill divergences with real venue trade-ids while it stays inside the
    // lookback. Under Synthesize mode that means a true no-op (no events, no alert).
    let a = Adopt::default();
    for filled in [0.0, 1.0] {
        let d = vec![Divergence::UnknownOrder(unknown_order_report(None, filled, 1, "ACCEPTED"))];
        let r = resolve(d, &ReconPolicy::default(), None, Some(a.ctx()));
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
    let d = vec![Divergence::UnknownOrder(unknown_order_report(None, 1.0, 1, "ACCEPTED"))];
    let r = resolve(d, &ReconPolicy::hybrid(), None, Some(a.ctx()));
    assert!(r.events.is_empty());
    assert_eq!(r.alerts.len(), 1);
    assert_eq!(r.alerts[0].kind, DivergenceKind::UnknownOrder);
    assert_eq!(r.alerts[0].proposed_events.len(), 1, "{:?}", r.alerts[0].proposed_events);
    assert!(matches!(r.alerts[0].proposed_events[0], Event::OrderAccepted(_)));
    assert_eq!(r.alerts[0].dedup_key.as_deref(), Some("v-unk"), "keyed for runtime dedup");
}

#[test]
fn terminal_unknown_order_beyond_the_fill_window_is_adopted() {
    // THE adoption case: terminal, executed, and no fill report in-pass (the executions fell
    // outside the lookback) — accept + one cumulative fill.
    let a = Adopt::default();
    let d = vec![unknown_order(None, 1.0, 1)];
    let r = resolve(d, &ReconPolicy::default(), None, Some(a.ctx()));
    assert_eq!(r.events.len(), 2, "{:?}", r.events);
    assert!(matches!(r.events[0], Event::OrderAccepted(_)));
    match &r.events[1] {
        Event::Fill(f) => {
            assert_eq!(f.side, 1);
            assert_eq!(f.last_qty, 1.0);
            assert_eq!(f.last_px, 100.0);
            assert!(f.trade_id.starts_with("EXT-ORD-"));
        }
        other => panic!("expected Fill, got {other:?}"),
    }
}

#[test]
fn terminal_unknown_order_with_in_pass_fill_reports_defers_to_the_fill_lane() {
    // A recently-executed external order surfaces BOTH a MissingFill (real venue trade-id) and
    // an UnknownOrder (coid absent from the registry) in the same pass. The adoption arm must
    // synthesize NOTHING — the fill lane books the qty exactly once under its real trade-id; a
    // same-pass PositionDrift nets against that fill alone.
    let mut a = Adopt::default();
    a.fills.insert("v-unk".into()); // this pass's fill reports cover order v-unk
    let mut mf = ext_fill("t-real-1");
    mf.venue_order_id = "v-unk".into();
    let d = vec![Divergence::MissingFill(mf), unknown_order(None, 1.0, 1), pos_drift(0.0, 1.0)];
    let r = resolve(d, &ReconPolicy::default(), None, Some(a.ctx()));
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
        Divergence::MissingFill(mf),
        Divergence::UnknownOrder(unknown_order_report(None, 2.0, 1, "PARTIALLY_FILLED")),
        pos_drift(1.0, 2.0),
    ];
    let r = resolve(d, &ReconPolicy::default(), None, Some(a.ctx()));
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
    // dropped_unknown_coid inflation either) and no alert row, under both auto-apply and held
    // policies.
    let mut a = Adopt::default();
    a.seen.insert("EXT-ORD-binance-v-unk".into());
    for policy in [ReconPolicy::default(), ReconPolicy::hybrid()] {
        let d = vec![unknown_order(None, 1.0, 1)];
        let r = resolve(d, &policy, None, Some(a.ctx()));
        assert!(r.events.is_empty(), "{policy:?}: {:?}", r.events);
        assert!(r.alerts.is_empty(), "{policy:?}: {:?}", r.alerts);
    }
}

#[test]
fn unknown_order_with_client_id_skips_the_synthesized_accept() {
    // A report that DOES carry a client_order_id (just absent from the local registry) is not
    // externally-placed in the MissingFill sense — no accept is minted, mirroring the
    // MissingFill arm's own `client_order_id.is_none()` gate. Only the Fill rides the report's
    // own coid.
    let a = Adopt::default();
    let d = vec![unknown_order(Some("c-known"), 2.0, -1)];
    let r = resolve(d, &ReconPolicy::default(), None, Some(a.ctx()));
    assert_eq!(r.events.len(), 1, "{:?}", r.events);
    match &r.events[0] {
        Event::Fill(f) => {
            assert_eq!(f.client_order_id, "c-known");
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
    let r = resolve(d, &ReconPolicy::hybrid(), None, Some(a.ctx()));
    assert!(r.events.is_empty(), "no-local-origin kind stays held under hybrid");
    assert_eq!(r.alerts.len(), 1);
    assert_eq!(r.alerts[0].kind, DivergenceKind::UnknownOrder);
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
    let r = resolve(d, &ReconPolicy::default(), None, Some(a.ctx()));
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
    let r = resolve(d, &ReconPolicy::hybrid(), None, Some(a.ctx()));
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
    // times, so a racing second fold dedupes on `ExecutionEngine::on_event`'s
    // `seen_trade_ids` guard instead of double-booking — the structural half of idempotency
    // `external_coid`'s own doc describes for MissingFill.
    let a = Adopt::default();
    let r1 =
        resolve(vec![unknown_order(None, 1.0, 1)], &ReconPolicy::default(), None, Some(a.ctx()));
    let r2 =
        resolve(vec![unknown_order(None, 1.0, 1)], &ReconPolicy::default(), None, Some(a.ctx()));
    let tid = |r: &Recon| {
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

fn balance_drift() -> Divergence {
    Divergence::BalanceDrift {
        venue: "binance".into(),
        asset: "USDT".into(),
        local: 10_000.0,
        venue_bal: 10_150.0,
        ts: 42,
    }
}

#[test]
fn balance_drift_synthesize_folds_one_account_state_no_alert() {
    // synthesize ⇒ adopt venue truth immediately: one AccountState folds, no alert.
    let r = resolve(vec![balance_drift()], &ReconPolicy::default(), None, None);
    assert!(r.alerts.is_empty());
    assert_eq!(r.events.len(), 1);
    match &r.events[0] {
        Event::AccountState(a) => {
            assert_eq!(a.venue.as_str(), "binance");
            assert_eq!(a.balances, vec![("USDT".to_string(), 10_150.0)]);
            assert_eq!(a.ts, 42);
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
        ReconPolicy::hybrid(),
        ReconPolicy { default: ReconMode::Quarantine, ..Default::default() },
    ] {
        let r = resolve(vec![balance_drift()], &policy, None, None);
        assert!(r.events.is_empty(), "never auto-folds a surprise ({policy:?})");
        assert_eq!(r.alerts.len(), 1);
        let a = &r.alerts[0];
        assert_eq!(a.kind, DivergenceKind::BalanceDrift);
        assert_eq!(a.dedup_key.as_deref(), Some("balance:USDT"), "one row per (venue, asset)");
        assert_eq!(a.proposed_events.len(), 1, "the AccountState an operator confirm folds");
        assert!(matches!(a.proposed_events[0], Event::AccountState(_)));
        assert!(a.detail.contains("+150"), "unexplained delta surfaced: {}", a.detail);
    }
}

// --- OrphanLocalPosition resolution (diff's local-side position sweep) ---

fn orphan_local_position(symbol: &str, side: &str, qty: f64) -> Divergence {
    Divergence::OrphanLocalPosition {
        venue: "binance".into(),
        symbol: symbol.into(),
        position_side: side.into(),
        local_qty: qty,
    }
}

#[test]
fn orphan_local_position_never_folds_an_event_under_any_policy() {
    // The load-bearing safety property: there is no defensible price for a synthetic close
    // (LocalView carries qty only), so this kind resolves to ZERO events under EVERY policy —
    // including `synthesize`, which folds everything else it can.
    for policy in [
        ReconPolicy::default(), // Synthesize
        ReconPolicy::hybrid(),
        ReconPolicy { default: ReconMode::Quarantine, ..Default::default() },
        ReconPolicy { default: ReconMode::Hybrid, ..Default::default() },
    ] {
        let r = resolve(vec![orphan_local_position("BTCUSDT", "BOTH", 1.0)], &policy, None, None);
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
        vec![orphan_local_position("BTCUSDT", "BOTH", 1.0)],
        &ReconPolicy::default(),
        None,
        None,
    );
    assert_eq!(r, Recon::default(), "synthesize: a true no-op");
}

#[test]
fn orphan_local_position_holds_one_dedup_keyed_alert_under_hybrid_and_quarantine() {
    // hybrid classifies it no-local-origin (an absent venue row is evidence-by-absence, which
    // an incomplete/symbol-scoped fetch produces just as readily as a genuinely closed
    // position), so BOTH held policies surface exactly one operator alert — and it is
    // dedup-keyed, because nothing heals this divergence and an un-keyed alert would append a
    // new row every pass.
    for policy in [
        ReconPolicy::hybrid(),
        ReconPolicy { default: ReconMode::Quarantine, ..Default::default() },
    ] {
        let r = resolve(vec![orphan_local_position("BTCUSDT", "BOTH", -2.5)], &policy, None, None);
        assert!(r.events.is_empty(), "{policy:?}");
        assert_eq!(r.alerts.len(), 1, "{policy:?}");
        let a = &r.alerts[0];
        assert_eq!(a.kind, DivergenceKind::OrphanLocalPosition);
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
    // The two classification sites must agree: `ReconPolicy::hybrid()`'s per-kind preset and
    // the bare `ReconMode::Hybrid` fallback (`is_local_origin`). Both must HOLD this kind.
    let bare = ReconPolicy { default: ReconMode::Hybrid, ..Default::default() };
    let preset = ReconPolicy::hybrid();
    let d = || vec![orphan_local_position("BTCUSDT", "BOTH", 1.0)];
    assert_eq!(resolve(d(), &bare, None, None), resolve(d(), &preset, None, None));
    assert_eq!(resolve(d(), &bare, None, None).alerts.len(), 1);
}

#[test]
fn orphan_local_position_dedup_key_separates_symbol_and_side() {
    // Hedge mode / multi-symbol: each (symbol, side) gets its own held row, so refreshing one
    // never overwrites another.
    let policy = ReconPolicy::hybrid();
    let d = vec![
        orphan_local_position("BTCUSDT", "LONG", 1.0),
        orphan_local_position("BTCUSDT", "SHORT", -2.0),
        orphan_local_position("ETHUSDT", "BOTH", 3.0),
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
    let policy = ReconPolicy::hybrid();
    let without = resolve(vec![Divergence::MissingFill(ext_fill("t1"))], &policy, None, None);
    let with = resolve(
        vec![
            Divergence::MissingFill(ext_fill("t1")),
            orphan_local_position("ETHUSDT", "BOTH", 4.0),
        ],
        &policy,
        None,
        None,
    );
    assert_eq!(with.events, without.events, "the fill's own events are untouched");
    assert_eq!(with.alerts.len(), without.alerts.len() + 1, "exactly one added alert");
}

// --- OrphanLocalOrder resolution (diff's local-side ORDER sweep; see the module doc) ---

fn orphan_order(coid: &str) -> Divergence {
    Divergence::OrphanLocalOrder { client_order_id: coid.into() }
}

fn quarantine_policy() -> ReconPolicy {
    ReconPolicy { default: ReconMode::Quarantine, ..Default::default() }
}

#[test]
fn orphan_local_order_never_folds_an_event_under_any_policy() {
    // The load-bearing safety property, unchanged by making the kind visible: a synthesized
    // cancel would terminalize an order that may still be RESTING at the venue (this module
    // folds locally and calls no venue), so the kind resolves to ZERO events under EVERY
    // policy — and its alert proposes zero events, so an operator confirm cannot fold one
    // either.
    for policy in [
        ReconPolicy::default(), // Synthesize
        ReconPolicy::hybrid(),
        quarantine_policy(),
        ReconPolicy { default: ReconMode::Hybrid, ..Default::default() },
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
    let r = resolve(vec![orphan_order("c-1")], &ReconPolicy::default(), None, None);
    assert_eq!(r, Recon::default(), "synthesize: a true no-op");
}

#[test]
fn orphan_local_order_holds_one_dedup_keyed_alert_under_hybrid_and_quarantine() {
    // Both held policies surface ONE dedup-keyed, operator-readable, event-free row.
    for policy in [ReconPolicy::hybrid(), quarantine_policy()] {
        let r = resolve(vec![orphan_order("c-1")], &policy, None, None);
        assert!(r.events.is_empty(), "{policy:?}");
        assert_eq!(r.alerts.len(), 1, "{policy:?}: {:?}", r.alerts);
        let a = &r.alerts[0];
        assert_eq!(a.kind, DivergenceKind::OrphanLocalOrder);
        assert_eq!(
            a.dedup_key.as_deref(),
            Some(ORPHAN_LOCAL_ORDER_KEY),
            "one held row per (venue, kind) — NOT one per coid, see the module doc"
        );
        assert!(a.proposed_events.is_empty(), "nothing an operator confirm could fold");
        assert!(a.recover_orders.is_empty());
        assert!(a.detail.contains("c-1"), "names the order: {}", a.detail);
        assert!(a.detail.contains('1'), "carries the count: {}", a.detail);
    }
}

#[test]
fn orphan_local_order_bare_hybrid_default_matches_the_preset() {
    // The two classification sites must agree: `ReconPolicy::hybrid()`'s per-kind preset and
    // the bare `ReconMode::Hybrid` fallback (`is_local_origin`). Both must HOLD this kind —
    // the twin of `orphan_local_position_bare_hybrid_default_matches_the_preset`.
    let bare = ReconPolicy { default: ReconMode::Hybrid, ..Default::default() };
    let preset = ReconPolicy::hybrid();
    let d = || vec![orphan_order("c-1")];
    assert_eq!(resolve(d(), &bare, None, None), resolve(d(), &preset, None, None));
    assert_eq!(resolve(d(), &bare, None, None).alerts.len(), 1);
}

#[test]
fn many_orphan_local_orders_aggregate_into_exactly_one_alert() {
    // The whole reason the coid is NOT the dedup key: the held-alert store never self-clears
    // and its rows are cloned into every published snapshot, so a pass in which the venue
    // report echoes no client ids at all (every live order orphans at once) must still cost
    // ONE row. The count is exact; only the NAMED sample truncates.
    let d: Vec<Divergence> =
        (0..ORPHAN_SAMPLE + 5).map(|i| orphan_order(&format!("c-{i:02}"))).collect();
    let n = d.len();
    let r = resolve(d, &ReconPolicy::hybrid(), None, None);
    assert_eq!(r.alerts.len(), 1, "{:?}", r.alerts);
    let detail = &r.alerts[0].detail;
    assert!(detail.starts_with(&format!("{n} live LOCAL order(s)")), "exact count: {detail}");
    assert!(detail.contains("(+5 more)"), "sample truncated with a remainder: {detail}");
    assert!(detail.contains("c-00"), "sorted sample starts at the lowest coid: {detail}");
    assert!(!detail.contains("c-12"), "beyond the sample cap: {detail}");
}

#[test]
fn a_steady_orphan_set_renders_a_byte_identical_detail() {
    // Sorting is load-bearing, not tidiness: the runtime REFRESHES a dedup-keyed row only when
    // the detail changed, so an unchanged orphan set arriving in a different registry order
    // must render the identical string (otherwise every pass writes a ring note and churns the
    // held row). A CHANGED set must move it.
    let p = ReconPolicy::hybrid();
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
    let policy = ReconPolicy::hybrid();
    let without = resolve(vec![Divergence::MissingFill(ext_fill("t1"))], &policy, None, None);
    let with = resolve(
        vec![orphan_order("c-1"), Divergence::MissingFill(ext_fill("t1"))],
        &policy,
        None,
        None,
    );
    assert_eq!(with.events, without.events, "the fill's own events are untouched");
    assert_eq!(with.alerts.len(), without.alerts.len() + 1, "exactly one added alert");
    assert_eq!(
        with.alerts.last().map(|a| a.kind),
        Some(DivergenceKind::OrphanLocalOrder),
        "aggregate lands last"
    );
}
