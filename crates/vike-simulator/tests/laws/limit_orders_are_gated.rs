//! ⚠ **Limit, stop and trailing orders must face the pre-trade `[risk]` gate in the backtest.**
//!
//! `OrderBrokerSimulator::gate_order` (formerly `gate_market_order`) was called from `submit_market` and
//! `submit_market_close` ALONE. Every other submitting verb — `submit_limit`, `submit_stop`,
//! `submit_trailing`, `submit_limit_close` and `HftBroker::submit_limit_tagged` — went straight to
//! `push_pending` with no gate at all.
//!
//! So a backtest of a limit strategy ran with the operator's `[risk]` budget effectively switched
//! off: no per-order notional cap, no projected-exposure cap, no buying-power / `max_leverage`
//! check. LIVE gates all of them, which means the BACKTEST was the permissive side — a strategy
//! could validate here and be refused in production. The market makers quote limits exclusively,
//! so their entire backtest history sat in that gap.
//!
//! `harness_risk_wiring.rs` proves the same wiring for the MARKET path end-to-end. This is its
//! limit twin, driven directly against `OrderBrokerSimulator` so the assertion is about the verb rather than
//! about a profile plumbing chain that file already covers.

use vike_marketdata::Bar;
use vike_model::RiskLimits;
use vike_model::{OrderBroker, OrderPlacement, Strategy};
use vike_simulator::{OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig};

const SYM: &str = "SYM0";
const VENUE: &str = "TEST";
const PX: f64 = 100.0;

fn bars(n: usize) -> Vec<Bar> {
    vike_simulator::test_support::flat_minute_bars(n, PX, 1_000.0)
}

/// Submits ONE order of `kind` on the first bar, then nothing.
struct SubmitOnce {
    kind: &'static str,
    qty: f64,
    done: bool,
}

impl Strategy<OrderBrokerSimulator> for SubmitOnce {
    fn on_bar(&mut self, b: &mut OrderBrokerSimulator, _bar: &Bar) {
        if self.done {
            return;
        }
        self.done = true;
        match self.kind {
            // A RESTING buy, well below the mark so it never crosses — the point is whether it is
            // allowed to rest at all, not whether it fills.
            "limit" => b.submit_limit(SYM, 1, self.qty, PX * 0.5, 1.0, true, None),
            "stop" => b.submit_stop(SYM, 1, self.qty, PX * 1.5, 1.0, true),
            "trailing" => b.submit_trailing(SYM, 1, self.qty, 5.0, 1.0, true),
            other => panic!("unknown kind {other}"),
        }
    }
}

/// Returns (orders that rested, the gate's drop reasons).
fn run(kind: &'static str, qty: f64, cap: Option<f64>) -> (usize, Vec<String>) {
    let params = StrategyEngineConfig {
        cash: 1_000_000.0,
        default_venue: Some(VENUE.to_string()),
        risk_limits: cap
            .map(|c| RiskLimits { max_notional_per_order: Some(c), ..RiskLimits::new() }),
        ..Default::default()
    };
    let strat = SubmitOnce { kind, qty, done: false };
    let mut e = StrategyEngine::new(vec![(SYM.to_string(), bars(3))], strat, params);
    let _ = e.run();
    let rested = e.core.symbol[0].pending.len();
    let reasons = e.core.dropped.iter().map(|d| d.reason.clone()).collect();
    (rested, reasons)
}

/// ⚠ THE GATE. A limit order whose notional exceeds `max_notional_per_order` must be refused, with
/// the live `RiskGate`'s own reason string — exactly as the market path already is.
///
/// NON-VACUOUS: the same order with NO cap is asserted to rest, so the refusal is the cap and not
/// the harness. 100 units x 100.0 = 10_000 notional against a 250.0 cap.
#[test]
fn a_limit_order_over_the_cap_is_refused() {
    let (rested, reasons) = run("limit", 100.0, Some(250.0));
    assert_eq!(rested, 0, "the over-cap limit order must not rest: reasons={reasons:?}");
    assert!(
        reasons.iter().any(|r| r == "over-max-notional"),
        "and the refusal must carry the RiskGate's own reason: {reasons:?}"
    );

    let (rested, reasons) = run("limit", 100.0, None);
    assert_eq!(rested, 1, "without a cap the identical order rests: reasons={reasons:?}");
}

