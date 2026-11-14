//! Example: buy a fixed size on the first bar and hold it.
//!
//! A strategy is ONE file in the project's `strategies/` folder (subfolders are fine): copy it in
//! and run it, with no program rebuild. The `toml` block below is its header: where it may run and
//! every key it reads, each with its type. A key the header does not declare is refused when a
//! profile hands it one. This file is the smallest complete one: one key and the `build` entry.
//!
//! ```toml
//! [strategy]
//! runs_on = "backtest"
//!
//! [params.qty]
//! type = "number"
//! default = 1.0
//! ```

use vike_marketdata::Bar;
use vike_model::params::{ParamValue, Params};
use vike_model::{OrderBroker, OrderPlacement, Strategy};

/// Buys `qty` at market on its first bar, then holds.
pub struct Example {
    qty: f64,
    entered: bool,
}

impl<B: OrderBroker> Strategy<B> for Example {
    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        if self.entered {
            return;
        }
        let Some(symbol) = bar.symbol.as_deref() else {
            return;
        };
        broker.place(symbol, "", 1, self.qty, OrderPlacement::Market, false);
        self.entered = true;
    }
}

/// The entry: the host calls it with the profile's `[strategy.params]`.
pub fn build<B: OrderBroker + 'static>(params: &Params) -> Box<dyn Strategy<B> + Send> {
    let qty = params.get("qty").and_then(ParamValue::as_f64).unwrap_or(1.0);
    Box::new(Example { qty, entered: false })
}
