//! IBKR realtime market data (Phase 3a) over the SOCKET: [`DataClientBusSpecBackendIbkrSocket`] is the socket
//! backend of `crate::feed::price::DataClientBusSpecIbkr`, on its OWN ibapi connection (a distinct client_id),
//! streaming quotes/trades/bars onto a `vike_data::DataClientSink` (the spec's fixed sink). No book and no depth: the
//! accounts hold no depth entitlement (docs/decisions/0140), so a book answers `Unsupported` off the declared caps row
//! (`crate::feed`'s `no_book_lane`, shared with the cpapi backend) and no `reqMktDepth` is ever opened. Mirrors
//! the crypto `Feeds` pattern: one joinable pump thread per subscription, draining an ibapi `Subscription<T>` and
//! mapping each item through the pure `map` module. Delayed-by-default (`switch_market_data_type` at connect).

// `pub`, not `pub(crate)`: the pure mappers are exercised from an external integration test
// (`tests/ibkr_market_data_map.rs`, the no-network CI gate) which compiles as its own crate and
// cannot see `pub(crate)` items.
pub mod map;
mod quote_retry;

mod lane;

/// Canonical venue string.
pub(crate) const VENUE: &str = "ibkr";

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use ibapi::client::blocking::Client;
use vike_bridge_core::data_client::bus::DataClientBusSpecKey;
use vike_data::{DataClientError, DataClientSink, DataClientSubscriptionId};

use crate::config::{IbkrConfig, MarketDataType};
use crate::feed::price::DataClientBusSpecBackendIbkr;
use crate::feed::price::session::{DataSessionLease, session_for};
use crate::transport::error_codes::IbkrError;
use crate::transport::gateway_gate::{AskDeadline, DataDialLane, gate_for};
use crate::transport::socket::OPEN_DEADLINE;

/// Per-subscription context handed to a pump thread. `client` is a shared handle to the market-data
/// ibapi connection; `stop` is polled on a bounded `next_timeout` so the thread is joinable.
pub(crate) struct FeedCtx {
    pub(crate) sink: Arc<dyn DataClientSink>,
    pub(crate) stop: Arc<AtomicBool>,
    /// Raised by the pump when its stream ended because the CONNECTION is gone ([`FeedCtx::ended`]).
    pub(crate) lost: Arc<AtomicBool>,
    pub(crate) client: Arc<Client>,
    /// The market-data type the quote pump sends again before EVERY (re)subscribe ([`apply_data_type`]).
    pub(crate) market_data: MarketDataType,
}

impl FeedCtx {
    /// The stream of `symbol` ended on `e`: say so ([`stream_ended`]).
    pub(crate) fn ended(&self, what: &str, symbol: &str, e: &ibapi::Error) {
        stream_ended(&self.lost, what, symbol, e);
    }
}

/// The stream of `symbol` ended on `e`: say so, and when the connection is gone say THAT, which is what
/// [`DataClientBusSpecBackendIbkrSocket`]'s `is_dead` reports (a request IB refused is not a dead connection and stays silent, as it was; the
/// quote pump's market-data refusals are the exception, `quote_retry`: it re-opens those itself).
pub(crate) fn stream_ended(lost: &AtomicBool, what: &str, symbol: &str, e: &ibapi::Error) {
    let gone = connection_gone(e);
    tracing::warn!(%symbol, error = %e, connection_lost = gone, "ibkr {what} stream ended");
    if gone {
        lost.store(true, Ordering::Relaxed);
    }
}