/// The same for a STOP, whose reference price is its TRIGGER rather than a limit price —
/// `check_inner`'s `price.or(trigger_price)` is what makes that work without a second rule.
#[test]
fn a_stop_order_over_the_cap_is_refused() {
    let (rested, reasons) = run("stop", 100.0, Some(250.0));
    assert_eq!(rested, 0, "the over-cap stop must not rest: reasons={reasons:?}");
    assert!(reasons.iter().any(|r| r == "over-max-notional"), "{reasons:?}");

    let (rested, _) = run("stop", 100.0, None);
    assert_eq!(rested, 1, "without a cap the identical stop rests");
}

/// And a TRAILING order, which carries no price of its own and falls back to the mark —
/// `check_inner`'s `unwrap_or(ctx.mark_price)`.
#[test]
fn a_trailing_order_over_the_cap_is_refused() {
    let (rested, reasons) = run("trailing", 100.0, Some(250.0));
    assert_eq!(rested, 0, "the over-cap trailing order must not rest: reasons={reasons:?}");
    assert!(reasons.iter().any(|r| r == "over-max-notional"), "{reasons:?}");

    let (rested, _) = run("trailing", 100.0, None);
    assert_eq!(rested, 1, "without a cap the identical trailing order rests");
}

/// ⚠ BYTE-IDENTITY GUARD: a compliant order is untouched. Without this the three tests above are
/// satisfied by a gate that refuses everything.
#[test]
fn a_compliant_limit_order_is_unaffected_by_the_gate() {
    let (rested, reasons) = run("limit", 1.0, Some(250.0));
    assert_eq!(rested, 1, "1 x 100 = 100 notional is under the 250 cap: reasons={reasons:?}");
    assert!(reasons.is_empty(), "and nothing is dropped: {reasons:?}");
}

// ---- the ACCOUNT-AGGREGATE exposure ceiling, backtest side ----------------------------------
//
// `RiskLimits::max_account_exposure` is a CROSS-SYMBOL axis, so it is the one gate lane a
// single-symbol harness cannot exercise at all: with one symbol the account sum is empty and the
// lane collapses onto the per-symbol projection. These two tests run a real two-symbol
// `StrategyEngine`, which is the only place `OrderBrokerSimulator::gate_order`'s fold of
// `RiskContext::account_exposure_excl_order` is reachable.
//
// Same standing rule as the rest of this file: **the backtest must never be the PERMISSIVE side.**
// A `0.0` there would vacate the account ceiling for every backtest while live refused it, so a
// strategy could validate here and be denied in production — which is exactly the gap that made
// `gate_market_order` grow into `gate_order` in the first place.

const SYMBOL1: &str = "SYM1";

/// Buys `qty` of [`SYMBOL1`] on step 0 and `qty` of [`SYM`] on step 2, once each. `on_bar` fires once
/// per symbol per step, hence the latches: the account state under test is "one filled position
/// plus one new order", not "however many bars there were".
struct LoadThenBuyOther {
    qty: f64,
    first: bool,
    second: bool,
}

impl Strategy<OrderBrokerSimulator> for LoadThenBuyOther {
    fn on_bar(&mut self, b: &mut OrderBrokerSimulator, _bar: &Bar) {
        if b.index == 0 && !self.first {
            self.first = true;
            b.submit(SYMBOL1, 1, self.qty, 1.0, true, None);
        }
        if b.index == 2 && !self.second {
            self.second = true;
            b.submit(SYM, 1, self.qty, 1.0, true, None);
        }
    }
}

/// Returns `(SYM position after the run, the gate's drop reasons)` for a two-symbol run under an
/// optional account ceiling.
fn run_two_symbol(qty: f64, account_cap: Option<f64>) -> (f64, Vec<String>) {
    let params = StrategyEngineConfig {
        cash: 1_000_000.0,
        default_venue: Some(VENUE.to_string()),
        risk_limits: account_cap
            .map(|c| RiskLimits { max_account_exposure: Some(c), ..RiskLimits::new() }),
        ..Default::default()
    };
    let strat = LoadThenBuyOther { qty, first: false, second: false };
    let mut e = StrategyEngine::new(
        vec![(SYM.to_string(), bars(5)), (SYMBOL1.to_string(), bars(5))],
        strat,
        params,
    );
    let _ = e.run();
    let reasons = e.core.dropped.iter().map(|d| d.reason.clone()).collect();
    (e.core.symbol[0].pos.size, reasons)
}

