//! The labs' bars: the venue's own candles, read through the datahub the way a backtest's one-shot readers read history
//! (`docs/decisions/0084-only-the-datahub-touches-the-store.md`: only the datahub touches the store, and nothing here opens
//! one of its files). Set B shares this file by path.
//!
//! The route is `vike_datahub_client::route`'s. The address is the project's `config.datahub_addr` row, else the loopback
//! default. The key is the project box's own DEVICE key (decision 0288 item 8: no shared datahub pair is read), from the
//! settings database `vike_secrets::open_credential_store` locates (`VIKE_SETTINGS_DIR`, else the project walk from the
//! working directory). It is never on argv and never printed. On a lane the labs run with `VIKE_SETTINGS_DIR` naming
//! the CI box's project.
//!
//! Every series is under the store's spelling: on Binance `BTCUSDT` is the spot pair, `ETHUSDT.P` the USDⓈ-M perpetual. Set A's
//! are Binance's; set B's are each row's venue's (`BYBIT:WIFUSDT.P` is Bybit's linear perpetual).
//!
//! A series the operator entered a share split of (`vike-cli data splits`, decision 0289) is read split-adjusted, by the
//! backtest's one rule (`vike_backtest::run::KeptRows::adjust_for_splits`) over the splits the datahub serves, asked
//! once: every bar a lab holds, its own reads and the runs' alike, is then the adjusted series, and each split applied
//! is said once on stderr. A series with no split is read exactly as before.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use vike_backtest::run::KeptRows;
use vike_data::{HistoryStore, TimestampRange};
use vike_datahub_client::route;
use vike_marketdata::Bar;

/// The datahub, opened once per process. Nothing dials it until the first read.
static DATAHUB: OnceLock<Box<dyn HistoryStore + Send + Sync>> = OnceLock::new();

/// The operator's share splits the datahub serves, asked at the first read.
static SPLITS: OnceLock<KeptRows> = OnceLock::new();

/// The split lines already said on stderr.
static SAID: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn splits() -> &'static KeptRows {
    SPLITS.get_or_init(|| match datahub().stock_splits() {
        Ok(splits) => KeptRows { splits: splits.unwrap_or_default(), ..KeptRows::default() },
        Err(e) => {
            eprintln!("the operator's share splits could not be read through the datahub ({e}): every series is read unadjusted");
            KeptRows::default()
        }
    })
}

fn open() -> Box<dyn HistoryStore + Send + Sync> {
    let env: HashMap<String, String> = std::env::vars().collect();
    let route = route::history_route(route::datahub_address_for_bin(&env).as_deref());
    let store = vike_secrets::open_credential_store(&env).unwrap_or_else(|e| panic!("{e}"));
    let device = match store.location() {
        Some(location) => vike_secrets::access::device_key_load(location.settings_dir()),
        None => Ok(None),
    };
    let keys = route::routed_hub_keys(device, route.hub());
    route::open_routed_history(&route, keys)
}

/// The datahub, dialled on the first read: set B reads its USDT-USD rates' lane through it too.
pub fn datahub() -> &'static (dyn HistoryStore + Send + Sync) {
    DATAHUB.get_or_init(open).as_ref()
}

/// `venue`'s `interval` bars of `symbol`, open times from `from_ms` (`None`: the series' first bar) to `to_ms`, both inclusive.
pub fn read(
    venue: &str,
    symbol: &str,
    interval: &str,
    from_ms: Option<i64>,
    to_ms: i64,
) -> Vec<Bar> {
    let range = TimestampRange { start: from_ms, end: Some(to_ms) };
    let mut bars = datahub()
        .load_bars(venue, symbol, interval, range)
        .unwrap_or_else(|e| panic!("{venue} {symbol} {interval} through the datahub: {e}"));
    assert!(!bars.is_empty(), "the datahub holds no {venue} {symbol} {interval} bar in {range:?}");
    let lines = splits()
        .adjust_for_splits(venue, symbol, &mut bars)
        .unwrap_or_else(|e| panic!("{venue} {symbol} {interval}: {}", e.message()));
    let mut said = vike_utility::mutex::lock(&SAID);
    for line in lines {
        if !said.contains(&line) {
            eprintln!("{line}");
            said.push(line);
        }
    }
    bars
}