/// **Tell the gateway which market-data type this connection's requests want** (`reqMarketDataType`; delayed by
/// default, so a stock the account has no live subscription for still streams). The type is a per-SESSION setting
/// of the gateway: a connection `ibapi` rebuilt by itself after a socket loss (the gateway restarted, a reset) is a
/// NEW session whose type is the real-time default (`ibapi`'s own notice on a reconnect: "a new connection whose
/// server-side state starts empty"), and a delayed-only account then answers every request `[354]`. So the type is
/// sent at the dial AND again before every quote subscribe, never assumed to have survived (`ibapi` 5.2.0 also
/// re-sends the last type itself after its own reconnect; this send stays, it costs one message and the rule does not
/// lean on the library). Best effort: a failed send is said and the request goes on (it fails on its own words if the
/// connection is gone).
pub(crate) fn apply_data_type(client: &Client, market_data: MarketDataType, what: &str) {
    if let Err(e) = client.switch_market_data_type(market_data.to_ibapi()) {
        tracing::warn!(error = %e, "ibkr {what}: switch_market_data_type failed; continuing");
    }
}

/// The IB codes that say the connection is gone when they reach a stream: 1300 (the socket port was reset and the
/// connection dropped), 1100 and 2110 (the link to IB's servers is lost). A subset of
/// [`crate::transport::error_codes::is_connection_lost`] on purpose: 2103 (a market data farm broken) alone is NOT a lost link
/// (`crate::transport::link_watch`'s module doc has the 136 s the farm was down with the link fine) and 326 is a
/// connect-time refusal.
pub(crate) fn link_gone_code(code: i32) -> bool {
    matches!(code, 1100 | 1300 | 2110)
}

/// Did a stream end because the CONNECTION is gone: a reset or closed socket, a reconnect `ibapi` gave up, or a
/// notice that says so ([`link_gone_code`])? Anything else (a refused request, a parse failure, a stream that ended
/// cleanly) is the request's, not the connection's.
pub(crate) fn connection_gone(e: &ibapi::Error) -> bool {
    use ibapi::Error;
    e.is_connection_lost()
        || matches!(e, Error::ConnectionFailed | Error::Shutdown)
        || matches!(e, Error::Notice(n) if link_gone_code(n.code))
}

/// One subscription's pump thread: its stop flag, whether it ended on a dead connection, and the join handle.
struct FeedThread {
    stop: Arc<AtomicBool>,
    lost: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

/// **The socket backend of the price stream** (`crate::feed::price::DataClientBusSpecIbkr`, which dials it through
/// `crate::feed`'s `connect_data_client`). Holds the gateway session's ONE data connection (`data_client_id`,
/// shared with the any-symbol lookup: [`crate::feed::price::session`]) and one joinable pump thread per subscription.
pub(crate) struct DataClientBusSpecBackendIbkrSocket {
    sink: Arc<dyn DataClientSink>,
    client: Arc<Client>,
    /// The market-data type of the config this feed was connected with (sent again before every quote subscribe).
    market_data: MarketDataType,
    /// The hold on the session's connection; the last hold closes it. `None` once `shutdown` ran.
    lease: Option<DataSessionLease<Client>>,
    next_id: u64,
    subs: HashMap<DataClientSubscriptionId, FeedThread>,
}

impl DataClientBusSpecBackendIbkrSocket {
    /// Take the market-data connection (own client_id) and set the market-data type (delayed by
    /// default). Degrades to `IbkrError::Connect` (→ caller keeps the venue data-less) on failure.
    /// A cpapi config is REFUSED with `IbkrError::Unsupported` before any socket is opened
    /// ([`refuse_cpapi`]).
    /// The connection is the session's: dialled once, bounded and through the gateway's data turn
    /// ([`dial_data_client`]), and joined without a dial when a lookup or another feed holds it.
    pub(crate) fn connect(
        config: &IbkrConfig,
        sink: Arc<dyn DataClientSink>,
    ) -> Result<DataClientBusSpecBackendIbkrSocket, IbkrError> {
        Self::connect_in(config, sink, OPEN_DEADLINE, DataDialLane::Lookup)
    }

