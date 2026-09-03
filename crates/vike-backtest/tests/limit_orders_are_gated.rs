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
