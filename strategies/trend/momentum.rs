//! Momentum: a price-momentum threshold opening triple-barrier positions. Each invitation (only
//! while the pair is flat AND past cooldown) compares the mark to the mark at its PREVIOUS
//! invitation: a move `>= threshold` opens LONG, `<= -threshold` SHORT, otherwise it declines. The
//! first-ever invitation only records the mark. Every opened position is guarded by the triple
//! barrier `tp`/`sl`/`time_limit_ms`/`trailing` (market entry, emulated exits).
//!
//! A [`Controller`] (the WHAT) run by the library's [`ControllerHarness`] (the HOW: one
//! [`vike_strategy::PositionExecutor`] per `(venue, symbol)`, the post-exit cooldown).
//!
//! ```toml
//! [strategy]
//! runs_on = "live"
//! routes = "single_leg"
//!
//! [params.qty]
//! type = "number"
//! default = 1.0
//!
//! [params.threshold]
//! type = "number"
//! default = 0.0
//!
//! [params.tp]
//! type = "number"
//!
//! [params.sl]
//! type = "number"
//!
//! [params.time_limit_ms]
//! type = "integer"
//!
//! [params.trailing]
//! type = "number"
//!
//! [params.venue]
//! type = "text"
//! default = "sim"
//! route = "venue"
//!
//! [params.cooldown_ms]
//! type = "integer"
//! default = 0
//!
//! [params.venues]
//! type = "table"
//! route = "venue_map"
//! ```

use vike_model::params::{ParamValue, Params};
use vike_model::{OrderBroker, PositionControllerParams, Strategy, TripleBarrier};
use vike_strategy::{Controller, ControllerHarness, PositionExecutorIntent};

/// The strategy this file runs, built from the profile's `[strategy.params]`: the controller's own
/// knobs read by [`MomentumController::from_params`], the harness-level ones by
/// [`controller_harness`].
pub fn build<B: OrderBroker + 'static>(params: &Params) -> Box<dyn Strategy<B> + Send> {
    Box::new(controller_harness(MomentumController::from_params(params), params))
}

/// What each declared key RESOLVED to, in the header's order (the mount's "effective params" line).
/// An absent barrier leg reads `(unarmed)`, and the harness's `venue` fallback `sim` — a tag no
/// venue answers to — is reported for exactly that reason.
pub fn echo(params: &Params) -> Vec<(&'static str, String)> {
    fn number(v: f64) -> String {
        format!("{v}")
    }
    fn opt_number(v: Option<f64>) -> String {
        v.map(number).unwrap_or_else(|| "(unarmed)".to_string())
    }
    fn opt_ms(v: Option<i64>) -> String {
        v.map(|i| i.to_string()).unwrap_or_else(|| "(unarmed)".to_string())
    }
    fn venues(map: &[(String, String)]) -> String {
        if map.is_empty() {
            return "(none)".to_string();
        }
        map.iter().map(|(s, v)| format!("{s}:{v}")).collect::<Vec<_>>().join(",")
    }
    let c = MomentumController::from_params(params);
    let b = c.barriers;
    vec![
        ("qty", number(c.qty)),
        ("threshold", number(c.threshold)),
        ("tp", opt_number(b.take_profit)),
        ("sl", opt_number(b.stop_loss)),
        ("time_limit_ms", opt_ms(b.time_limit_ms)),
        ("trailing", opt_number(b.trailing)),
        ("venue", harness_venue(params).to_string()),
        ("cooldown_ms", harness_cooldown_ms(params).to_string()),
        ("venues", venues(&harness_venue_map(params))),
    ]
}

/// Read a [`TripleBarrier`] from a params table — every leg OPTIONAL (absent ⇒ un-armed):
/// `tp`/`sl`/`trailing` are absolute price offsets from the entry fill, `time_limit_ms` the max
/// holding duration.
fn barriers_from_params(params: &Params) -> TripleBarrier {
    TripleBarrier::new(
        params.get("tp").and_then(ParamValue::as_f64),
        params.get("sl").and_then(ParamValue::as_f64),
        params.get("time_limit_ms").and_then(ParamValue::as_integer),
        params.get("trailing").and_then(ParamValue::as_f64),
    )
}