    /// [`Self::connect`] under a `within` bound (a test shortens it), for `lane`'s share of the gate's hourly data
    /// budget: the price stream's supervised re-dial (`crate::feed::price::redial`) passes [`DataDialLane::Redial`].
    pub(crate) fn connect_in(
        config: &IbkrConfig,
        sink: Arc<dyn DataClientSink>,
        within: Duration,
        lane: DataDialLane,
    ) -> Result<DataClientBusSpecBackendIbkrSocket, IbkrError> {
        refuse_cpapi(config, "the market-data feed")?;
        let lease =
            session_for(config).lease(|| dial_data_client(config, lane, "market_data", within))?;
        Ok(DataClientBusSpecBackendIbkrSocket {
            sink,
            client: Arc::clone(lease.client()),
            market_data: config.market_data_type,
            lease: Some(lease),
            next_id: 0,
            subs: HashMap::new(),
        })
    }

    /// Allocate an id + stop flag, spawn the pump `body`, and track it for join. `body` receives the
    /// resolved canonical symbol, the interval string ("" for non-bar verbs), and the `FeedCtx`.
    fn spawn_with(
        &mut self,
        symbol: &str,
        interval: &str,
        body: impl FnOnce(String, String, FeedCtx) + Send + 'static,
    ) -> Result<DataClientSubscriptionId, DataClientError> {
        let stop = Arc::new(AtomicBool::new(false));
        let lost = Arc::new(AtomicBool::new(false));
        let ctx = FeedCtx {
            sink: Arc::clone(&self.sink),
            stop: Arc::clone(&stop),
            lost: Arc::clone(&lost),
            client: Arc::clone(&self.client),
            market_data: self.market_data,
        };
        let (symbol, interval) = (symbol.to_string(), interval.to_string());
        let handle = std::thread::Builder::new()
            .name(format!("ibkr-feed-{symbol}"))
            .spawn(move || {
                vike_execution::affinity::pin_current_thread(
                    vike_execution::affinity::Role::MarketData,
                    VENUE,
                );
                body(symbol, interval, ctx)
            })
            .map_err(|e| DataClientError::Subscribe(format!("spawn ibkr feed: {e}")))?;
        let id = DataClientSubscriptionId(self.next_id);
        self.next_id += 1;
        self.subs.insert(id, FeedThread { stop, lost, handle });
        Ok(id)
    }
}

impl DataClientBusSpecBackendIbkr for DataClientBusSpecBackendIbkrSocket {
    /// A bars key refuses an interval the 5-second realtime stream cannot build (`live_bar_interval_ms`):
    /// an unparseable or misaligned interval is an error here, never a stream of bars under a
    /// default size. A book is refused (no book entitlement, docs/decisions/0140) off the declared row,
    /// nothing opened.
    fn want(
        &mut self,
        key: &DataClientBusSpecKey,
    ) -> Result<DataClientSubscriptionId, DataClientError> {
        match key {
            DataClientBusSpecKey::Quotes(symbol) => self.spawn_with(symbol, "", lane::quotes_pump),
            DataClientBusSpecKey::Trades(symbol) => self.spawn_with(symbol, "", lane::trades_pump),
            DataClientBusSpecKey::Bars { symbol, interval } => {
                let Some(interval_ms) = live_bar_interval_ms(interval) else {
                    return Err(DataClientError::Subscribe(format!(
                        "ibkr bars: interval {interval:?} is not a positive multiple of the \
                         {REALTIME_BAR_MS} ms realtime bar the stream aggregates from"
                    )));
                };
                self.spawn_with(symbol, interval, move |symbol, interval, ctx| {
                    lane::bars_pump(symbol, interval, interval_ms, ctx)
                })
            }
            DataClientBusSpecKey::Book(_) => {
                crate::feed::no_book_lane(vike_venues::DataClientVerb::Book)
            }
        }
    }
    fn unwant(&mut self, id: DataClientSubscriptionId) {
        if let Some(pump) = self.subs.remove(&id) {
            pump.stop.store(true, Ordering::Relaxed);
            let _ = pump.handle.join();
        }
    }
    /// A held subscription is dead when its pump ended because the CONNECTION is gone (a reset or closed socket, a
    /// reconnect `ibapi` gave up, a notice of 1100/1300/2110) or the connection reports itself down: the price
    /// stream's supervisor (`crate::feed::price`) re-dials on it. A pump
    /// that ended on a refused request, or that was stopped, is not dead (it stays quiet, as it always did).
    fn is_dead(&self, id: DataClientSubscriptionId) -> bool {
        self.subs.get(&id).is_some_and(|pump| {
            !pump.stop.load(Ordering::Relaxed)
                && (pump.lost.load(Ordering::Relaxed) || !self.client.is_connected())
        })
    }
    fn shutdown(&mut self) {
        for pump in self.subs.values() {
            pump.stop.store(true, Ordering::Relaxed);
        }
        // Give the connection back: the LAST hold closes the ibapi channels, so any pump parked in
        // next()/next_timeout returns and joins. A lookup that still holds the session keeps it (the pumps
        // end on their stop flags within one poll).
        drop(self.lease.take());
        for (_, pump) in self.subs.drain() {
            let _ = pump.handle.join();
        }
    }
}

impl Drop for DataClientBusSpecBackendIbkrSocket {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Open the data lane's ibapi client (`config.data_client_id`) so that it can never delay the order lane's connect
/// (`GatewayGate::dial_data`): refused at once, nothing dialled, while the gateway's rebuild schedule is armed, while
/// another dial holds the turn or an order-lane dial waits for it, while the data lane's own backoff runs (a data
/// dial the deadline won) and past its hourly budget; otherwise spaced after the previous dial and bounded by
/// `within`, so a gateway that takes the socket and never answers ends in a typed `IbkrError::Connect` instead of
/// hanging the caller, and an order-lane dial that arrives meanwhile makes it give way. `what` names the lane in the
/// error ("market_data", "hist"). The market-data type is switched inside the turn (best-effort: delayed by default so
/// it works with no paid subscriptions; realtime when entitled). A timeout does NOT arm the rebuild schedule: only
/// the exec connect and the watchdog do; it arms the data lane's own backoff. `lane` names whose share of the hourly
/// budget it spends ([`DataDialLane`]): the supervised re-dial's, or everyone else's.
pub(crate) fn dial_data_client(
    config: &IbkrConfig,
    lane: DataDialLane,
    what: &'static str,
    within: Duration,
) -> Result<Client, IbkrError> {
    dial_data_client_by(config, lane, what, within, None)
}

/// [`dial_data_client`] for a facts ask with an overall deadline (`AskDeadline`): the waits for the gateway's turn
/// end at it and the open step gets what is left, so the ask's bound is TOTAL.
pub(crate) fn dial_data_client_by(
    config: &IbkrConfig,
    lane: DataDialLane,
    what: &'static str,
    within: Duration,
    ask: Option<AskDeadline>,
) -> Result<Client, IbkrError> {
    let addr = format!("{}:{}", config.host, config.port);
    let gate = gate_for(&addr);
    let (client_id, market_data) = (config.data_client_id, config.market_data_type);
    gate.dial_data_by(lane, &format!("{what} connect"), within, ask, move || {
        let client = Client::connect(&addr, client_id).map_err(|e| {
            IbkrError::Connect(format!("{what} {addr} (client_id={client_id}): {e}"))
        })?;
        apply_data_type(&client, market_data, what);
        Ok(client)
    })
}

/// **A cpapi box opens no ibapi socket** (decision 0140): the ibapi data connections
/// ([`DataClientBusSpecBackendIbkrSocket::connect`], `crate::history::HistoricalFetcher::connect`) refuse a config whose
/// backend is `cpapi` with `IbkrError::Unsupported`, the same skip `crate::fetch_ibkr_properties`
/// makes. Dialling the IB Gateway port from a box whose session is the Client Portal one would be
/// a second product on the one account, which evicts one of the two sessions.
pub(crate) fn refuse_cpapi(config: &IbkrConfig, what: &str) -> Result<(), IbkrError> {
    if config.backend == vike_venues::IbkrBackend::Cpapi {
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
/// (`vike_marketdata::interval::bar_step_ms`) narrowed to what the 5-second realtime stream can build (a
/// positive multiple of [`REALTIME_BAR_MS`]). `None` for anything else: the caller REFUSES it,
/// never falls back to a default interval.
pub(crate) fn live_bar_interval_ms(interval: &str) -> Option<i64> {
    vike_marketdata::interval::bar_step_ms(interval).filter(|ms| ms % REALTIME_BAR_MS == 0)
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
pub(crate) fn pump_contract(
    c: &crate::instrument::contract::IbkrContract,
) -> ibapi::contracts::Contract {
    use ibapi::contracts::{Contract, Currency, Exchange, SecurityType, Symbol};
    Contract {
        symbol: Symbol::from(c.symbol.as_str()),
        security_type: match c.sec_type {
            crate::instrument::contract::SecType::Stk => SecurityType::Stock,
            crate::instrument::contract::SecType::Cash => SecurityType::ForexPair,
            crate::instrument::contract::SecType::Fut => SecurityType::Future,
            crate::instrument::contract::SecType::Ind => SecurityType::Index,
            crate::instrument::contract::SecType::Crypto => SecurityType::Crypto,
            crate::instrument::contract::SecType::Opt => SecurityType::Option,
            crate::instrument::contract::SecType::Other(ref s) => SecurityType::Other(s.clone()),
        },
        exchange: Exchange::from(c.exchange.as_str()),
        currency: Currency::from(c.currency.as_str()),
        contract_id: c.con_id.unwrap_or(0) as i32,
        ..Contract::default()
    }
}

/// The data lane's own connects (the socket backend's, `HistoricalFetcher`) against the silent-gateway fake of the
/// exec, recon and grid tests: bounded, typed, serialised and spaced with every other dial, refused while the gate is
/// armed.
#[cfg(test)]
mod connect_tests {
    use std::time::Instant;

    use super::*;
    use crate::feed::test_support::{RecSink, offline_config};
    use crate::history::HistoricalFetcher;
    use crate::transport::gateway_gate::CONNECT_GAP;
    use crate::transport::socket::IbkrTransportSocket;
    use crate::transport::socket::socket_tests::{QUICK, TEST_BOUND, silent_gateway};

    fn feeds(
        config: &IbkrConfig,
        within: Duration,
    ) -> Result<DataClientBusSpecBackendIbkrSocket, IbkrError> {
        DataClientBusSpecBackendIbkrSocket::connect_in(
            config,
            RecSink::arc(),
            within,
            DataDialLane::Lookup,
        )
    }

    /// **A cpapi box opens no ibapi socket** (decision 0140): the socket backend refuses a `cpapi` config with
    /// `Unsupported` BEFORE dialling. The config points at port 1, where nothing listens, so a connect that skipped
    /// the refusal would answer `Connect` instead and fail this test. (`HistoricalFetcher`'s half is
    /// `crates/bridges/vike-ibkr/tests/ibkr_feeds_lifecycle.rs`'s `a_cpapi_config_opens_no_data_socket`.)
    #[test]
    fn a_cpapi_config_opens_no_data_socket() {
        let config = offline_config(vike_venues::IbkrBackend::Cpapi);
        match DataClientBusSpecBackendIbkrSocket::connect(&config, RecSink::arc()) {
            Err(IbkrError::Unsupported(msg)) => assert!(msg.contains("cpapi"), "{msg}"),
            Err(e) => panic!("the socket backend refused for the wrong reason: {e}"),
            Ok(_) => panic!("the socket backend connected on a cpapi config"),
        }
    }

    /// A gateway that takes the socket and never answers ends both connects in a typed `Connect` within the bound.
    #[test]
    fn the_data_connects_give_up_on_a_silent_gateway() {
        let (_gateway, config) = silent_gateway();
        let began = Instant::now();
        let Err(IbkrError::Connect(words)) = feeds(&config, TEST_BOUND) else {
            panic!("a silent gateway must not connect")
        };
        assert!(words.starts_with("market_data connect did not answer"), "{words}");
        assert!(began.elapsed() < QUICK, "waited {:?}", began.elapsed());

        let (_gateway, config) = silent_gateway();
        let began = Instant::now();
        let Err(IbkrError::Connect(words)) = HistoricalFetcher::connect_within(&config, TEST_BOUND)
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
        let mut config = silent_gateway().1;
        // A closed port no test's listener can be handed (below every ephemeral range, as a released
        // one is not), and this test's own: the gate (`gate_for`) is keyed by address.
        config.port = 2;
        assert!(feeds(&config, Duration::from_secs(5)).is_err(), "the port is closed");
        let began = Instant::now();
        assert!(HistoricalFetcher::connect_within(&config, Duration::from_secs(5)).is_err());
        assert!(began.elapsed() >= CONNECT_GAP, "spaced: {:?}", began.elapsed());
    }

    /// An exec connect the deadline won ARMS the gate; both data connects are then refused with the watchdog's words,
    /// at once, without dialling and without arming or spending anything themselves.
    #[test]
    fn the_data_connects_are_refused_while_the_gate_is_armed() {
        let (_gateway, config) = silent_gateway();
        assert!(
            IbkrTransportSocket::connect_within(&config, TEST_BOUND).is_err(),
            "the deadline won"
        );
        let began = Instant::now();
        let Err(IbkrError::Connect(a)) = feeds(&config, Duration::from_secs(30)) else {
            panic!("armed: refused")
        };
        let Err(IbkrError::Connect(b)) =
            HistoricalFetcher::connect_within(&config, Duration::from_secs(30))
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
        let mut config = silent_gateway().1;
        // Closed and this test's own, as in `the_data_connects_are_spaced_after_the_previous_dial`.
        config.port = 3;
        let Err(IbkrError::Connect(words)) = feeds(&config, Duration::from_secs(5)) else {
            panic!("the port is closed")
        };
        assert!(words.starts_with("market_data ") && words.contains("client_id=2"), "{words}");
        assert!(!gate_for("127.0.0.1:3").is_armed());
    }
}

#[cfg(test)]
#[path = "wire_tests.rs"]
mod wire_tests;

/// What a stream's end says about its CONNECTION: the price stream's supervisor re-dials on a dead one and leaves a
/// refused request quiet (`DataClientBusSpecBackendIbkrSocket`'s `is_dead`).
#[cfg(test)]
mod gone_tests {
    use std::io::ErrorKind;

    use ibapi::{Error, Notice};

    use super::*;

    fn notice(code: i32) -> Error {
        Error::Notice(Notice {
            request_id: None,
            code,
            message: "scripted".into(),
            error_time: None,
            advanced_order_reject_json: String::new(),
        })
    }

    /// The ends that mean the connection is gone: a reset, a closed socket, a reconnect that gave up, a shutdown, and the
    /// notices that say the socket or the server link is gone (1300, 1100, 2110).
    #[test]
    fn a_reset_a_closed_socket_and_a_lost_link_are_a_dead_connection() {
        for e in [
            Error::ConnectionReset,
            Error::ConnectionFailed,
            Error::Shutdown,
            Error::Io(std::io::Error::from(ErrorKind::BrokenPipe)),
            Error::Io(std::io::Error::from(ErrorKind::UnexpectedEof)),
            notice(1300),
            notice(1100),
            notice(2110),
        ] {
            assert!(connection_gone(&e), "{e:?} is a dead connection");
        }
    }

    /// The ends that are the REQUEST's: IB refused the contract (200), a data farm alone is down (2103, which the link
    /// watchdog says is not a lost link), a stream that ended cleanly, a bad argument. They stay quiet, as they always did.
    #[test]
    fn a_refused_request_is_not_a_dead_connection() {
        for e in [
            notice(200),
            notice(354),
            notice(2103),
            notice(326),
            Error::Cancelled,
            Error::EndOfStream,
            Error::Simple("no such contract".into()),
            Error::AlreadySubscribed,
        ] {
            assert!(!connection_gone(&e), "{e:?} is not a dead connection");
        }
        assert!(link_gone_code(1100) && link_gone_code(1300) && link_gone_code(2110));
        assert!(!link_gone_code(2103) && !link_gone_code(1101) && !link_gone_code(1102));
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
