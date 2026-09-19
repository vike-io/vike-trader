//! `fixture_hold` — the committed fixture strategy the pipeline test drives.
//!
//! This file is NOT an example for humans (that is the tier README + `my_experiment` template) —
//! it exists so CI proves the scan→generate→compile→construct pipeline on every run, in checkouts
//! that have no real `user_data/`. It follows the entry-file contract exactly: a folder-named
//! file exporting `build`.

use vike_model::{Bar, Broker, Strategy};

/// Buy `qty` once on the first bar and hold. Trivial by design: the test asserts the ORDER, not
/// the idea.
pub struct FixtureHold {
    qty: f64,
    entered: bool,
}

impl<B: Broker> Strategy<B> for FixtureHold {
    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        if self.entered {
            return;
        }
        let symbol = bar.symbol.clone().unwrap_or_default();
        if symbol.is_empty() {
            return;
        }
        broker.submit_market(&symbol, 1, self.qty);
        self.entered = true;
    }
}

/// The entry-file contract: lenient param reading, same idiom as the built-in `from_params` arms.
pub fn build<B: vike_model::HftBroker + 'static>(
    params: &toml::Value,
) -> Box<dyn vike_model::Strategy<B> + Send> {
    let qty = params
        .get("qty")
        .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
        .unwrap_or(1.0);
    Box::new(FixtureHold { qty, entered: false })
}