/// ⚠ **THE BACKTEST FOLDS THE ACCOUNT SUM.** A position in ANOTHER symbol is counted against the
/// ceiling, so the second order is refused here exactly as the live gate refuses it.
///
/// 40 units of `SYMBOL1` fill at 100 (4 000 of account exposure); the 40 units of `SYM` that follow
/// project another 4 000, and 8 000 is over the 6 000 ceiling. NON-VACUOUS in two directions: the
/// FIRST order is admitted (it is inside the ceiling on its own), and the identical run with no
/// ceiling fills the second — pinned by the test below.
#[test]
fn the_backtest_counts_another_symbols_position_against_the_account_ceiling() {
    let (pos, reasons) = run_two_symbol(40.0, Some(6_000.0));
    assert_eq!(pos, 0.0, "the second symbol's order must be refused whole: reasons={reasons:?}");
    assert!(
        reasons.iter().any(|r| r.starts_with("over-account-exposure")),
        "…under the live gate's OWN account reason, never the per-symbol one: {reasons:?}"
    );
}

/// …and the same run with NO ceiling fills it, so the refusal above is the ceiling and not the
/// harness — and an unarmed backtest is byte-identical to before this axis existed.
#[test]
fn the_backtest_is_unchanged_when_no_account_ceiling_is_armed() {
    let (pos, reasons) = run_two_symbol(40.0, None);
    assert_eq!(pos, 40.0, "without a ceiling the identical order fills: reasons={reasons:?}");
    assert!(reasons.is_empty(), "…and nothing is dropped at all: {reasons:?}");
}

/// **A RESTING ORDER COUNTS IN THE BACKTEST TOO** — the sim's twin of the live producer's
/// in-flight half.
///
/// Without it a strategy could rest N orders inside one fill window and have each judged as though
/// the others committed nothing, which is the hole `Engine::live_order_margin` records on
/// the buying-power lane. Here the first order is a LIMIT far below the mark, so it rests unfilled
/// for the whole run: only a fold that counts working orders can refuse the second.
#[test]
fn the_backtest_counts_a_resting_order_against_the_account_ceiling() {
    struct RestThenBuy {
        rested: bool,
        bought: bool,
    }
    impl Strategy<OrderBrokerSimulator> for RestThenBuy {
        fn on_bar(&mut self, b: &mut OrderBrokerSimulator, _bar: &Bar) {
            if b.index == 0 && !self.rested {
                self.rested = true;
                // Far below the mark: it never crosses, so it is still WORKING when the next
                // order is judged. ⚠ A working order is valued at the MARK, not at its own limit
                // price — the same basis the live producer prices it on, and the same basis the
                // position it would become is valued on.
                b.submit_limit(SYMBOL1, 1, 20.0, PX * 0.5, 1.0, true, None);
            }
            if b.index == 2 && !self.bought {
                self.bought = true;
                b.submit(SYM, 1, 40.0, 1.0, true, None);
            }
        }
    }
    let run = |cap: Option<f64>| {
        let params = StrategyEngineConfig {
            cash: 1_000_000.0,
            default_venue: Some(VENUE.to_string()),
            risk_limits: cap
                .map(|c| RiskLimits { max_account_exposure: Some(c), ..RiskLimits::new() }),
            ..Default::default()
        };
        let mut e = StrategyEngine::new(
            vec![(SYM.to_string(), bars(5)), (SYMBOL1.to_string(), bars(5))],
            RestThenBuy { rested: false, bought: false },
            params,
        );
        let _ = e.run();
        let reasons: Vec<String> = e.core.dropped.iter().map(|d| d.reason.clone()).collect();
        (e.core.symbol[0].pos.size, e.core.symbol[1].pending.len(), reasons)
    };

    // The resting order is 20 units valued at the mark 100 = 2 000 of committed exposure; the
    // market buy projects 4 000. 6 000 is over a 5 000 ceiling, and ONLY because the resting order
    // counts — a positions-only fold sees 4 000 and admits.
    let (pos, resting, reasons) = run(Some(5_000.0));
    assert_eq!(resting, 1, "precondition: the limit really is still working");
    assert_eq!(pos, 0.0, "the market buy must be refused: reasons={reasons:?}");
    assert!(
        reasons.iter().any(|r| r.starts_with("over-account-exposure")),
        "a positions-only fold sees an empty account here and admits: {reasons:?}"
    );

    // …and with the ceiling raised just above the pair, the same run fills — so the refusal is the
    // resting order's 2 000 and nothing else.
    let (pos, resting, reasons) = run(Some(7_000.0));
    assert_eq!(resting, 1);
    assert_eq!(pos, 40.0, "6 000 fits under 7 000: reasons={reasons:?}");
}

