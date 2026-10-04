use super::*;
use crate::MarkSource;
use crate::{Account, BalanceMode, RiskLimits};
use vike_model::OrderRequest;
use vike_model::events::{FillEvent, OrderFilled};

fn engine() -> ExecutionEngine<RecordingClient> {
    ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    )
}

/// Task 8: `local_view()` must report the registered coid, the folded trade_id, and the net
/// position after one submit+fill — the owned read `recon::diff` (Task 3) consumes across the
/// reconcile driver's thread boundary.
#[test]
fn local_view_reports_order_fill_and_position() {
    let mut eng = engine();
    let mut outbox = Outbox::default();

    let req = OrderRequest {
        client_order_id: "c1".into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(100.0),
        ts: 1,
        ..Default::default()
    };
    eng.submit_order(&req, 1, &mut outbox);
    assert!(eng.registry.contains_key("c1"), "submit registers the coid");

    let fill = FillEvent {
        trade_id: "t1".into(),
        client_order_id: "c1".into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: 1.0,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 1,
        mark_price: Some(100.0),
        position_side: "BOTH".into(),
    };
    // Real venue adapters emit both the bare fold lane and the FSM wrap for one fill (see
    // price_board_wiring.rs's comment on the same idiom).
    eng.on_event(&Event::Fill(fill.clone()), &mut outbox);
    eng.on_event(
        &Event::OrderFilled(OrderFilled { client_order_id: "c1".to_string(), fill, ts: 1 }),
        &mut outbox,
    );

    let owned = eng.local_view();
    assert_eq!(owned.venue, "sim");
    assert!(owned.orders.contains_key("c1"), "coid present in orders");
    assert!(owned.seen_trade_ids.contains("t1"), "trade_id present in seen set");
    assert_eq!(
        owned.positions.get(&("BTCUSDT".to_string(), "BOTH".to_string())),
        Some(&1.0),
        "net position folded"
    );

    // as_view borrows the same content back out as a LocalView.
    let view = owned.as_view();
    assert_eq!(view.venue, "sim");
    assert!(view.orders.contains_key("c1"));
    assert!(view.seen_trade_ids.contains("t1"));
    assert_eq!(view.positions.get(&("BTCUSDT".to_string(), "BOTH".to_string())), Some(&1.0));
}

/// MAJOR-3 (the liquidation law's partition at the ADMITTING gate): an Isolated position
/// is backed by its own walled-off wallet — not the shared equity the gate admits
/// against — so it must no longer inflate the gate's `margin_used` (double-charging a
/// mixed account). The same book with the position CROSS must still be charged: the
/// filter keys on the MODE, and an all-cross book is byte-identical to the old fold.
#[test]
fn gate_margin_used_excludes_isolated_positions() {
    use vike_model::MarginMode;
    let mk = |mode: MarginMode| {
        let mut eng = ExecutionEngine::new(
            Account::new(1.0, "sim", None, BalanceMode::Delta),
            RiskGate::new(RiskLimits { im_requirement: Some(0.1), ..RiskLimits::new() }),
            RecordingClient::default(),
            "sim",
            "BTCUSDT",
        );
        eng.equity_seed = 50.0; // equity 50 (no balance/realized/unreal: avg == mark)
        // an open ETH position that prices 10·100·1·0.1 = 100 margin IF counted
        eng.account.positions.insert(
            ("sim".into(), "ETHUSDT".into(), "BOTH".into()),
            crate::account::PositionEntry {
                size: 10.0,
                avg_px: 100.0,
                margin_mode: mode,
                isolated_margin: mode.is_isolated().then_some(100.0),
            },
        );
        eng.account.set_mark_from("sim", "ETHUSDT", 100.0, MarkSource::VenueMark, 0);
        // the gate's margin fold is resolver-priced now: feed the board's mark slot the
        // same price the write-sites would (account.marks alone no longer prices margin)
        eng.price_board.set_mark("sim", "ETHUSDT", 100.0, 0);
        eng
    };
    let order = OrderRequest {
        client_order_id: "c1".into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(100.0),
        ts: 1,
        ..Default::default()
    };
    // order margin = 100·1·1·0.1 = 10 vs equity 50.
    // Isolated ETH excluded → margin_used 0 → free 50 ≥ 10 → ADMITTED.
    let mut iso = mk(MarginMode::Isolated);
    let mut outbox = Outbox::default();
    iso.submit_order(&order, 1, &mut outbox);
    assert_eq!(
        iso.client.submissions.len(),
        1,
        "an isolated position must not consume the shared gate margin"
    );
    // Cross ETH counted → margin_used 100 > equity 50 → free 0 → DENIED.
    let mut cross = mk(MarginMode::Cross);
    let mut outbox = Outbox::default();
    cross.submit_order(&order, 1, &mut outbox);
    assert!(
        cross.client.submissions.is_empty(),
        "the same book cross-margined must still be charged (filter keys on mode)"
    );
    assert!(
        outbox.0.iter().any(|e| matches!(e, Event::OrderDenied(_))),
        "cross case denies through the normal veto path"
    );
}

