//! The per-symbol price/size grid through the gate: `SymbolGrid` overrides as `check` applies them.
//! The pure lookups and builders (`grid_for`, the two `from_properties`) are tested beside them, in
//! `crates/vike-model/src/risk/limits_tests.rs`.

use super::*;
use vike_model::SymbolGrid;

// ---- per-symbol price/size grid ----

fn market_in(symbol: &str, side: i32, qty: f64) -> OrderRequest {
    OrderRequest { symbol: symbol.to_string(), ..market_order(side, qty) }
}

/// THE BUG THE GRID FIXES. An engine's scalars come from ONE symbol's `SymbolProperties`, so a
/// coarse mount lot is applied to a DIFFERENT symbol's order too: `round_to(0.5, Some(1.0))`
/// is `0.0`, and the gate then denies a perfectly valid order as `"non-positive-size"` — a
/// reason that names nothing about the real cause.
#[test]
fn a_coarse_mount_lot_destroys_a_finer_grid_order_without_an_override() {
    let lim = RiskLimits { lot_size: Some(1.0), ..RiskLimits::new() };
    let v = RiskGate::new(lim).check(&market_in("ETHUSDT", 1, 0.5), &RiskContext::default());
    assert!(!v.ok, "0.5 rounds to 0.0 on a lot of 1.0");
    assert_eq!(v.reason, "non-positive-size");
}

/// With ITS OWN grid declared, the same order survives: rounded onto 0.001 rather than 1.0.
#[test]
fn a_declared_symbol_is_rounded_onto_its_own_lot() {
    let mut lim = RiskLimits { lot_size: Some(1.0), ..RiskLimits::new() };
    lim.grid_by_symbol.insert(
        "ETHUSDT".to_string(),
        SymbolGrid { lot_size: Some(0.001), ..SymbolGrid::default() },
    );
    let v = RiskGate::new(lim).check(&market_in("ETHUSDT", 1, 0.5), &RiskContext::default());
    assert!(v.ok, "the finer lot must admit it: {v:?}");
    let admitted = v.request.expect("an admitted verdict carries the request");
    assert!((admitted.qty - 0.5).abs() < 1e-12, "qty {} != 0.5", admitted.qty);
}

/// An override is per SYMBOL, not global: the engine's own symbol keeps the scalars.
#[test]
fn an_override_does_not_leak_to_other_symbols() {
    let mut lim = RiskLimits { lot_size: Some(1.0), ..RiskLimits::new() };
    lim.grid_by_symbol.insert(
        "ETHUSDT".to_string(),
        SymbolGrid { lot_size: Some(0.001), ..SymbolGrid::default() },
    );
    let v = RiskGate::new(lim).check(&market_order(1, 0.5), &RiskContext::default());
    assert!(!v.ok, "BTCUSDT still rounds on the 1.0 scalar");
}

/// Fallback is FIELD-BY-FIELD: an override that pins only `lot_size` must still inherit the
/// engine's `min_qty`. A half-specified grid must not become a way to switch a floor off.
#[test]
fn a_partial_override_still_inherits_the_engines_floors() {
    let mut lim = RiskLimits { lot_size: Some(1.0), min_qty: Some(10.0), ..RiskLimits::new() };
    lim.grid_by_symbol.insert(
        "ETHUSDT".to_string(),
        SymbolGrid { lot_size: Some(0.001), ..SymbolGrid::default() },
    );
    let v = RiskGate::new(lim).check(&market_in("ETHUSDT", 1, 0.5), &RiskContext::default());
    assert!(!v.ok, "0.5 clears the finer lot but not the inherited min_qty of 10");
    assert_eq!(v.reason, "below-min-qty");
}