/// A price-momentum threshold [`Controller`]. Each invitation (only while the pair is flat AND past
/// cooldown) compares the mark ([`OrderBroker::price`]) to the mark at its PREVIOUS invitation: a move
/// `>= threshold` opens LONG, `<= -threshold` SHORT, otherwise it declines. The first-ever
/// invitation only records the mark. Deterministic and read-only, so parity-clean on the bar and
/// tick paths.
#[derive(Debug, Clone)]
pub struct MomentumController {
    /// Position size (units) each opened intent carries.
    pub qty: f64,
    /// Absolute price move (since the last invitation) that arms a long (`>=`) / short (`<= -`).
    /// `0.0` ⇒ always open from the second invitation on (up/flat → long, down → short).
    pub threshold: f64,
    /// The triple barrier every opened position is guarded by (market entry, emulated exits).
    pub barriers: TripleBarrier,
    /// The mark observed at the previous invitation (`None` before the first) — the momentum origin.
    last_ref: Option<f64>,
}

impl MomentumController {
    /// A momentum controller sizing every entry at `qty`, arming on a `threshold` move, guarding each
    /// position with `barriers`. Starts with no reference (declines its first invitation).
    pub fn new(qty: f64, threshold: f64, barriers: TripleBarrier) -> Self {
        MomentumController { qty, threshold, barriers, last_ref: None }
    }

    /// Read a params table (unknown keys ignored, missing keys default): `qty` (default `1.0`),
    /// `threshold` (default `0.0`) and the barrier legs (see [`barriers_from_params`]). The
    /// harness-level `venue`/`cooldown_ms` are read by [`controller_harness`].
    pub fn from_params(params: &Params) -> Self {
        let qty = params.get("qty").and_then(ParamValue::as_f64).unwrap_or(1.0);
        let threshold = params.get("threshold").and_then(ParamValue::as_f64).unwrap_or(0.0);
        MomentumController::new(qty, threshold, barriers_from_params(params))
    }
}

impl Controller for MomentumController {
    fn evaluate<B: OrderBroker>(
        &mut self,
        broker: &B,
        venue: &str,
        symbol: &str,
    ) -> Option<PositionExecutorIntent> {
        let px = broker.price(symbol);
        let side = match self.last_ref {
            Some(previous) if px - previous >= self.threshold => 1,
            Some(previous) if px - previous <= -self.threshold => -1,
            _ => 0, // no reference yet, or the move is inside the threshold band
        };
        self.last_ref = Some(px);
        if side == 0 {
            return None;
        }
        Some(PositionExecutorIntent::market(venue, symbol, side, self.qty, self.barriers))
    }

    /// Hot-swap `qty`, `threshold` and `barriers` (`cooldown_ms` is the harness's). `last_ref` is
    /// DELIBERATELY kept: it is evolving STATE, not a tunable, so the new `threshold` applies on the
    /// very next [`evaluate`](Controller::evaluate) without a spurious first-invitation reset.
    fn apply_params(&mut self, params: &PositionControllerParams) {
        self.qty = params.qty;
        self.threshold = params.threshold;
        self.barriers = params.barriers;
    }
}

/// Wrap a [`Controller`] in its portable [`ControllerHarness`], reading the harness-level knobs
/// (NOT controller knobs) from the same params table: `venue` (default `"sim"`), `cooldown_ms`
/// (post-exit re-open spacing, default `0`) and the `venues` map. The controller's OWN knobs were
/// read by its `from_params`.
fn controller_harness<C: Controller>(controller: C, params: &Params) -> ControllerHarness<C> {
    ControllerHarness::new(controller, harness_venue(params), harness_cooldown_ms(params))
        .with_venue_map(harness_venue_map(params))
}

/// The harness's default venue tag. ⚠ The fallback is the literal `"sim"`, which on a LIVE mount is
/// a tag no venue answers to — [`echo`] reports it for exactly that reason.
///
/// Split out of [`controller_harness`] so the mount and the echo read it ONCE, in one place: two
/// copies of a default is how an echo starts lying about a mount.
fn harness_venue(params: &Params) -> &str {
    params.get("venue").and_then(ParamValue::as_str).unwrap_or("sim")
}

/// Post-exit re-open spacing, default `0`. Same one-read rule as [`harness_venue`].
fn harness_cooldown_ms(params: &Params) -> i64 {
    params.get("cooldown_ms").and_then(ParamValue::as_integer).unwrap_or(0)
}

/// Optional per-symbol venue routing: a `[strategy.params.venues]` table (`SYMBOL = "venue"`).
/// Absent ⇒ every symbol routes to the default `venue`.
///
/// ⚠ A row whose VALUE is not a string is dropped silently by the `filter_map` — a per-ROW coercion
/// no key-level type check reaches, which is why the echo prints the surviving map rather than the
/// table that was typed.
fn harness_venue_map(params: &Params) -> Vec<(String, String)> {
    params
        .get("venues")
        .and_then(ParamValue::as_table)
        .map(|t| {
            t.iter()
                .filter_map(|(symbol, v)| v.as_str().map(|ven| (symbol.clone(), ven.to_string())))
                .collect()
        })
        .unwrap_or_default()
}
