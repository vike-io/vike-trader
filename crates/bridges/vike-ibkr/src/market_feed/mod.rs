//! IBKR realtime market data (Phase 3a): `IbkrFeeds` implements `vike_data::DataClient` on its OWN
//! ibapi connection (a distinct client_id), streaming quotes/trades/bars/depth onto a
//! `vike_data::LiveDataSink`. Mirrors the crypto `Feeds` pattern: one joinable pump thread per
//! subscription, draining an ibapi `Subscription<T>` and mapping each item through the pure
//! `map` module. Delayed-by-default (`switch_market_data_type` at connect).

// `pub`, not `pub(crate)`: the pure mappers are exercised from an external integration test
// (`tests/ibkr_mktdata_map.rs`, the no-network CI gate) which compiles as its own crate and
// cannot see `pub(crate)` items.
mod agg;
pub mod map;
mod pump;

/// Canonical venue string.
pub(crate) const VENUE: &str = "ibkr";

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use ibapi::client::blocking::Client;
use vike_data::{DataClient, LiveDataError, LiveDataSink, SubscriptionId};

use crate::config::IbkrConfig;
use crate::error::IbkrError;

/// Per-subscription context handed to a pump thread. `client` is a shared handle to the market-data
/// ibapi connection; `stop` is polled on a bounded `next_timeout` so the thread is joinable.
pub(crate) struct FeedCtx {
    pub(crate) sink: Arc<dyn LiveDataSink>,
    pub(crate) stop: Arc<AtomicBool>,
    pub(crate) client: Arc<Client>,
}

/// IBKR realtime market-data feed. Owns its own ibapi connection (distinct client_id) and one
/// joinable pump thread per subscription.
pub struct IbkrFeeds {
    sink: Arc<dyn LiveDataSink>,
    client: Arc<Client>,
    next_id: u64,
    subs: HashMap<SubscriptionId, (Arc<AtomicBool>, JoinHandle<()>)>,
}

impl IbkrFeeds {
    /// Open the market-data connection (own client_id) and set the market-data type (delayed by
    /// default). Degrades to `IbkrError::Connect` (→ caller keeps the venue data-less) on failure.
    pub fn connect(cfg: &IbkrConfig, sink: Arc<dyn LiveDataSink>) -> Result<IbkrFeeds, IbkrError> {
        let addr = format!("{}:{}", cfg.host, cfg.port);
        let client = Client::connect(&addr, cfg.data_client_id).map_err(|e| {
            IbkrError::Connect(format!("mktdata {addr} (client_id={}): {e}", cfg.data_client_id))
        })?;
        // Delayed-by-default so it works with no paid subscriptions; realtime when entitled.
        if let Err(e) = client.switch_market_data_type(cfg.mktdata_type.to_ibapi()) {
            tracing::warn!(error = %e, "ibkr switch_market_data_type failed; continuing");
        }
        Ok(IbkrFeeds { sink, client: Arc::new(client), next_id: 0, subs: HashMap::new() })
    }

    /// Allocate an id + stop flag, spawn the pump `body`, and track it for join. `body` receives the
    /// resolved canonical symbol, the interval string ("" for non-bar verbs), and the `FeedCtx`.
    fn spawn_with(
        &mut self,
        symbol: &str,
        interval: &str,
        body: impl FnOnce(String, String, FeedCtx) + Send + 'static,
    ) -> Result<SubscriptionId, LiveDataError> {
        let stop = Arc::new(AtomicBool::new(false));
        let ctx = FeedCtx {
            sink: Arc::clone(&self.sink),
            stop: Arc::clone(&stop),
            client: Arc::clone(&self.client),
        };
        let (symbol, interval) = (symbol.to_string(), interval.to_string());
        let h = std::thread::Builder::new()
            .name(format!("ibkr-feed-{symbol}"))
            .spawn(move || body(symbol, interval, ctx))
            .map_err(|e| LiveDataError::Subscribe(format!("spawn ibkr feed: {e}")))?;
        let id = SubscriptionId(self.next_id);
        self.next_id += 1;
        self.subs.insert(id, (stop, h));
        Ok(id)
    }
}

