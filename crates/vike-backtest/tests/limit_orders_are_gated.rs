//! ⚠ **Limit, stop and trailing orders must face the pre-trade `[risk]` gate in the backtest.**
//!
//! `SimBroker::gate_order` (formerly `gate_market_order`) was called from `submit_market` and
//! `submit_market_close` ALONE. Every other submitting verb — `submit_limit`, `submit_stop`,
//! `submit_trailing`, `submit_limit_close` and `HftBroker::submit_limit_tagged` — went straight to
//! `push_pending` with no gate at all.
//!
//! So a backtest of a limit strategy ran with the operator's `[risk]` budget effectively switched
//! off: no per-order notional cap, no projected-exposure cap, no buying-power / `max_leverage`
//! check. LIVE gates all of them, which means the BACKTEST was the permissive side — a strategy
//! could validate here and be refused in production. `vike-mm` quotes limits exclusively, so its
//! entire backtest history sat in that gap.
//!
//! `harness_risk_wiring.rs` proves the same wiring for the MARKET path end-to-end. This is its
//! limit twin, driven directly against `SimBroker` so the assertion is about the verb rather than
//! about a profile plumbing chain that file already covers.

use vike_backtest::{EngineParams, SimBroker, StrategyEngine};
use vike_exec::RiskLimits;
use vike_model::{Bar, Strategy};

const SYM: &str = "SYM0";
const VENUE: &str = "TEST";
const PX: f64 = 100.0;

fn bars(n: usize) -> Vec<Bar> {
    (0..n)
        .map(|i| Bar {
            ts: 60_000 * (i as i64 + 1),
            open: PX,
            high: PX,
            low: PX,
            close: PX,
            volume: 1_000.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        })
        .collect()
}

/// Submits ONE order of `kind` on the first bar, then nothing.
struct SubmitOnce {
    kind: &'static str,
    qty: f64,
    done: bool,
}

impl Strategy<SimBroker> for SubmitOnce {
    fn on_bar(&mut self, b: &mut SimBroker, _bar: &Bar) {
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
    let params = EngineParams {
        cash: 1_000_000.0,
        default_venue: Some(VENUE.to_string()),
        risk_limits: cap
            .map(|c| RiskLimits { max_notional_per_order: Some(c), ..RiskLimits::new() }),
        ..Default::default()
    };
    let strat = SubmitOnce { kind, qty, done: false };
    let mut e = StrategyEngine::new(vec![(SYM.to_string(), bars(3))], strat, params);
    let _ = e.run();
    let rested = e.core.sym[0].pending.len();
    let reasons = e.core.dropped.iter().map(|(_, r, _, _)| r.clone()).collect();
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
// `StrategyEngine`, which is the only place `SimBroker::gate_order`'s fold of
// `RiskContext::account_exposure_excl_order` is reachable.
//
// Same standing rule as the rest of this file: **the backtest must never be the PERMISSIVE side.**
// A `0.0` there would vacate the account ceiling for every backtest while live refused it, so a
// strategy could validate here and be denied in production — which is exactly the gap that made
// `gate_market_order` grow into `gate_order` in the first place.

const SYM1: &str = "SYM1";

/// Buys `qty` of [`SYM1`] on step 0 and `qty` of [`SYM`] on step 2, once each. `on_bar` fires once
/// per symbol per step, hence the latches: the account state under test is "one filled position
/// plus one new order", not "however many bars there were".
struct LoadThenBuyOther {
    qty: f64,
    first: bool,
    second: bool,
}

impl Strategy<SimBroker> for LoadThenBuyOther {
    fn on_bar(&mut self, b: &mut SimBroker, _bar: &Bar) {
        if b.index == 0 && !self.first {
            self.first = true;
            b.submit(SYM1, 1, self.qty, 1.0, true, None);
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
    let params = EngineParams {
        cash: 1_000_000.0,
        default_venue: Some(VENUE.to_string()),
        risk_limits: account_cap
            .map(|c| RiskLimits { max_account_exposure: Some(c), ..RiskLimits::new() }),
        ..Default::default()
    };
    let strat = LoadThenBuyOther { qty, first: false, second: false };
    let mut e = StrategyEngine::new(
        vec![(SYM.to_string(), bars(5)), (SYM1.to_string(), bars(5))],
        strat,
        params,
    );
    let _ = e.run();
    let reasons = e.core.dropped.iter().map(|(_, r, _, _)| r.clone()).collect();
    (e.core.sym[0].pos.size, reasons)
}

/// ⚠ **THE BACKTEST FOLDS THE ACCOUNT SUM.** A position in ANOTHER symbol is counted against the
/// ceiling, so the second order is refused here exactly as the live gate refuses it.
///
/// 40 units of `SYM1` fill at 100 (4 000 of account exposure); the 40 units of `SYM` that follow
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
/// the others committed nothing, which is the hole `ExecutionEngine::live_order_margin` records on
/// the buying-power lane. Here the first order is a LIMIT far below the mark, so it rests unfilled
/// for the whole run: only a fold that counts working orders can refuse the second.
#[test]
fn the_backtest_counts_a_resting_order_against_the_account_ceiling() {
    struct RestThenBuy {
        rested: bool,
        bought: bool,
    }
    impl Strategy<SimBroker> for RestThenBuy {
        fn on_bar(&mut self, b: &mut SimBroker, _bar: &Bar) {
            if b.index == 0 && !self.rested {
                self.rested = true;
                // Far below the mark: it never crosses, so it is still WORKING when the next
                // order is judged. ⚠ A working order is valued at the MARK, not at its own limit
                // price — the same basis the live producer prices it on, and the same basis the
                // position it would become is valued on.
                b.submit_limit(SYM1, 1, 20.0, PX * 0.5, 1.0, true, None);
            }
            if b.index == 2 && !self.bought {
                self.bought = true;
                b.submit(SYM, 1, 40.0, 1.0, true, None);
            }
        }
    }
    let run = |cap: Option<f64>| {
        let params = EngineParams {
            cash: 1_000_000.0,
            default_venue: Some(VENUE.to_string()),
            risk_limits: cap
                .map(|c| RiskLimits { max_account_exposure: Some(c), ..RiskLimits::new() }),
            ..Default::default()
        };
        let mut e = StrategyEngine::new(
            vec![(SYM.to_string(), bars(5)), (SYM1.to_string(), bars(5))],
            RestThenBuy { rested: false, bought: false },
            params,
        );
        let _ = e.run();
        let reasons: Vec<String> = e.core.dropped.iter().map(|(_, r, _, _)| r.clone()).collect();
        (e.core.sym[0].pos.size, e.core.sym[1].pending.len(), reasons)
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
