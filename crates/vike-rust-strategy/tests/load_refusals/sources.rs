//! The three user-strategy sources the template-built tests hand the real builder, byte for byte.
//! Each is written on the one broker interface (`build<B: vike_model::OrderBroker + 'static>`,
//! `OrderBroker::place`), as a strategy file is since `ABI_VERSION` 8.

pub(super) const PARAMS_PROBE_SOURCE: &str = r#"
use vike_model::params::{ParamValue, Params};
use vike_model::{OrderBroker, Strategy};
use vike_marketdata::Bar;

pub struct ParamsProbe {
    warmup_bars: usize,
}

impl<B: OrderBroker> Strategy<B> for ParamsProbe {
    fn warmup(&self) -> usize {
        self.warmup_bars
    }
    fn on_bar(&mut self, _broker: &mut B, _bar: &Bar) {}
}

pub fn build<B: vike_model::OrderBroker + 'static>(
    params: &Params,
) -> Box<dyn vike_model::Strategy<B> + Send> {
    let warmup_bars = params
        .get("warmup_bars")
        .and_then(ParamValue::as_integer)
        .map(|i| i.max(0) as usize)
        .unwrap_or(1);
    Box::new(ParamsProbe { warmup_bars })
}
"#;

pub(super) const PANICS_ON_THE_SECOND_BAR_SOURCE: &str = r#"
use vike_model::{OrderBroker, Strategy, OrderPlacement};
use vike_marketdata::Bar;

pub struct PanicsOnTheSecondBar {
    seen: usize,
}

impl<B: OrderBroker> Strategy<B> for PanicsOnTheSecondBar {
    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        self.seen += 1;
        if self.seen >= 2 {
            panic!("deliberate panic in a user strategy's on_bar");
        }
        let symbol = bar.symbol.clone().unwrap_or_default();
        broker.place(&symbol, "", 1, 3.5, OrderPlacement::Market, false);
    }
}

pub fn build<B: vike_model::OrderBroker + 'static>(
    _params: &vike_model::params::Params,
) -> Box<dyn vike_model::Strategy<B> + Send> {
    Box::new(PanicsOnTheSecondBar { seen: 0 })
}
"#;

/// Reads `OrderBroker::contract_value` for the bar's symbol and places it back as data: the kind as the
/// side (linear +1, inverse -1), the unit as the quantity; nothing when it reads `None`.
pub(super) const READS_THE_CONTRACT_VALUE_SOURCE: &str = r#"
use vike_model::{NotionalKind, OrderBroker, Strategy, OrderPlacement};
use vike_marketdata::Bar;

pub struct ReadsTheContractValue;

impl<B: OrderBroker> Strategy<B> for ReadsTheContractValue {
    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        let symbol = bar.symbol.clone().unwrap_or_default();
        if let Some(v) = broker.contract_value(&symbol) {
            let side = match v.kind {
                NotionalKind::Linear => 1,
                NotionalKind::Inverse => -1,
            };
            broker.place(&symbol, "", side, v.per_unit, OrderPlacement::Market, false);
        }
    }
}

pub fn build<B: vike_model::OrderBroker + 'static>(
    _params: &vike_model::params::Params,
) -> Box<dyn vike_model::Strategy<B> + Send> {
    Box::new(ReadsTheContractValue)
}
"#;
