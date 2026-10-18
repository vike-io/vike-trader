//! IBKR realtime market data (Phase 3a): `IbkrFeeds` implements `vike_data::DataClient` on its OWN
//! ibapi connection (a distinct client_id), streaming quotes/trades/bars onto a
//! `vike_data::LiveDataSink`. No book and no depth: the accounts hold no depth entitlement
//! (docs/decisions/0140), so those two verbs answer `Unsupported` off the declared caps row
//! (`crate::data_door`'s `no_book_lane`, shared with the cpapi client) and no `reqMktDepth` is
//! ever opened. Mirrors the crypto `Feeds` pattern: one joinable pump thread per
//! subscription, draining an ibapi `Subscription<T>` and mapping each item through the pure
//! `map` module. Delayed-by-default (`switch_market_data_type` at connect).

// `pub`, not `pub(crate)`: the pure mappers are exercised from an external integration test
// (`tests/ibkr_mktdata_map.rs`, the no-network CI gate) which compiles as its own crate and
// cannot see `pub(crate)` items.
pub mod map;
mod pump;

/// Canonical venue string.
pub(crate) const VENUE: &str = "ibkr";

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use ibapi::client::blocking::Client;
use vike_data::{DataClient, LiveDataError, LiveDataSink, SubscriptionId};

