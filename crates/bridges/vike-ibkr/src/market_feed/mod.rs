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
    /// Refuses an interval the 5-second realtime stream cannot build (`live_bar_interval_ms`):
    /// an unparseable or misaligned interval is an error here, never a stream of bars under a
    /// default size.
    fn subscribe_bars(
        &mut self,
        symbol: &str,
        interval: &str,
    ) -> Result<SubscriptionId, LiveDataError> {
        let Some(interval_ms) = live_bar_interval_ms(interval) else {
            return Err(LiveDataError::Subscribe(format!(
                "ibkr bars: interval {interval:?} is not a positive multiple of the \
                 {REALTIME_BAR_MS} ms realtime bar the stream aggregates from"
            )));
        };
        self.spawn_with(symbol, interval, move |symbol, interval, ctx| {
            pump::bars_pump(symbol, interval, interval_ms, ctx)
        })
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

/// IBKR's realtime bars arrive at ONE fixed size, five seconds (`reqRealTimeBars` serves no other),
/// and the live bar stream aggregates them up to the requested interval. So a live bar interval
/// must be a positive multiple of this: a shorter or misaligned one cannot be built from the
/// stream and is refused at `subscribe_bars` rather than relabelled.
pub(crate) const REALTIME_BAR_MS: i64 = 5_000;

/// The live bar interval `interval` names, in ms — the shared vocabulary
/// (`vike_model::time::interval_ms`) narrowed to what the 5-second realtime stream can build (a
/// positive multiple of [`REALTIME_BAR_MS`]). `None` for anything else: the caller REFUSES it,
/// never falls back to a default interval.
pub(crate) fn live_bar_interval_ms(interval: &str) -> Option<i64> {
    vike_model::time::interval_ms(interval).filter(|ms| *ms > 0 && ms % REALTIME_BAR_MS == 0)
}

/// The ibapi historical `BarSize` that is EXACTLY `interval_ms` long, or `None` when IB serves no
/// bar of that length (`45m`, `6h`, …). Exact, never "closest": the bars a caller fetches are
/// labelled with the caller's interval (the live seed hands them to `seed_bars` under it), so a
/// nearby size would store, say, one-minute bars as a `15m` series. `pub(crate)` — reused by the
/// sibling `historical` module; NOT public (would leak the ibapi `BarSize` type in a public
/// signature).
pub(crate) fn hist_bar_size(interval_ms: i64) -> Option<ibapi::market_data::historical::BarSize> {
    use ibapi::market_data::historical::BarSize;
    const S: i64 = 1_000;
    const M: i64 = 60 * S;
    const H: i64 = 60 * M;
    Some(match interval_ms {
        x if x == S => BarSize::Sec,
        x if x == 5 * S => BarSize::Sec5,
        x if x == 15 * S => BarSize::Sec15,
        x if x == 30 * S => BarSize::Sec30,
        x if x == M => BarSize::Min,
        x if x == 2 * M => BarSize::Min2,
        x if x == 3 * M => BarSize::Min3,
        x if x == 5 * M => BarSize::Min5,
        x if x == 15 * M => BarSize::Min15,
        x if x == 30 * M => BarSize::Min30,
        x if x == H => BarSize::Hour,
        x if x == 2 * H => BarSize::Hour2,
        x if x == 4 * H => BarSize::Hour4,
        x if x == 24 * H => BarSize::Day,
        _ => return None,
    })
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

#[cfg(test)]
mod interval_tests {
    use super::{hist_bar_size, live_bar_interval_ms};

    /// The live stream takes any interval the shared parser reads that the 5-second realtime bar
    /// builds — `15m` and `4h` included, which the old whitelist streamed as one-minute bars — and
    /// refuses the rest instead of defaulting it.
    #[test]
    fn a_live_bar_interval_is_parsed_or_refused_never_defaulted() {
        for (interval, ms) in [("5s", 5_000), ("1m", 60_000), ("15m", 900_000), ("4h", 14_400_000)]
        {
            assert_eq!(live_bar_interval_ms(interval), Some(ms), "{interval}");
        }
        for refused in ["", "1s", "7s", "0m", "1w", "15", "m", "abc"] {
            assert_eq!(live_bar_interval_ms(refused), None, "{refused:?}");
        }
    }

    /// A historical bar size is EXACT or absent: a length IB serves no bar for is `None`, never the
    /// nearest size.
    #[test]
    fn a_historical_bar_size_is_exact_or_absent() {
        for ms in [1_000, 5_000, 60_000, 900_000, 3_600_000, 14_400_000, 86_400_000] {
            assert!(hist_bar_size(ms).is_some(), "{ms}");
        }
        for ms in [0, 45 * 60_000, 6 * 3_600_000, 7 * 86_400_000] {
            assert!(hist_bar_size(ms).is_none(), "{ms}");
        }
    }
}
