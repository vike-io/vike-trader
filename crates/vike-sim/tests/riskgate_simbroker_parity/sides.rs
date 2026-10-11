//! The two sides each row drives: the LIVE `RiskGate` verdict and the BACKTEST engine run.

use std::sync::{Arc, Mutex};

use vike_exec::{RiskContext, RiskGate};
use vike_model::RiskLimits;
use vike_model::{Bar, OrderRequest, Strategy, SymbolProperties};
use vike_sim::{EngineParams, SimBroker, StrategyEngine};

use super::{BAR_MS, Row, SYM, SYM2, T0, TEST_FILL_TS, VENUE};

// ---------------------------------------------------------------------------------------------
// LIVE side
// ---------------------------------------------------------------------------------------------

pub(super) fn live_verdict(row: &Row) -> (bool, String) {
    let limits = RiskLimits {
        min_qty: vike_model::nz_step(row.min_qty),
        min_notional: vike_model::nz_step(row.min_notional),
        im_requirement: row.im,
        required_free_bp_pct: row.free_bp_pct,
        max_account_exposure: row.account_cap,
        ..RiskLimits::new()
    };
    // The margin fields exactly as the LIVE engine builds them (`gate_and_register`): the
    // open-book margin fold — one position here — and the reversing closing credit. Before
    // phase 2 this harness left both 0.0, which under-modeled the live side for any row with a
    // position; the backtest's `gate_market_order` mirrors this same construction, so the
    // matrix now compares the two engines' REAL contexts, not simplified ones.
    let (margin_used, closing_credit) = match limits.im_for(SYM) {
        Some(im) => {
            let used = row.position.abs() * row.price * row.multiplier * im;
            let credit = if row.position != 0.0 && row.side as f64 * row.position < 0.0 {
                2.0 * row.position.abs() * row.price * row.multiplier * im
            } else {
                0.0
            };
            (used, credit)
        }
        None => (0.0, 0.0),
    };
    let ctx = RiskContext {
        position_size: row.position,
        mark_price: row.price,
        equity: row.equity,
        multiplier: row.multiplier,
        margin_used,
        closing_credit,
        // The ACCOUNT-aggregate term, built the way the LIVE producer builds it
        // (`ExecutionEngine::resolved_account_exposure_excluding`): GROSS — `|size|`, never netting
        // — over every symbol but the order's, at the mark and the contract multiplier. Written
        // out here rather than taken from a helper for the same reason `margin_used` above is:
        // this side must state the arithmetic it believes in, so that when the backtest's own fold
        // disagrees the row FAILS instead of both sides quietly sharing one bug.
        //
        // The order's own symbol contributes nothing — the gate re-adds it PROJECTED — and this
        // harness has no resting orders, so the producer's second half is exercised by the
        // crate-local suites (`crates/vike-sim/tests/laws/limit_orders_are_gated.rs` and
        // `crates/vike-exec/tests/risk/risk_account_ceiling.rs`) rather than here.
        account_exposure_excl_order: vike_model::gross_notional(
            row.other_position,
            row.price,
            row.multiplier,
        ),
        ..RiskContext::default()
    };
    let req = OrderRequest {
        client_order_id: "parity".to_string(),
        venue: VENUE.to_string(),
        symbol: SYM.to_string(),
        order_type: vike_model::OrderType::Market,
        side: row.side,
        qty: row.qty,
        reduce_only: row.reduce_only,
        ..Default::default()
    };
    let v = RiskGate::new(limits).check(&req, &ctx);
    (v.ok, v.reason)
}

// ---------------------------------------------------------------------------------------------
// BACKTEST side
// ---------------------------------------------------------------------------------------------

/// bar 0: submit the SETUP order (establishes `position`). bar 1: the setup fill has landed and
/// the ORDER UNDER TEST is submitted. bar 2: the test order fills at the open. bar 3: drain.
struct SetupThenTest {
    position: f64,
    side: i32,
    qty: f64,
    /// The position ACTUALLY established when the order under test is submitted. Recorded rather
    /// than assumed: `gate_market_order` gates the SETUP order too (denies it whole by default,
    /// truncates it under the clamp knob), so a row that arms the gate can silently start from
    /// a different account state than it declares. [`check`] asserts this equals `Row::position`,
    /// which is what turns that failure mode into a loud harness error instead of a mislabelled
    /// parity result.
    observed_before: Arc<Mutex<f64>>,
    /// The signed position to establish in [`SYM2`] — the ACCOUNT lane's other symbol. `0.0` (every
    /// row but that lane's) submits nothing and the run stays single-symbol.
    other_position: f64,
    /// …and what was ACTUALLY established there, for the same reason `observed_before` exists one
    /// field up: the setup leg for the other symbol crosses the SAME armed gate, so a row whose
    /// account ceiling refused its own setup would silently reconcile a different scenario than it
    /// declares. [`check`] asserts it.
    observed_other: Arc<Mutex<f64>>,
    /// ⚠ `on_bar` fires ONCE PER SYMBOL PER STEP, so with the account lane's second series every
    /// arm below would otherwise run twice. Latched rather than keyed on `bar.symbol` because the
    /// engine re-tags each bar `SYM.VENUE` at construction, and a test that parsed that string
    /// would be asserting on the tagging convention rather than on the gate.
    fired: [bool; 2],
}