/// A symbol's own floors apply when it declares them.
#[test]
fn a_declared_symbol_uses_its_own_min_quantity() {
    let mut lim = RiskLimits { lot_size: Some(1.0), min_qty: Some(10.0), ..RiskLimits::new() };
    lim.grid_by_symbol.insert(
        "ETHUSDT".to_string(),
        SymbolGrid { lot_size: Some(0.001), min_qty: Some(0.1), ..SymbolGrid::default() },
    );
    let v = RiskGate::new(lim).check(&market_in("ETHUSDT", 1, 0.5), &RiskContext::default());
    assert!(v.ok, "its own 0.1 floor admits 0.5: {v:?}");
}

// ---- an order judged on its OWN symbol (decision 0173) ----

/// An `AdmitSymbol` row: the grid plus the `on_demand` marker the engine sets.
fn admitted_grid(tick: f64, lot: f64) -> SymbolGrid {
    SymbolGrid {
        tick_size: Some(tick),
        lot_size: Some(lot),
        on_demand: true,
        ..SymbolGrid::default()
    }
}

/// A stop carries `trigger_price` and no `price`: the collar judges the trigger, and it must round
/// it on the ORDER symbol's tick. On the engine-wide tick of 1.0 a 100.4 trigger rounded to 100.0
/// and slipped inside a 0.3 band that its own 0.01 tick puts it outside of.
#[test]
fn the_collar_rounds_a_trigger_on_the_order_symbols_tick() {
    let mut lim = RiskLimits {
        tick_size: Some(1.0),
        price_collar: Some(vike_model::PriceCollar { percent: 0.003, abs_floor: 0.0 }),
        ..RiskLimits::new()
    };
    lim.grid_by_symbol.insert("ETHUSDT".to_string(), admitted_grid(0.01, 0.001));
    let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };
    let stop = |symbol: &str| OrderRequest {
        order_type: vike_model::OrderType::Stop,
        trigger_price: Some(100.4),
        ..market_in(symbol, 1, 1.0)
    };
    let mut g = RiskGate::new(lim);
    let v = g.check(&stop("ETHUSDT"), &ctx);
    assert!(!v.ok && v.reason == "price-collar", "0.4 off the mark on a 0.01 tick: {v:?}");
    // The mounted symbol keeps the scalar tick: a SELL stop at 100.4 goes down to 100.0, inside the band.
    let sell = OrderRequest { side: -1, ..stop("BTCUSDT") };
    assert!(g.check(&sell, &ctx).ok, "the mounted symbol is judged on its own tick");
    // A BUY stop at 100.4 goes UP to 101.0 (`trigger_on_grid`), and the collar judges the trigger it sends.
    let v = g.check(&stop("BTCUSDT"), &ctx);
    assert!(!v.ok && v.reason == "price-collar", "a buy stop is judged at 101: {v:?}");
}

// ---- a TRIGGER goes on the grid by the order's side (decision 0269 row 27, the owner's G1) ----

/// The trigger the gate lets out for a `kind` order of `side` at `trigger`, on a 0.01 tick.
fn sent_trigger(kind: vike_model::OrderType, side: i32, trigger: f64) -> Option<f64> {
    let lim = RiskLimits { tick_size: Some(0.01), ..RiskLimits::new() };
    let req =
        OrderRequest { order_type: kind, trigger_price: Some(trigger), ..market_order(side, 1.0) };
    let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };
    let v = RiskGate::new(lim).check(&req, &ctx);
    v.request.unwrap_or_else(|| panic!("admitted: {}", v.reason)).trigger_price
}