impl DataClient for IbkrFeeds {
    fn subscribe_bars(
        &mut self,
        symbol: &str,
        interval: &str,
    ) -> Result<SubscriptionId, LiveDataError> {
        self.spawn_with(symbol, interval, pump::bars_pump)
    }
    fn subscribe_quotes(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.spawn_with(symbol, "", pump::quotes_pump)
    }
    fn subscribe_trades(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.spawn_with(symbol, "", pump::trades_pump)
    }
    fn subscribe_book(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.spawn_with(symbol, "", pump::depth_pump) // IBKR depth → folded L2Book book lane
    }
    fn subscribe_depth(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.spawn_with(symbol, "", pump::depth_pump) // → conflating l2_snapshot (DOM)
    }
    fn unsubscribe(&mut self, id: SubscriptionId) {
        if let Some((stop, h)) = self.subs.remove(&id) {
            stop.store(true, Ordering::Relaxed);
            let _ = h.join();
        }
    }
    fn shutdown(&mut self) {
        for (stop, _) in self.subs.values() {
            stop.store(true, Ordering::Relaxed);
        }
        // Close the ibapi channels so any pump parked in next()/next_timeout returns and joins.
        self.client.disconnect();
        for (_, (_, h)) in self.subs.drain() {
            let _ = h.join();
        }
    }
}

impl Drop for IbkrFeeds {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Parse a vike interval string ("1s"/"1m"/"5m"/"1h"/"1d") into milliseconds. `None` for anything
/// unrecognized — callers fall back to a default interval.
pub(crate) fn interval_ms(interval: &str) -> Option<i64> {
    match interval {
        "1s" => Some(1_000),
        "1m" => Some(60_000),
        "5m" => Some(300_000),
        "1h" => Some(3_600_000),
        "1d" => Some(86_400_000),
        _ => None,
    }
}

/// Map a vike interval string to the closest ibapi historical `BarSize` for the seed fetch.
/// Defaults to one-minute bars for anything unrecognized. `pub(crate)` — reused by the sibling
/// `historical` module; NOT public (would leak the ibapi `BarSize` type in a public signature).
pub(crate) fn hist_bar_size(interval: &str) -> ibapi::market_data::historical::BarSize {
    use ibapi::market_data::historical::BarSize;
    match interval {
        "1m" => BarSize::Min,
        "5m" => BarSize::Min5,
        "1h" => BarSize::Hour,
        "1d" => BarSize::Day,
        _ => BarSize::Min,
    }
}

/// Build an ibapi Contract from the vike IbkrContract (symbology). Mirrors
/// `transport::socket::build_contract` (kept independent — that copy also carries
/// OPT/FUT-only fields the market-data path doesn't need). `pub(crate)` — reused by the sibling
/// `historical` module; NOT public (would leak the ibapi `Contract` type in a public signature).
pub(crate) fn pump_contract(c: &crate::contract::IbkrContract) -> ibapi::contracts::Contract {
    use ibapi::contracts::{Contract, Currency, Exchange, SecurityType, Symbol};
    Contract {
        symbol: Symbol::from(c.symbol.as_str()),
        security_type: match c.sec_type {
            crate::contract::SecType::Stk => SecurityType::Stock,
            crate::contract::SecType::Cash => SecurityType::ForexPair,
            crate::contract::SecType::Fut => SecurityType::Future,
            crate::contract::SecType::Ind => SecurityType::Index,
            crate::contract::SecType::Crypto => SecurityType::Crypto,
            crate::contract::SecType::Opt => SecurityType::Option,
            crate::contract::SecType::Other(ref s) => SecurityType::Other(s.clone()),
        },
        exchange: Exchange::from(c.exchange.as_str()),
        currency: Currency::from(c.currency.as_str()),
        contract_id: c.con_id.unwrap_or(0) as i32,
        ..Contract::default()
    }
}