// ---- the BUYING-POWER lane's margin used, the live rule ------------------------------------
//
// Live counts every live un-filled order, gross, and skips a covered reduce
// (`Engine::live_order_margin`). The backtest counted resting MARKET orders only, netted
// per symbol, covered reduces included, so a limit strategy validated here and was refused live.
// Every run below: equity 10 000 at price 100 with `initial_margin` 1.0, so the whole budget is 100 units.

/// Calls `act` with the step index on every bar of a one-symbol engine.
struct Script(fn(&mut OrderBrokerSimulator, usize));

impl Strategy<OrderBrokerSimulator> for Script {
    fn on_bar(&mut self, b: &mut OrderBrokerSimulator, _bar: &Bar) {
        let step = b.index;
        (self.0)(b, step);
    }
}

fn margin_limits() -> RiskLimits {
    RiskLimits { initial_margin_requirement: Some(1.0), ..RiskLimits::new() }
}

/// Returns `(final position, the gate's drop reasons)`.
fn margin_run(act: fn(&mut OrderBrokerSimulator, usize), limits: RiskLimits) -> (f64, Vec<String>) {
    let params = StrategyEngineConfig {
        cash: 10_000.0,
        default_venue: Some(VENUE.to_string()),
        risk_limits: Some(limits),
        // a tagged quote is the maker law's (the only order placed by tag here)
        optimistic_quotes: true,
        ..Default::default()
    };
    let mut e = StrategyEngine::new(vec![(SYM.to_string(), bars(4))], Script(act), params);
    let _ = e.run();
    let reasons = e.core.dropped.iter().map(|d| d.reason.clone()).collect();
    (e.core.symbol[0].pos.size, reasons)
}

/// ⚠ **A RESTING LIMIT COMMITS BUYING POWER.** It rests far below the mark and is charged at the
/// mark, as live charges it: 60 × 100 = 6 000 committed, 4 000 free, and a market order of 60 needs
/// 6 000. The market-only fold saw 0 committed and filled it. NON-VACUOUS: 40 fits exactly.
#[test]
fn a_resting_limit_counts_against_buying_power() {
    let (pos, reasons) = margin_run(
        |b, step| match step {
            0 => b.submit_limit(SYM, 1, 60.0, PX * 0.5, 0.0, true, None),
            1 => b.submit(SYM, 1, 60.0, 0.0, true, None),
            _ => {}
        },
        margin_limits(),
    );
    assert_eq!(reasons, ["insufficient-margin"], "6 000 + 6 000 > 10 000");
    assert_eq!(pos, 0.0);

    let (pos, reasons) = margin_run(
        |b, step| match step {
            0 => b.submit_limit(SYM, 1, 60.0, PX * 0.5, 0.0, true, None),
            1 => b.submit(SYM, 1, 40.0, 0.0, true, None),
            _ => {}
        },
        margin_limits(),
    );
    assert!(reasons.is_empty(), "6 000 + 4 000 fits: {reasons:?}");
    assert_eq!(pos, 40.0);
}

/// ...and so does a resting TAGGED quote, the shape the maker files place every order in.
#[test]
fn a_resting_tagged_quote_counts_against_buying_power() {
    let (pos, reasons) = margin_run(
        |b, step| match step {
            0 => b.place("", "bid", 1, 60.0, OrderPlacement::Limit { price: PX * 0.5 }, false),
            1 => b.submit(SYM, 1, 60.0, 0.0, true, None),
            _ => {}
        },
        margin_limits(),
    );
    assert_eq!(reasons, ["insufficient-margin"], "6 000 + 6 000 > 10 000");
    assert_eq!(pos, 0.0);
}

/// ⚠ **OPPOSED ORDERS DO NOT NET AWAY.** Buy 40 and sell 40 are 8 000 committed, gross, so a third
/// order of 40 needs 4 000 against 2 000 free. A signed sum reads them as 0 committed and admits it.
#[test]
fn opposed_orders_commit_gross_margin() {
    let (pos, reasons) = margin_run(
        |b, step| {
            if step == 0 {
                b.submit(SYM, 1, 40.0, 0.0, true, None);
                b.submit(SYM, -1, 40.0, 0.0, true, None);
                b.submit(SYM, 1, 40.0, 0.0, true, None);
            }
        },
        margin_limits(),
    );
    assert_eq!(reasons, ["insufficient-margin"], "the third order does not fit");
    assert_eq!(pos, 0.0, "the first two fill and cancel out");
}

