//! The labs' bars: the venue's own candles, read through the datahub the way a backtest's one-shot readers read history
//! (`docs/decisions/0084-only-the-datahub-touches-the-store.md`: only the datahub touches the store, and nothing here opens
//! one of its files). Set B shares this file by path.
//!
//! The route is `vike_datahub_client::route`'s. The address is the project's `config.datahub_addr` row, else the loopback
//! default. The key is the datahub's OBSERVE key alone, read from the project's hub-key store
//! (`vike_secrets::open_credential_store`: `VIKE_SETTINGS_DIR`, else the project walk from the working directory). It is
//! never on argv and never printed. On a lane the labs run with `VIKE_SETTINGS_DIR` naming the CI box's project.
//!
//! Every series is Binance's, under the store's spelling: `BTCUSDT` is the spot pair, `ETHUSDT.P` the USDⓈ-M perpetual.

use std::collections::HashMap;
use std::sync::OnceLock;

use vike_data::{HistoryStore, TimestampRange};
use vike_datahub_client::route;
use vike_marketdata::Bar;

/// The datahub, opened once per process. Nothing dials it until the first read.
static DATAHUB: OnceLock<Box<dyn HistoryStore + Send + Sync>> = OnceLock::new();

fn open() -> Box<dyn HistoryStore + Send + Sync> {
    let env: HashMap<String, String> = std::env::vars().collect();
    let route = route::history_route(route::datahub_address_for_bin(&env).as_deref());
    let store = vike_secrets::open_credential_store(&env).unwrap_or_else(|e| panic!("{e}"));
    // The observe key alone: the labs only read.
    let observe = |name: &str| name == vike_hub_protocol::auth::DATAHUB_OBSERVE_KEY_ENV;
    let keys = route::routed_hub_keys(store.hub_keys(observe), route.hub());
    route::open_routed_history(&route, keys)
}

/// The datahub, dialled on the first read: set B reads its USDT-USD rates' lane through it too.
pub fn datahub() -> &'static (dyn HistoryStore + Send + Sync) {
    DATAHUB.get_or_init(open).as_ref()
}

/// Binance's `interval` bars of `symbol`, open times from `from_ms` (`None`: the series' first bar) to `to_ms`, both inclusive.
pub fn read(symbol: &str, interval: &str, from_ms: Option<i64>, to_ms: i64) -> Vec<Bar> {
    let range = TimestampRange { start: from_ms, end: Some(to_ms) };
    let bars = datahub()
        .load_bars("binance", symbol, interval, range)
        .unwrap_or_else(|e| panic!("binance {symbol} {interval} through the datahub: {e}"));
    assert!(!bars.is_empty(), "the datahub holds no binance {symbol} {interval} bar in {range:?}");
    bars
}