/// A stop's trigger between two ticks goes AWAY from the market, a buy up and a sell down, so it never
/// fires before the level its author stated; a stop-limit's and a take-profit's go as a limit's price, a
/// buy down and a sell up. The same stop then rests at the same price on every venue (the venues' own
/// rules disagreed: truncation, nearest, the limit rule, refusal).
#[test]
fn a_trigger_between_two_ticks_goes_on_the_grid_by_the_side_of_the_order() {
    use vike_model::OrderType::{Stop, StopLimit, TakeProfit};
    assert_eq!(sent_trigger(Stop, 1, 100.005), Some(100.01), "a buy stop goes up");
    assert_eq!(sent_trigger(Stop, -1, 99.995), Some(99.99), "a sell stop goes down");
    assert_eq!(
        sent_trigger(StopLimit, 1, 100.005),
        Some(100.0),
        "a stop-limit's trigger as a limit"
    );
    assert_eq!(sent_trigger(StopLimit, -1, 99.995), Some(100.0));
    assert_eq!(sent_trigger(TakeProfit, 1, 99.995), Some(99.99), "a buy take-profit goes down");
    assert_eq!(sent_trigger(TakeProfit, -1, 100.005), Some(100.01), "a sell take-profit goes up");
    assert_eq!(sent_trigger(Stop, 1, 100.01), Some(100.01), "on the grid: as it is");
    assert_eq!(sent_trigger(Stop, -1, 100.01), Some(100.01));
}

/// What the rule leaves alone: a trailing order's trigger is its ACTIVATION (which side it fires on is the
/// venue's), a trigger the gate has no tick for, and a non-finite trigger (a snapped infinity would be a NaN,
/// which every cap lets through).
#[test]
fn a_trailing_activation_an_unticked_symbol_and_a_non_finite_trigger_keep_their_trigger() {
    use vike_model::OrderType::{Stop, Trailing};
    assert_eq!(sent_trigger(Trailing, 1, 100.005), Some(100.005));
    assert_eq!(sent_trigger(Stop, 1, f64::INFINITY), Some(f64::INFINITY));
    let req =
        OrderRequest { order_type: Stop, trigger_price: Some(100.005), ..market_order(1, 1.0) };
    let v = RiskGate::new(RiskLimits::new()).check(&req, &RiskContext::default());
    assert_eq!(v.request.and_then(|r| r.trigger_price), Some(100.005), "no tick, no grid");
}

/// A RESTING order's trigger (its `price`: the core's held arm, the backtest's book) goes on the grid by
/// the gate's own rule, so a stop the core or the simulator fires waits where a venue would rest it.
#[test]
fn a_resting_orders_trigger_goes_on_the_grid_by_the_gates_rule() {
    use vike_model::{OrderKind, OrderResting};
    let rested = |kind, side, px: f64| {
        let mut o = OrderResting::new(kind, side, 1.0);
        o.price = Some(px);
        working_trigger_on_grid(&mut o, Some(0.01));
        o.price
    };
    assert_eq!(rested(OrderKind::Stop, 1, 100.005), Some(100.01), "a buy stop goes up");
    assert_eq!(rested(OrderKind::Stop, -1, 99.995), Some(99.99), "a sell stop goes down");
    assert_eq!(rested(OrderKind::StopLimit, 1, 100.005), Some(100.0), "a stop-limit as a limit");
    assert_eq!(rested(OrderKind::TakeProfit, -1, 100.005), Some(100.01), "a sell take-profit up");
    assert_eq!(
        rested(OrderKind::Limit, 1, 100.005),
        Some(100.005),
        "a limit's price is no trigger"
    );
    let mut stop = OrderResting::new(OrderKind::Stop, 1, 1.0);
    stop.price = Some(100.005);
    working_trigger_on_grid(&mut stop, None);
    assert_eq!(stop.price, Some(100.005), "no tick, no grid");
}

/// A STOP's amend names its new trigger in `price` (`vike_model::modified_price_is_trigger`), and the gate
/// puts it on the grid the way a submitted stop's trigger goes: a buy up, a sell down, never to nearest.
#[test]
fn a_stops_amended_price_is_its_trigger_and_goes_on_the_grid_as_a_stop() {
    let lim = RiskLimits { tick_size: Some(0.01), ..RiskLimits::new() };
    let amend = |side, px| OrderRequest {
        order_type: vike_model::OrderType::Stop,
        price: Some(px),
        trigger_price: Some(90.0),
        ..market_order(side, 1.0)
    };
    let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };
    let mut g = RiskGate::new(lim);
    let sent =
        |g: &mut RiskGate, req| g.check_modify(&req, &ctx, 0.0).request.and_then(|r| r.price);
    assert_eq!(sent(&mut g, amend(1, 100.004)), Some(100.01), "a buy stop goes up, not to 100.00");
    assert_eq!(
        sent(&mut g, amend(-1, 99.996)),
        Some(99.99),
        "a sell stop goes down, not to 100.00"
    );
}

