//! A directional perp funding-HARVEST reference strategy ([`FundingCapture`]), written as a
//! portable `impl<B: OrderBroker> Strategy<B>`, so the SAME type runs in the backtest sweep
//! harness and live. It reads the funding rate off `Bar::funding` (joined onto replayed bars by
//! `engine.attach_funding`). A behavioural reference with no Python twin, so NOT parity-gated.
//!
//! ```toml
//! [strategy]
//! runs_on = "backtest"
//! why = """\
//!     reads `Bar::funding`, which every live kline lane leaves `None` \
//!     (`vike_bridge_core::klines::kline_to_bar`) — it would mount and never see a \
//!     funding rate"""
//! routes = "single_leg"
//!
//! [params.threshold]
//! type = "number"
//! default = 0.0
//!
//! [params.qty]
//! type = "number"
//! default = 1.0
//!
//! [params.symbol]
//! type = "text"
//! route = "symbol"
//! ```
//!
//! ## What it does
//!
//! The engine's accrual (`ContractValue::funding` = `pos_size * mark * funding_rate *
//! multiplier`, then `cash -= charge`) encodes the venue convention: a LONG PAYS positive funding
//! and a SHORT RECEIVES it. The collecting side is therefore OPPOSITE the funding sign: POSITIVE ⇒
//! SHORT, NEGATIVE ⇒ LONG.
//!
//! On each bar whose funding magnitude exceeds `threshold` it targets the collecting side at `qty`
//! with a market delta; a sub-threshold funding bar flattens. Bars WITHOUT funding HOLD unchanged,
//! deliberately: the engine charges funding against the position established BEFORE that bar's
//! strategy call, so the position must still rest at the NEXT funding timestamp to collect.
//!
//! Only an untagged market [`OrderBroker::place`] and `position`, no `cancel`: it steers a NET
//! position with market deltas, never resting limits.

use vike_model::params::{ParamValue, Params};

use vike_marketdata::Bar;
use vike_model::{OrderBroker, OrderPlacement, Strategy};

/// The strategy this file runs, built from the profile's `[strategy.params]`.
pub fn build<B: OrderBroker + 'static>(params: &Params) -> Box<dyn Strategy<B> + Send> {
    Box::new(FundingCapture::from_params(params))
}

/// What each declared key RESOLVED to, in the header's order (the mount's "effective params" line).
pub fn echo(params: &Params) -> Vec<(&'static str, String)> {
    fn number(v: f64) -> String {
        format!("{v}")
    }
    /// An OPTIONAL symbol, in the THREE states it actually has — absent, explicitly EMPTY, and set.
    /// `Some("")` is a STATED value `on_bar` refuses to trade on (it guards on `is_empty()`).
    fn opt_symbol(v: &Option<String>) -> String {
        match v.as_deref() {
            None => "(from the feed)".to_string(),
            Some("") => "(empty — this strategy cannot trade)".to_string(),
            Some(s) => s.to_string(),
        }
    }
    let f = FundingCapture::from_params(params);
    vec![
        ("threshold", number(f.threshold)),
        ("qty", number(f.qty)),
        ("symbol", opt_symbol(&f.symbol)),
    ]
}

/// A directional funding-harvest strategy. See the module doc for the contract; params via
/// [`FundingCapture::from_params`].
#[derive(Debug, Clone)]
pub struct FundingCapture {
    /// Minimum ABSOLUTE funding rate to hold a position for. A funding bar with `|funding| <=
    /// threshold` flattens; the default `0.0` acts on any non-zero funding.
    pub threshold: f64,
    /// Position size (raw units, not notional — mirrors [`OrderBroker::place`]'s `qty`) held on
    /// the collecting side. Default `1.0`.
    pub qty: f64,
    /// Explicit symbol override. `None` uses whatever symbol the first bar carries (the harness's
    /// single-symbol path).
    pub symbol: Option<String>,
}

impl Default for FundingCapture {
    fn default() -> Self {
        FundingCapture { threshold: 0.0, qty: 1.0, symbol: None }
    }
}

impl FundingCapture {
    /// Read the knobs (`BuyHold` reader convention): `threshold` (default `0.0`), `qty` (default
    /// `1.0`), `symbol` (optional).
    pub fn from_params(params: &Params) -> Self {
        let threshold = params.get("threshold").and_then(ParamValue::as_f64).unwrap_or(0.0);
        let qty = params.get("qty").and_then(ParamValue::as_f64).unwrap_or(1.0);
        let symbol = params.get("symbol").and_then(ParamValue::as_str).map(str::to_string);
        FundingCapture { threshold, qty, symbol }
    }

    /// The signed target position for a given funding rate: OPPOSITE the funding sign (collect), or
    /// flat when the rate's magnitude is at/below `threshold`.
    fn target(&self, funding: f64) -> f64 {
        if funding.abs() <= self.threshold {
            0.0
        } else if funding > 0.0 {
            -self.qty // funding positive ⇒ SHORT collects
        } else {
            self.qty // funding negative ⇒ LONG collects
        }
    }
}

impl<B: OrderBroker> Strategy<B> for FundingCapture {
    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        // Only funding bars are decision points; between funding timestamps HOLD unchanged so the
        // position is still resting when the next funding charge lands.
        let Some(funding) = bar.funding else {
            return;
        };
        let symbol =
            self.symbol.clone().or_else(|| bar.symbol.map(String::from)).unwrap_or_default();
        if symbol.is_empty() {
            return; // OrderBrokerSimulator panics on an empty symbol — never route through "".
        }
        let target = self.target(funding);
        let delta = target - broker.position(&symbol);
        if delta != 0.0 {
            let side = if delta > 0.0 { 1 } else { -1 };
            broker.place(&symbol, "", side, delta.abs(), OrderPlacement::Market, false);
        }
    }
}

// Copied from `crates/vike-strategy/src/strategies/funding_capture.rs`'s tests: the reader and
// `target`. Run by `crates/vike-strategy-lab` (`cargo test -p vike-strategy-lab`); a
// build of this file compiles no test. The END-TO-END funding tests need the real engine:
// `crates/vike-strategy-lab/tests/takers.rs`.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_params_reads_threshold_quantity_and_symbol() {
        let params: Params =
            toml::from_str("threshold = 0.0005\nqty = 3.0\nsymbol = \"ETHUSDT\"\n").unwrap();
        let s = FundingCapture::from_params(&params);
        assert_eq!(s.threshold, 0.0005);
        assert_eq!(s.qty, 3.0);
        assert_eq!(s.symbol.as_deref(), Some("ETHUSDT"));
    }

    #[test]
    fn from_params_defaults_when_absent() {
        let s = FundingCapture::from_params(&Params::new());
        assert_eq!(s.threshold, 0.0);
        assert_eq!(s.qty, 1.0);
        assert_eq!(s.symbol, None);
    }

    #[test]
    fn from_params_accepts_integer_quantity() {
        let params: Params = toml::from_str("qty = 2\n").unwrap();
        assert_eq!(FundingCapture::from_params(&params).qty, 2.0);
    }

    #[test]
    fn target_takes_the_collecting_side_and_respects_threshold() {
        let s = FundingCapture { threshold: 0.001, qty: 2.0, symbol: None };
        // funding POSITIVE above threshold ⇒ SHORT collects.
        assert_eq!(s.target(0.01), -2.0);
        // funding NEGATIVE above threshold ⇒ LONG collects.
        assert_eq!(s.target(-0.01), 2.0);
        // magnitude at/below threshold ⇒ flat.
        assert_eq!(s.target(0.001), 0.0);
        assert_eq!(s.target(0.0), 0.0);
    }
}