/// ⚠ **A COVERED REDUCE COMMITS NOTHING.** Long 50 holds 5 000. Selling that 50 frees margin, so
/// counting it left 0 free and refused an order that fits. Charged as live charges it, 5 000 is
/// free and the add of 50 passes.
#[test]
fn a_covered_reduce_commits_no_margin() {
    let (pos, reasons) = margin_run(
        |b, step| match step {
            0 => b.submit(SYM, 1, 50.0, 0.0, true, None),
            1 => {
                b.submit(SYM, -1, 50.0, 0.0, true, None);
                b.submit(SYM, 1, 50.0, 0.0, true, None);
            }
            _ => {}
        },
        margin_limits(),
    );
    assert!(reasons.is_empty(), "5 000 used, 5 000 free, 5 000 asked: {reasons:?}");
    assert_eq!(pos, 50.0);
}

/// ⚠ **`max_sizing_equity` caps what the buying-power lane spends against**, as live's
/// `Engine::sizing_equity` does. Equity is 10 000, the ceiling 5 000: 6 000 of margin is
/// refused, 4 000 is not.
#[test]
fn the_sizing_equity_ceiling_capabilities_the_margin_lane() {
    let capped = || RiskLimits { max_sizing_equity: Some(5_000.0), ..margin_limits() };

    let (pos, reasons) = margin_run(
        |b, step| {
            if step == 0 {
                b.submit(SYM, 1, 60.0, 0.0, true, None);
            }
        },
        capped(),
    );
    assert_eq!(reasons, ["insufficient-margin"]);
    assert_eq!(pos, 0.0);

    let (pos, reasons) = margin_run(
        |b, step| {
            if step == 0 {
                b.submit(SYM, 1, 40.0, 0.0, true, None);
            }
        },
        capped(),
    );
    assert!(reasons.is_empty(), "{reasons:?}");
    assert_eq!(pos, 40.0);
}

/// Long 150 at 100 first; then a market buy of 60, one of 50 and a reducing limit sell of 10.
fn leverage_script(b: &mut OrderBrokerSimulator, step: usize) {
    match step {
        0 => b.submit(SYM, 1, 150.0, 0.0, true, None),
        2 => {
            b.submit(SYM, 1, 60.0, 0.0, true, None);
            b.submit(SYM, 1, 50.0, 0.0, true, None);
            b.submit_limit(SYM, -1, 10.0, PX * 1.5, 0.0, true, None);
        }
        _ => {}
    }
}

/// **The run's MOUNT leverage cap** (`StrategyEngineConfig::max_mount_leverage`, judged by the live
/// drain's one judge, the run as its one mount): with 10 000 of equity and a cap of 2 a long of 150
/// at 100 holds 15 000, so a buy of 60 (21 000) is refused whole in the live words, one of 50 lands
/// exactly at 20 000 and fills, and a reducing sell is not judged. No gate is armed: the cap judges
/// on its own. NON-VACUOUS: with no cap the buy of 60 fills too.
#[test]
fn the_mount_leverage_cap_refuses_an_opening_order_past_it() {
    let run = |cap: Option<f64>| {
        let params = StrategyEngineConfig {
            cash: 10_000.0,
            default_venue: Some(VENUE.to_string()),
            max_mount_leverage: cap,
            ..Default::default()
        };
        let script = Script(leverage_script);
        let mut e = StrategyEngine::new(vec![(SYM.to_string(), bars(5))], script, params);
        let _ = e.run();
        let reasons: Vec<String> = e.core.dropped.iter().map(|d| d.reason.clone()).collect();
        (e.core.symbol[0].pos.size, reasons)
    };
    let (pos, reasons) = run(Some(2.0));
    assert_eq!(reasons.len(), 1, "{reasons:?}");
    assert!(reasons[0].starts_with(vike_execution::risk::MOUNT_LEVERAGE_REFUSAL), "{reasons:?}");
    assert_eq!(pos, 200.0, "150 then 50; the buy of 60 never went");

    let (pos, reasons) = run(None);
    assert!(reasons.is_empty(), "{reasons:?}");
    assert_eq!(pos, 260.0);
}