fn order_report(coid: &str, status: &str, filled: f64) -> vike_model::OrderStatusReport {
    vike_model::OrderStatusReport {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        venue_order_id: "v7".into(),
        client_order_id: Some(coid.into()),
        side: 1,
        order_type: "limit".into(),
        qty: 2.0,
        filled_qty: filled,
        avg_px: if filled > 0.0 { 100.0 } else { 0.0 },
        status: status.into(),
        ts: 5,
    }
}

/// Order-loss recovery (recon JournalDivergence confirm): `reregister_orders` INSERT-ONLY re-seeds
/// a lost order from its venue report (status/filled/venue_id restored, `created_ms: None` adopted),
/// never clobbers a coid already present, and skips reports with no client_order_id.
#[test]
fn reregister_orders_reseeds_lost_order_insert_only() {
    let mut eng = engine();

    // an order local already knows must NOT be clobbered by a re-register.
    let mut outbox = Outbox::default();
    let req = OrderRequest {
        client_order_id: "keep".into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(100.0),
        ..Default::default()
    };
    eng.submit_order(&req, 1, &mut outbox);
    let keep_before = eng.registry.get("keep").cloned().unwrap();

    // one lost order to recover + one no-coid report (skipped) + the already-known "keep".
    let no_coid = vike_model::OrderStatusReport {
        client_order_id: None,
        ..order_report("x", "ACCEPTED", 0.0)
    };
    let n = eng.reregister_orders(&[
        order_report("lost", "PARTIALLY_FILLED", 0.5),
        no_coid,
        order_report("keep", "CANCELED", 0.0), // already known → must be left untouched
    ]);
    assert_eq!(n, 1, "only the genuinely-lost order is re-registered");

    let lost = eng.registry.get("lost").expect("lost order re-registered");
    assert_eq!(lost.status, OrderStatus::PartiallyFilled, "status reconstructed from the report");
    assert_eq!(lost.filled_qty, 0.5);
    assert_eq!(lost.avg_fill_px, 100.0);
    assert_eq!(lost.venue_order_id.as_deref(), Some("v7"));
    assert_eq!(lost.created_ms, None, "adopted order is never swept by the stuck watchdog");

    assert_eq!(eng.registry.get("keep"), Some(&keep_before), "existing order untouched");
    assert!(!eng.registry.contains_key("x"), "a no-coid report is skipped");

    // idempotent: a second pass with the same lost report (now present) re-registers nothing.
    assert_eq!(eng.reregister_orders(&[order_report("lost", "FILLED", 2.0)]), 0);
    assert_eq!(
        eng.registry.get("lost").unwrap().status,
        OrderStatus::PartiallyFilled,
        "not clobbered"
    );
}

// ---------------------------------------------------------------------------------------
// 4th #458-class multiplier-in-context regression (live-vs-backtest divergence ledger:
// #458 gate notional, #477 UI cap, #479 ibkr_mount): `gate_and_register` minted
// `ctx.multiplier` ONLY inside the armed buying-power branch (`im_for` Some) — the
// unarmed else returned 1.0 — while the gate's notional (`qty × ref_price ×
// ctx.multiplier`) feeds min_notional AND max_notional_per_order regardless of the
// margin lane. So a mult≠1 instrument with notional floors/caps armed but no margin
// lane (the live default mount) was judged at multiplier 1.0; the backtest ctx carries
// the real multiplier unconditionally.
// ---------------------------------------------------------------------------------------