impl Strategy<SimBroker> for SetupThenTest {
    fn on_bar(&mut self, ctx: &mut SimBroker, _bar: &Bar) {
        match ctx.index {
            0 if !self.fired[0] => {
                self.fired[0] = true;
                if self.position != 0.0 {
                    let side = if self.position > 0.0 { 1 } else { -1 };
                    ctx.submit(SYM, side, self.position.abs(), 0.0, true, None);
                }
                if self.other_position != 0.0 {
                    let side = if self.other_position > 0.0 { 1 } else { -1 };
                    ctx.submit(SYM2, side, self.other_position.abs(), 0.0, true, None);
                }
            }
            1 if !self.fired[1] => {
                self.fired[1] = true;
                *self.observed_before.lock().expect("uncontended") = ctx.position_of(SYM).size;
                // ⚠ Only when the row asked for a second symbol: a single-symbol run has no
                // `SYM2` registered at all, and asking for its position would panic in `idx`.
                if self.other_position != 0.0 {
                    *self.observed_other.lock().expect("uncontended") = ctx.position_of(SYM2).size;
                }
                ctx.submit(SYM, self.side, self.qty, 0.0, true, None);
            }
            _ => {}
        }
    }
}

/// Returns `(position_before_test_order, other_symbol_position, final_position, dropped)` —
/// `dropped` is the engine's strategy-observable refusal channel (`SimBroker::dropped`), where the
/// default deny-whole gate records the live gate's own reason string (unified as of phase 2, e.g.
/// `"insufficient-margin"`). The second element is the ACCOUNT lane's other symbol, `0.0` for every
/// row that declares none; [`check`] asserts it against the row for the same harness reason the
/// first element is asserted.
#[expect(clippy::type_complexity)] // the (before, other, after, dropped-channel) tuple, test-local
pub(super) fn sim_outcome(row: &Row) -> (f64, f64, f64, Vec<(String, String, f64, f64)>) {
    let bars: Vec<Bar> = (0..4)
        .map(|i| Bar {
            ts: T0 + i as i64 * BAR_MS,
            open: row.price,
            high: row.price,
            low: row.price,
            close: row.price,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: Some(SYM.to_string()),
        })
        .collect();

    // UNCONSTRAINED until the test order's own fill bar, then the row's grid — so the setup leg
    // can establish a position the row's floors would themselves have refused (see the module
    // doc). `step_size`/`tick_size` stay 0.0 (unconstrained) throughout: this file reconciles the
    // FLOOR and MARGIN decisions, and `properties_fills.rs` already gates the snapping.
    let (min_qty, min_notional) = (row.min_qty, row.min_notional);
    #[expect(clippy::type_complexity)]
    let grid: Arc<dyn Fn(&str, &str, i64) -> Option<SymbolProperties> + Send + Sync> =
        Arc::new(move |_v, _s, ts| {
            if ts >= TEST_FILL_TS {
                Some(SymbolProperties { min_qty, min_notional, ..Default::default() })
            } else {
                Some(SymbolProperties::default())
            }
        });

    let params = EngineParams {
        cash: row.equity,
        multiplier: row.multiplier,
        leverage: row.leverage,
        clamp_to_leverage: row.clamp,
        // PHASE 2: a live-only knob flows to the backtest VERBATIM via `risk_limits` — armed
        // exactly when the row uses one (`free_bp_pct`); otherwise the `leverage -> im = 1/L`
        // convenience mapping is the config under test.
        // …and the ACCOUNT-aggregate ceiling rides the same seam, so a row that arms it arms the
        // backtest's gate too. Both conditions are ORed rather than one being folded into the
        // other because either knob alone must arm the limits (a row with a ceiling and no
        // free-BP haircut is the account lane's ordinary shape).
        risk_limits: (row.free_bp_pct != 0.0 || row.account_cap.is_some()).then(|| RiskLimits {
            im_requirement: row.im,
            required_free_bp_pct: row.free_bp_pct,
            max_account_exposure: row.account_cap,
            ..RiskLimits::new()
        }),
        default_venue: Some(VENUE.to_string()),
        properties: Some(grid),
        ..Default::default()
    };
    let observed_before = Arc::new(Mutex::new(0.0));
    let observed_other = Arc::new(Mutex::new(0.0));
    let strat = SetupThenTest {
        position: row.position,
        side: row.side,
        qty: row.qty,
        observed_before: Arc::clone(&observed_before),
        other_position: row.other_position,
        observed_other: Arc::clone(&observed_other),
        fired: [false; 2],
    };
    // ⚠ A SECOND SERIES only when the row asks for one. Every pre-existing row keeps the
    // one-symbol engine it always had — `on_bar` fires once per symbol per step, so an
    // unconditional second series would change every row's dispatch count, not just its account
    // sum.
    let series = if row.other_position == 0.0 {
        vec![(SYM.to_string(), bars)]
    } else {
        let other: Vec<Bar> =
            bars.iter().map(|b| Bar { symbol: Some(SYM2.to_string()), ..b.clone() }).collect();
        vec![(SYM.to_string(), bars), (SYM2.to_string(), other)]
    };
    let mut e = StrategyEngine::new(series, strat, params);
    let _ = e.run();
    let before = *observed_before.lock().expect("uncontended");
    let other = *observed_other.lock().expect("uncontended");
    (before, other, e.core.sym[0].pos.size, e.core.dropped.clone())
}
