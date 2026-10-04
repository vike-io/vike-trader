//! `fixture_live` — the fixture that pins the `strategy.toml` `live = true` path: identical to
//! `fixture_hold` except its folder manifest opts into live mounting, so the pipeline test can
//! assert `USER_LIVE_CAPABLE` carries exactly this one.

use vike_model::{Bar, Broker, Strategy};

pub struct FixtureLive {
    qty: f64,
    entered: bool,
}

impl<B: Broker> Strategy<B> for FixtureLive {
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

pub fn build<B: vike_model::HftBroker + 'static>(
    params: &toml::Value,
) -> Box<dyn vike_model::Strategy<B> + Send> {
    let qty = params
        .get("qty")
        .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
        .unwrap_or(1.0);
    Box::new(FixtureLive { qty, entered: false })
}
