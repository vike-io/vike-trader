//! The two user-strategy sources the template-built tests hand the real builder, byte for byte.

pub(super) const PARAMS_PROBE_SOURCE: &str = r#"
use vike_model::{Bar, Broker, Strategy};

pub struct ParamsProbe {
    warmup_bars: usize,
}

impl<B: Broker> Strategy<B> for ParamsProbe {
    fn warmup(&self) -> usize {
        self.warmup_bars
    }
    fn on_bar(&mut self, _broker: &mut B, _bar: &Bar) {}
}

pub fn build<B: vike_model::HftBroker + 'static>(
    params: &toml::Value,
) -> Box<dyn vike_model::Strategy<B> + Send> {
    let warmup_bars = params
        .get("warmup_bars")
        .and_then(toml::Value::as_integer)
        .map(|i| i.max(0) as usize)
        .unwrap_or(1);
    Box::new(ParamsProbe { warmup_bars })
}
"#;

pub(super) const PANICS_ON_THE_SECOND_BAR_SOURCE: &str = r#"
use vike_model::{Bar, Broker, Strategy};

pub struct PanicsOnTheSecondBar {
    seen: usize,
}

impl<B: Broker> Strategy<B> for PanicsOnTheSecondBar {
    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        self.seen += 1;
        if self.seen >= 2 {
            panic!("deliberate panic in a user strategy's on_bar");
        }
        let symbol = bar.symbol.clone().unwrap_or_default();
        broker.submit_market(&symbol, 1, 3.5);
    }
}

pub fn build<B: vike_model::HftBroker + 'static>(
    _params: &toml::Value,
) -> Box<dyn vike_model::Strategy<B> + Send> {
    Box::new(PanicsOnTheSecondBar { seen: 0 })
}
"#;