/// Engine with floors/caps armed: floor 5, cap 100; optional per-symbol multiplier grid
/// and optional buying-power lane (`im_requirement`).
fn mult_gate_engine(grid_mult: Option<f64>, im: Option<f64>) -> ExecutionEngine<RecordingClient> {
    let grid: Option<IndexMap<String, f64>> =
        grid_mult.map(|m| [("BTCUSDT".to_string(), m)].into_iter().collect());
    let mut eng = ExecutionEngine::new(
        Account::new(1.0, "sim", grid, BalanceMode::Delta),
        RiskGate::new(RiskLimits {
            min_notional: Some(5.0),
            max_notional_per_order: Some(100.0),
            im_requirement: im,
            ..RiskLimits::new()
        }),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    eng.equity_seed = 1_000.0; // ample equity for the armed lane; ignored (0.0 ctx) unarmed
    eng
}

fn buy_limit(coid: &str, qty: f64, px: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty,
        order_type: "limit".into(),
        price: Some(px),
        ts: 1,
        ..Default::default()
    }
}

fn denied_reason(outbox: &Outbox) -> Option<String> {
    outbox.0.iter().find_map(|e| match e {
        Event::OrderDenied(d) => Some(d.reason.to_string()),
        _ => None,
    })
}

/// THE REGRESSION (cap side): margin lane UNARMED + multiplier 100. qty·px = 5.0 sits on
/// the floor and under the 100 cap at multiplier 1.0 — the pre-fix ctx ADMITTED it — but
/// the real notional is 5·100 = 500 > cap, so it must be DENIED.
#[test]
fn unarmed_lane_notional_cap_judged_with_contract_multiplier() {
    let mut eng = mult_gate_engine(Some(100.0), None);
    let mut outbox = Outbox::default();
    eng.submit_order(&buy_limit("cap", 0.5, 10.0), 1, &mut outbox);
    assert_eq!(denied_reason(&outbox).as_deref(), Some("over-max-notional"));
    assert!(eng.client.submissions.is_empty(), "over-cap notional must not reach the venue");
    assert!(!eng.registry.contains_key("cap"), "a denied order never enters the registry");
}

/// THE REGRESSION (floor mirror): qty·px = 0.1 < floor 5 — the pre-fix ctx wrongly DENIED
/// it below-min-notional — but the real notional 0.1·100 = 10 clears the floor and sits
/// under the cap, so it must be ADMITTED.
#[test]
fn unarmed_lane_notional_floor_judged_with_contract_multiplier() {
    let mut eng = mult_gate_engine(Some(100.0), None);
    let mut outbox = Outbox::default();
    eng.submit_order(&buy_limit("floor", 0.01, 10.0), 1, &mut outbox);
    assert_eq!(denied_reason(&outbox), None);
    assert_eq!(eng.client.submissions.len(), 1, "real notional clears the floor → submitted");
    assert!(eng.registry.contains_key("floor"));
}

/// THE COMPAT PIN: multiplier 1.0 (symbol absent from the grid — every production mount
/// today except deribit post-#484). Verdicts are the raw qty·px judgment in BOTH lanes,
/// byte-identical to the pre-fix ctx (`x * 1.0` is an IEEE-754 no-op; the unarmed branch
/// carried the literal 1.0 before, `multiplier_of`'s default 1.0 now).
#[test]
fn multiplier_one_verdicts_identical_across_lanes() {
    for im in [None, Some(0.001)] {
        // below the floor → denied, both lanes
        let mut eng = mult_gate_engine(None, im);
        let mut outbox = Outbox::default();
        eng.submit_order(&buy_limit("lo", 0.1, 10.0), 1, &mut outbox);
        assert_eq!(denied_reason(&outbox).as_deref(), Some("below-min-notional"), "im={im:?}");
        // over the cap → denied, both lanes
        let mut eng = mult_gate_engine(None, im);
        let mut outbox = Outbox::default();
        eng.submit_order(&buy_limit("hi", 20.0, 10.0), 1, &mut outbox);
        assert_eq!(denied_reason(&outbox).as_deref(), Some("over-max-notional"), "im={im:?}");
        // in-band → admitted, both lanes
        let mut eng = mult_gate_engine(None, im);
        let mut outbox = Outbox::default();
        eng.submit_order(&buy_limit("ok", 1.0, 10.0), 1, &mut outbox);
        assert_eq!(denied_reason(&outbox), None, "im={im:?}");
        assert_eq!(eng.client.submissions.len(), 1, "im={im:?}");
    }
}

