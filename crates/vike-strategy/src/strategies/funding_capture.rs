//! A directional perp funding-HARVEST reference strategy ([`FundingCapture`]), written as a
//! portable `impl<B: Broker> Strategy<B>`, so the SAME type runs in the backtest sweep harness and
//! live. It reads the funding rate off `Bar::funding` (joined onto replayed bars by
//! `engine.attach_funding`). A behavioural reference with no Python twin, so NOT parity-gated.
//!
//! ## What it does
//!
//! The engine's accrual (`broker_sim::funding_charge` = `pos_size * mark * funding_rate *
//! multiplier`, then `cash -= charge`) encodes the venue convention: a LONG PAYS positive funding
//! and a SHORT RECEIVES it. The collecting side is therefore OPPOSITE the funding sign: POSITIVE ⇒
//! SHORT, NEGATIVE ⇒ LONG.
//!
//! On each bar whose funding magnitude exceeds `threshold` it targets the collecting side at `qty`
//! with a market delta; a sub-threshold funding bar flattens. Bars WITHOUT funding HOLD unchanged,
//! deliberately: the engine charges funding against the position established BEFORE that bar's
//! strategy call, so the position must still rest at the NEXT funding timestamp to collect.
//!
//! Only the portable [`Broker`] surface (`submit_market` + `position`, no `cancel`): it steers a
//! NET position with market deltas, never resting limits.

use toml::Value;

use vike_model::{Bar, Broker, Strategy};

use crate::controller::as_f64;

/// A directional funding-harvest strategy. See the module doc for the contract; params via
/// [`FundingCapture::from_params`].
#[derive(Debug, Clone)]
pub struct FundingCapture {
    /// Minimum ABSOLUTE funding rate to hold a position for. A funding bar with `|funding| <=
    /// threshold` flattens; the default `0.0` acts on any non-zero funding.
    pub threshold: f64,
    /// Position size (raw units, not notional — mirrors [`Broker::submit_market`]'s `qty`) held on
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
    pub fn from_params(params: &Value) -> Self {
        let threshold = params.get("threshold").and_then(as_f64).unwrap_or(0.0);
        let qty = params.get("qty").and_then(as_f64).unwrap_or(1.0);
        let symbol = params.get("symbol").and_then(Value::as_str).map(str::to_string);
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

impl<B: Broker> Strategy<B> for FundingCapture {
    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        // Only funding bars are decision points; between funding timestamps HOLD unchanged so the
        // position is still resting when the next funding charge lands.
        let Some(funding) = bar.funding else {
            return;
        };
        let symbol = self.symbol.clone().or_else(|| bar.symbol.clone()).unwrap_or_default();
        if symbol.is_empty() {
            return; // SimBroker panics on an empty symbol — never route through "".
        }
        let target = self.target(funding);
        let delta = target - broker.position(&symbol);
        if delta != 0.0 {
            let side = if delta > 0.0 { 1 } else { -1 };
            broker.submit_market(&symbol, side, delta.abs());
        }
    }
}

// The END-TO-END funding tests need the real `StrategyEngine` (above this crate), so they run in
// `tests/funding_capture_engine.rs` over the upward DEV edge. Below: the reader and `target`.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_params_reads_threshold_qty_and_symbol() {
        let params: Value =
            toml::from_str("threshold = 0.0005\nqty = 3.0\nsymbol = \"ETHUSDT\"\n").unwrap();
        let s = FundingCapture::from_params(&params);
        assert_eq!(s.threshold, 0.0005);
        assert_eq!(s.qty, 3.0);
        assert_eq!(s.symbol.as_deref(), Some("ETHUSDT"));
    }

    #[test]
    fn from_params_defaults_when_absent() {
        let s = FundingCapture::from_params(&Value::Table(Default::default()));
        assert_eq!(s.threshold, 0.0);
        assert_eq!(s.qty, 1.0);
        assert_eq!(s.symbol, None);
    }

    #[test]
    fn from_params_accepts_integer_qty() {
        let params: Value = toml::from_str("qty = 2\n").unwrap();
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