/// A market order carries no price, so on a symbol with no mark its notional is ZERO and every
/// price-based cap passes it. On an admitted symbol that is refused with a reason the operator can
/// act on; the cap it would have escaped is the point.
#[test]
fn a_priceless_order_on_an_admitted_symbol_needs_a_reference_price() {
    let mut lim = RiskLimits { max_notional_per_order: Some(1_000.0), ..RiskLimits::new() };
    lim.grid_by_symbol.insert("ETHUSDT".to_string(), admitted_grid(0.01, 0.001));
    let mut g = RiskGate::new(lim);
    let v = g.check(&market_in("ETHUSDT", 1, 5.0), &RiskContext::default());
    assert!(!v.ok && v.reason == "no-reference-price", "no mark, no price: {v:?}");
    // With a mark the notional is real and the cap applies as it always did.
    let ctx = RiskContext { mark_price: 3_000.0, ..RiskContext::default() };
    let v = g.check(&market_in("ETHUSDT", 1, 5.0), &ctx);
    assert!(!v.ok && v.reason == "over-max-notional", "5 x 3000 is over the cap: {v:?}");
    assert!(g.check(&market_in("ETHUSDT", 1, 0.1), &ctx).ok, "0.1 x 3000 fits");
    // A limit order carries its own price, but on an admitted symbol that is not a reference: with no
    // mark the collar and the exposure caps would judge it against itself, so it is denied too
    // (decision 0174); with a mark it goes.
    let v = g.check(&limit_in("ETHUSDT", 1, 0.1), &RiskContext::default());
    assert!(!v.ok && v.reason == "no-reference-price", "a limit order is not its own mark: {v:?}");
    assert!(g.check(&limit_in("ETHUSDT", 1, 0.1), &ctx).ok);
}

/// A covered reduce is never stranded by a missing price, as for every other floor.
#[test]
fn a_covered_reduce_on_an_admitted_symbol_needs_no_reference_price() {
    let mut lim = RiskLimits::new();
    lim.grid_by_symbol.insert("ETHUSDT".to_string(), admitted_grid(0.01, 0.001));
    let ctx = RiskContext { position_size: 1.0, ..RiskContext::default() };
    let reduce = OrderRequest { reduce_only: true, ..market_in("ETHUSDT", -1, 1.0) };
    assert!(RiskGate::new(lim).check(&reduce, &ctx).ok, "the close must reach the venue");
}

/// BYTE-IDENTICAL for everything that is not an admitted symbol: the mounted symbol, and a leg a
/// strategy declared at assembly (a grid row WITHOUT the marker) keep passing a priceless order.
#[test]
fn a_priceless_order_elsewhere_is_judged_as_before() {
    let mut lim = RiskLimits::new();
    lim.grid_by_symbol.insert(
        "LEGUSDT".to_string(),
        SymbolGrid { lot_size: Some(0.001), ..SymbolGrid::default() },
    );
    let mut g = RiskGate::new(lim);
    assert!(g.check(&market_in("BTCUSDT", 1, 1.0), &RiskContext::default()).ok, "mounted");
    assert!(g.check(&market_in("LEGUSDT", 1, 1.0), &RiskContext::default()).ok, "declared leg");
}

fn limit_in(symbol: &str, side: i32, qty: f64) -> OrderRequest {
    OrderRequest { symbol: symbol.to_string(), ..limit(side, qty, 3_000.0) }
}