use crate::config::IbkrConfig;
use crate::error::IbkrError;
use crate::transport::gateway_gate::gate_for;
use crate::transport::socket::OPEN_DEADLINE;

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
    /// A cpapi config is REFUSED with `IbkrError::Unsupported` before any socket is opened
    /// ([`refuse_cpapi`]).
    /// The dial is bounded and takes the gateway's connect turn ([`dial_data_client`]).
    pub fn connect(cfg: &IbkrConfig, sink: Arc<dyn LiveDataSink>) -> Result<IbkrFeeds, IbkrError> {
        Self::connect_within(cfg, sink, OPEN_DEADLINE)
    }

    /// [`Self::connect`] under a `within` bound (a test shortens it).
    pub(crate) fn connect_within(
        cfg: &IbkrConfig,
        sink: Arc<dyn LiveDataSink>,
        within: Duration,
    ) -> Result<IbkrFeeds, IbkrError> {
        refuse_cpapi(cfg, "the market-data feed")?;
        let client = dial_data_client(cfg, "mktdata", within)?;
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
    /// No book entitlement (docs/decisions/0140): refused off the declared row, nothing opened.
    fn subscribe_book(&mut self, _symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        crate::data_door::no_book_lane(vike_venues::LiveVerb::Book)
    }
    /// No depth entitlement (docs/decisions/0140): refused off the declared row, nothing opened.
    fn subscribe_depth(&mut self, _symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        crate::data_door::no_book_lane(vike_venues::LiveVerb::Depth)
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

/// Open the data lane's ibapi client (`cfg.data_client_id`) the way every other blocking open step goes: refused
/// while the gateway's rebuild schedule is armed (`GatewayGate::permit_data`, nothing dialled), then ONE dial at a
/// time per gateway, spaced, and bounded by `within` (`GatewayGate::dial`), so a gateway that takes the socket and
/// never answers ends in a typed `IbkrError::Connect` instead of hanging the caller. `what` names the lane in the
/// error ("mktdata", "hist"). The market-data type is switched inside the turn (best-effort: delayed by default so
/// it works with no paid subscriptions; realtime when entitled). A timeout does NOT arm the schedule: only the exec
/// connect and the watchdog do.
pub(crate) fn dial_data_client(
    cfg: &IbkrConfig,
    what: &'static str,
    within: Duration,
) -> Result<Client, IbkrError> {
    let addr = format!("{}:{}", cfg.host, cfg.port);
    let gate = gate_for(&addr);
    gate.permit_data()?;
    let (client_id, mktdata) = (cfg.data_client_id, cfg.mktdata_type);
    gate.dial(&format!("{what} connect"), within, move || {
        let client = Client::connect(&addr, client_id).map_err(|e| {
            IbkrError::Connect(format!("{what} {addr} (client_id={client_id}): {e}"))
        })?;
        if let Err(e) = client.switch_market_data_type(mktdata.to_ibapi()) {
            tracing::warn!(error = %e, "ibkr {what} switch_market_data_type failed; continuing");
        }
        Ok(client)
    })
    .0
}

/// **A cpapi box opens no ibapi socket** (decision 0140): the ibapi data connections
/// ([`IbkrFeeds::connect`], `crate::historical::HistoricalFetcher::connect`) refuse a config whose
/// backend is `cpapi` with `IbkrError::Unsupported`, the same skip `crate::fetch_ibkr_properties`
/// makes. Dialling the IB Gateway port from a box whose session is the Client Portal one would be
/// a second product on the one account, which evicts one of the two sessions.
pub(crate) fn refuse_cpapi(cfg: &IbkrConfig, what: &str) -> Result<(), IbkrError> {
    if cfg.backend == vike_venues::IbkrBackend::Cpapi {
        return Err(IbkrError::Unsupported(format!(
            "ibkr {what} speaks the socket (IB Gateway) protocol and this config's backend is \
             `cpapi`: a cpapi box opens no ibapi socket"
        )));
    }
    Ok(())
}

/// IBKR's realtime bars arrive at ONE fixed size, five seconds (`reqRealTimeBars` serves no other),
/// and the live bar stream aggregates them up to the requested interval. So a live bar interval
/// must be a positive multiple of this: a shorter or misaligned one cannot be built from the
/// stream and is refused at `subscribe_bars` rather than relabelled.
pub(crate) const REALTIME_BAR_MS: i64 = 5_000;

/// The live bar interval `interval` names, in ms — the shared vocabulary
/// (`vike_marketdata::interval::interval_ms`) narrowed to what the 5-second realtime stream can build (a
/// positive multiple of [`REALTIME_BAR_MS`]). `None` for anything else: the caller REFUSES it,
/// never falls back to a default interval.
pub(crate) fn live_bar_interval_ms(interval: &str) -> Option<i64> {
    vike_marketdata::interval::interval_ms(interval)
        .filter(|ms| *ms > 0 && ms % REALTIME_BAR_MS == 0)
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

/// The data lane's own connects (`IbkrFeeds`, `HistoricalFetcher`) against the silent-gateway fake of the exec,
/// recon and grid tests: bounded, typed, serialised and spaced with every other dial, refused while the gate is armed.
#[cfg(test)]
mod connect_tests {
    use std::time::Instant;

    use super::*;
    use crate::data_door::test_support::RecSink;
    use crate::historical::HistoricalFetcher;
    use crate::transport::gateway_gate::CONNECT_GAP;
    use crate::transport::socket::SocketTransport;
    use crate::transport::socket::socket_tests::{QUICK, TEST_BOUND, silent_gateway};

    fn feeds(cfg: &IbkrConfig, within: Duration) -> Result<IbkrFeeds, IbkrError> {
        IbkrFeeds::connect_within(cfg, RecSink::arc(), within)
    }

    /// A gateway that takes the socket and never answers ends both connects in a typed `Connect` within the bound.
    #[test]
    fn the_data_connects_give_up_on_a_silent_gateway() {
        let (_gateway, cfg) = silent_gateway();
        let began = Instant::now();
        let Err(IbkrError::Connect(words)) = feeds(&cfg, TEST_BOUND) else {
            panic!("a silent gateway must not connect")
        };
        assert!(words.starts_with("mktdata connect did not answer"), "{words}");
        assert!(began.elapsed() < QUICK, "waited {:?}", began.elapsed());

        let (_gateway, cfg) = silent_gateway();
        let began = Instant::now();
        let Err(IbkrError::Connect(words)) = HistoricalFetcher::connect_within(&cfg, TEST_BOUND)
        else {
            panic!("a silent gateway must not connect")
        };
        assert!(words.starts_with("hist connect did not answer"), "{words}");
        assert!(began.elapsed() < QUICK, "waited {:?}", began.elapsed());
    }

    /// The two data connects take the SAME turn as every other dial on a gateway: the second waits the gap
    /// after the first one ended.
    #[test]
    fn the_data_connects_are_spaced_after_the_previous_dial() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let port = listener.local_addr().expect("bound").port();
        drop(listener);
        let mut cfg = silent_gateway().1;
        cfg.port = port;
        assert!(feeds(&cfg, Duration::from_secs(5)).is_err(), "the port is closed");
        let began = Instant::now();
        assert!(HistoricalFetcher::connect_within(&cfg, Duration::from_secs(5)).is_err());
        assert!(began.elapsed() >= CONNECT_GAP, "spaced: {:?}", began.elapsed());
    }

    /// An exec connect the deadline won ARMS the gate; both data connects are then refused with the watchdog's words,
    /// at once, without dialling and without arming or spending anything themselves.
    #[test]
    fn the_data_connects_are_refused_while_the_gate_is_armed() {
        let (_gateway, cfg) = silent_gateway();
        assert!(SocketTransport::connect_within(&cfg, TEST_BOUND).is_err(), "the deadline won");
        let began = Instant::now();
        let Err(IbkrError::Connect(a)) = feeds(&cfg, Duration::from_secs(30)) else {
            panic!("armed: refused")
        };
        let Err(IbkrError::Connect(b)) =
            HistoricalFetcher::connect_within(&cfg, Duration::from_secs(30))
        else {
            panic!("armed: refused")
        };
        for words in [a, b] {
            assert!(
                words.contains("link watchdog") && words.contains("nothing was dialled"),
                "{words}"
            );
        }
        assert!(
            began.elapsed() < Duration::from_millis(250),
            "refused without dialling: {:?}",
            began.elapsed()
        );
    }

    /// A closed port is a typed `Connect` that names the lane, and does not arm the gate (a restart is not spaced).
    #[test]
    fn a_refused_data_connect_is_typed_and_fails_fast() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let port = listener.local_addr().expect("bound").port();
        drop(listener);
        let mut cfg = silent_gateway().1;
        cfg.port = port;
        let Err(IbkrError::Connect(words)) = feeds(&cfg, Duration::from_secs(5)) else {
            panic!("the port is closed")
        };
        assert!(words.starts_with("mktdata ") && words.contains("client_id=2"), "{words}");
        assert!(!gate_for(&format!("127.0.0.1:{port}")).is_armed());
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