/// THE ARMED-LANE PIN: the armed branch already minted the real multiplier pre-fix — the
/// hoist must not change it. Same mult-100 over-cap order, margin lane ARMED, still
/// denies over-max-notional; an in-band mult-100 order still clears notional AND buying
/// power (notional 0.05·10·100 = 50 ∈ [5, 100]; IM = 50·0.01 = 0.5 ≤ equity 1000).
#[test]
fn armed_lane_multiplier_behavior_unchanged() {
    let mut eng = mult_gate_engine(Some(100.0), Some(0.01));
    let mut outbox = Outbox::default();
    eng.submit_order(&buy_limit("cap", 0.5, 10.0), 1, &mut outbox);
    assert_eq!(denied_reason(&outbox).as_deref(), Some("over-max-notional"));

    let mut eng = mult_gate_engine(Some(100.0), Some(0.01));
    let mut outbox = Outbox::default();
    eng.submit_order(&buy_limit("ok", 0.05, 10.0), 1, &mut outbox);
    assert_eq!(denied_reason(&outbox), None);
    assert_eq!(eng.client.submissions.len(), 1);
}

/// ⚠ **AN ORDER ALREADY IN FLIGHT MUST CONSUME BUYING POWER** — it did not, so the gate
/// admitted a second order as though the first committed nothing.
///
/// `RiskContext::margin_used` folded open POSITIONS only. An order that is live at the venue
/// but not yet filled holds no position, so it contributed 0 — and a strategy that submits
/// twice before the first fill got both admitted against the same equity. The BACKTEST has
/// always counted its pending orders (`SimBroker::margin_in_use_pending_aware`), and that
/// side's doc claimed it mirrored live "field for field"; live was the permissive one.
///
/// The numbers: equity 50, im 0.1, mark 100. One order of 3 commits 3·100·1·0.1 = 30, leaving
/// free 20. A second order of 3 needs 30 > 20 and must be DENIED. Before the fix `margin_used`
/// was 0, free was 50, and it was admitted.
///
/// NON-VACUOUS in both directions: the FIRST order is asserted to still be admitted (so the
/// term did not simply break the gate), and a size that fits inside the remaining 20 is
/// asserted to still pass (so the denial is the margin arithmetic, not a blanket refusal of any
/// second order).
#[test]
fn a_live_unfilled_order_consumes_buying_power() {
    let mk = || {
        let mut eng = ExecutionEngine::new(
            Account::new(1.0, "sim", None, BalanceMode::Delta),
            RiskGate::new(RiskLimits { im_requirement: Some(0.1), ..RiskLimits::new() }),
            RecordingClient::default(),
            "sim",
            "BTCUSDT",
        );
        eng.equity_seed = 50.0;
        eng.account.set_mark_from("sim", "BTCUSDT", 100.0, MarkSource::VenueMark, 0);
        eng.price_board.set_mark("sim", "BTCUSDT", 100.0, 0);
        eng
    };
    let order = |coid: &str, qty: f64| OrderRequest {
        client_order_id: coid.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty,
        ts: 1,
        ..Default::default()
    };

    let mut eng = mk();
    let mut outbox = Outbox::default();
    eng.submit_order(&order("c1", 3.0), 1, &mut outbox);
    assert_eq!(eng.client.submissions.len(), 1, "the first order is admitted: 30 <= 50");

    // The venue accepts it — live, un-filled, holding no position yet.
    eng.on_event(
        &Event::OrderAccepted(vike_model::events::OrderAccepted {
            client_order_id: "c1".into(),
            venue_order_id: None,
            ts: 1,
        }),
        &mut outbox,
    );

    // A second order of the same size needs 30 against a remaining 20.
    let mut ob2 = Outbox::default();
    eng.submit_order(&order("c2", 3.0), 2, &mut ob2);
    assert_eq!(
        eng.client.submissions.len(),
        1,
        "the second order must be DENIED — the first committed 30 of the 50 equity, and \
             counting positions alone made that commitment invisible"
    );
    assert!(
        ob2.0.iter().any(|e| matches!(e, Event::OrderDenied(_))),
        "and it denies through the normal veto path, not by vanishing"
    );

    // ...but one that FITS in the remaining 20 still passes, so this is the margin arithmetic
    // rather than a blanket refusal of any second order.
    let mut ob3 = Outbox::default();
    eng.submit_order(&order("c3", 1.0), 3, &mut ob3);
    assert_eq!(eng.client.submissions.len(), 2, "10 <= the remaining 20 is still admitted");
}
