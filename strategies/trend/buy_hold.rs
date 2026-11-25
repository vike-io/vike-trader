//! Buy and hold: buy `size` units of `symbol` ONCE (on the first bar or the first quote tick) and
//! hold. The simplest possible strategy, and the end-to-end smoke fixture.
//!
//! ```toml
//! [strategy]
//! runs_on = "live"
//! why = """\
//!     bars reaching a live mount are symbol-stamped — \
//!     `crates/vike-core/src/runtime/dispatch.rs`'s `BarClose` arm sets \
//!     `bar.symbol = Some(key.1)` before `drive_strategy`, so this strategy routes \
//!     to the mounted instrument rather than through the empty symbol"""
//! routes = "single_leg"
//!
//! [params.size]
//! type = "number"
//! default = 1.0
//!
//! [params.symbol]
//! type = "text"
//! route = "symbol"
//! ```

use vike_marketdata::{Bar, TickQuote};
use vike_model::params::{ParamValue, Params};
use vike_model::{OrderBroker, OrderPlacement, Strategy};

/// The strategy this file runs, built from the profile's `[strategy.params]`.
pub fn build<B: OrderBroker + 'static>(params: &Params) -> Box<dyn Strategy<B> + Send> {
    Box::new(BuyHold::from_params(params))
}

/// What each declared key RESOLVED to, in the header's order (the mount's "effective params" line).
pub fn echo(params: &Params) -> Vec<(&'static str, String)> {
    /// An OPTIONAL symbol, in the THREE states it actually has — absent, explicitly EMPTY, and set.
    ///
    /// ⚠ **Absent and empty are not the same mount.** `None` takes the symbol off the feed;
    /// `Some("")` is a STATED value this reader refuses to trade on (`BuyHold::buy` guards on
    /// `is_empty()`). It mirrors that `is_empty()` exactly (no trimming): a whitespace symbol IS
    /// routed.
    fn opt_symbol(v: &Option<String>) -> String {
        match v.as_deref() {
            None => "(from the feed)".to_string(),
            Some("") => "(empty — this strategy cannot trade)".to_string(),
            Some(s) => s.to_string(),
        }
    }
    let s = BuyHold::from_params(params);
    vec![("size", format!("{}", s.size)), ("symbol", opt_symbol(&s.symbol))]
}

/// Buy `size` units of `symbol` ONCE (on the first bar or the first quote tick) and hold.
/// PORTABLE (`impl<B: OrderBroker> Strategy<B>`), so it runs unchanged on the live stack.
pub struct BuyHold {
    /// Units to buy (raw, not notional — mirrors [`OrderBroker::place`]'s `qty`).
    pub size: f64,
    /// Explicit symbol override. `None` uses whatever symbol the first bar/tick carries (the
    /// harness's single-symbol path).
    pub symbol: Option<String>,
    bought: bool,
}

impl BuyHold {
    /// `size` defaults to `1.0`; `symbol` is optional. Unrecognized/missing keys are ignored — a
    /// params READER, not a strict schema.
    pub fn from_params(params: &Params) -> Self {
        let size = params.get("size").and_then(ParamValue::as_f64).unwrap_or(1.0);
        let symbol = params.get("symbol").and_then(ParamValue::as_str).map(str::to_string);
        BuyHold { size, symbol, bought: false }
    }

    /// Construct directly (the test/`Default`-ish path the params reader lowers into).
    pub fn new(size: f64, symbol: Option<String>) -> Self {
        BuyHold { size, symbol, bought: false }
    }

    /// The one market buy. ⚠ An EMPTY symbol submits nothing — the claim the resolved echo's
    /// "cannot trade" rendering makes, pinned by its test.
    fn buy<B: OrderBroker>(&mut self, broker: &mut B, symbol: &str) {
        if self.bought || symbol.is_empty() {
            return;
        }
        broker.place(symbol, "", 1, self.size, OrderPlacement::Market, false);
        self.bought = true;
    }
}

impl<B: OrderBroker> Strategy<B> for BuyHold {
    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        let symbol =
            self.symbol.clone().or_else(|| bar.symbol.map(String::from)).unwrap_or_default();
        self.buy(broker, &symbol);
    }

    fn on_quote_tick(&mut self, broker: &mut B, q: &TickQuote) {
        let symbol = self.symbol.clone().unwrap_or_else(|| q.symbol.to_string());
        self.buy(broker, &symbol);
    }
}

// The reader's pure surface, copied from `crates/vike-strategy/src/registry/tests/buy_hold.rs`.
// Run by `crates/vike-strategy-lab` (`cargo test -p vike-strategy-lab`); a build of this
// file compiles no test.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buy_hold_from_params_reads_size_and_symbol() {
        let params: Params = toml::from_str("size = 2.5\nsymbol = \"ETHUSDT\"\n").unwrap();
        let strat = BuyHold::from_params(&params);
        assert_eq!(strat.size, 2.5);
        assert_eq!(strat.symbol.as_deref(), Some("ETHUSDT"));
    }

    #[test]
    fn buy_hold_from_params_defaults_size_to_one_and_symbol_to_none() {
        let strat = BuyHold::from_params(&Params::new());
        assert_eq!(strat.size, 1.0);
        assert_eq!(strat.symbol, None);
    }

    #[test]
    fn buy_hold_from_params_accepts_integer_size() {
        let params: Params = toml::from_str("size = 3\n").unwrap();
        assert_eq!(BuyHold::from_params(&params).size, 3.0);
    }
}
