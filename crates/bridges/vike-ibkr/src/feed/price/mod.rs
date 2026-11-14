//! **The price stream for symbols the engine was not mounted on** (any-symbol, decision 0174): ONE data client for
//! every symbol, dialled on the first symbol asked for and not before, and SUPERVISED: a connection that dies is
//! re-dialled and every held symbol subscribed again.
//!
//! The control door prices a foreign symbol before it admits it (an opening order needs a mark: the collar and the
//! exposure caps judge it). [`DataClientBusSpecIbkr`] is the venue half of that price lane for IBKR: the source of
//! the shared engine `vike_bridge_core::data_client::bus::DataClientBus`, which the gatewayhub serves as ibkr's
//! credentialed stream (`crates/vike-gatewayhub/src/data/credentialed.rs`'s `build_credentialed`), the ONE ibkr
//! `DataClient` a box runs. Its connection is one of its two BACKENDS (`DataClientBusSpecBackendIbkr`), opened by
//! `crate::feed`'s `connect_data_client` by the config's backend: the socket's (`crate::feed::socket`'s
//! `DataClientBusSpecBackendIbkrSocket`, `ibapi` on `data_client_id`) or the Client Portal's (`crate::feed::cpapi`'s
//! `DataClientBusSpecBackendIbkrCpapi`), each delivering into the engine's fixed sink; nothing is new at the wire.
//! The engine counts each symbol's holders, mints the caller's ids, caps the symbols and refuses every lane but
//! quotes, in this stream's words; the outage voice below is the engine's `Down` and `Up`:
//!
//! * **LAZY.** Building it dials nothing and subscribes nothing (the datahub opens the stream when the tradehub
//!   dials its strategy lane at boot, long before any order names a stock). The first `subscribe_quotes` dials the
//!   client through the gate's data lane (`GatewayGate::dial_data`: it never delays an order-lane dial, and spends
//!   the data lane's own hourly budget as that gate's module doc says), so a box that never admits a stock holds no
//!   connection.
//! * **ONE data client for every symbol, dialled once.** Every later symbol is a `subscribe_quotes` on the same
//!   client, so N symbols are one dial and N subscriptions, never N dials. The connection is the gateway
//!   session's ([`crate::feed::price::session`]): the any-symbol lookup of the same stock JOINS it instead of dialling a
//!   second client under `data_client_id` (error 326). A dial that failed is REMEMBERED for [`REDIAL_AFTER`]:
//!   every ask inside it answers the same sentence without touching the gateway (the gate's own backoff sits
//!   under this one).
//! * **At most `vike_execution::MAX_EXTRA_SYMBOLS` symbols** (the engine's own cap on admitted symbols): IBKR's
//!   market-data line limit is UNMEASURED, so the client never asks for more lines than the engine could hold.
//! * **QUOTES only.** Bars, trades, book and depth are refused by name; the price lane needs nothing else.
//! * **Releasing one key disturbs no other** (decision 0193 verdict 3: the reading the datahub's sharing row of ibkr
//!   rests on). `unsubscribe` stops one symbol's stream and touches nothing else: on the socket one pump and its one
//!   `cancelMarketData` (`feed/socket/wire_tests.rs`'s `releasing_one_quote_key_cancels_exactly_its_market_data_request_and_no_other`),
//!   on the Client Portal one stream, and `umd` only with the last stream of its conId. Two keys that are one symbol here
//!   (a key differs only by case) are two holds on its one stream, and the last hold stops it. A second holder in the
//!   datahub reaches none of this: the broker refcounts its key and asks once.
//!
//! What turns a quote into a PRICE is not here: the quotes and the stream's real-time label travel the tube to the
//! tradehub, whose price tap (`crates/vike-tradehub/src/feeds/price_tap.rs`'s `PriceTap::labelled_quotes`) prices
//! from a two-sided quote of any stream the venue has labelled (decision 0140: an order MAY be priced off delayed
//! data, the label is information and never a gate for a price; the one rule on an OPENING order is this crate's exec,
//! `crate::execution::delayed_order`), and records the label with the order. Whether the paper
//! account is entitled to real-time stocks is UNMEASURED (decision 0140 records delayed stocks): on a box without
//! the entitlement every stock is labelled delayed (about 15 minutes old) and is priced as such.
//!
//! # The supervisor: a dead connection is re-dialled (the crate's CLAUDE.md has the one logoff measured)
//!
//! A thread (`ibkr-px-redial`, started with the first dial, joined at the shutdown) looks at the held symbols
//! every second. When the backend's `is_dead` says a stream's connection is gone (IB's daily logoff, a lost link, a
//! closed socket), the policy in `crate::feed::price::redial` decides: the outage is DISCLOSED at once (`stream_status`
//! `GapStart` on the `quotes` lane of every held symbol, once), a dial is made when the gateway can take it (the
//! schedule, the budget it shares with the lookups, the port answering), every held symbol is subscribed again on
//! the new connection, and `Live` follows when the connection has stood `crate::feed::price::redial`'s `LIVE_AFTER`. The policy
//! module doc has the schedule, the budget argument and the daily logoff.
//!
//! * **The caller's ids are STABLE.** The id `subscribe_quotes` returned stays the symbol's id across every
//!   re-subscribe (the client's own id changes under it), so `unsubscribe` still finds the stream. `is_dead` is
//!   `false` for every id: this client heals itself, and the datahub's broker must not drop and re-ask a key it is
//!   already repairing.
//! * **The dial runs OUTSIDE the lock** the caller's thread takes, so a `shutdown` waits for a re-dial in flight no
//!   longer than the gate's open deadline, and a `subscribe_quotes` never waits for one.
//! * **`GapStart` is a dead-man window.** The tradehub's link dead-man reads the venue source of a held symbol: a
//!   `GapStart` opens its grace window and `Live` closes it (decision 0038), and ibkr's price-only venue is armed by
//!   the two hub links while it holds an admitted symbol or a live order
//!   (`crates/vike-tradehub/src/venue_arming/deadman.rs`'s `price_only_link_disclosure`). A data outage past the
//!   grace therefore cancels ibkr's resting orders and halts ibkr (decision 0180). The daily logoff opens one every
//!   night, about 100 s long against the default 120 s grace. ⚠ A released key's window is CLOSED (`Live` on
//!   `unsubscribe` and at the shutdown): a window nobody closes trips the venue after the grace.
//! * **Not detected:** a stream IB refused (no such contract), a subscription that ended cleanly, and a server link
//!   lost with the socket still up (error 1100 reaches the client's global notice stream, which the data client
//!   does not bind): those stay quiet and their prices age out of the tap (fail-closed), as they did.

/// The POLICY of the price stream's supervised re-dial (pure, over a clock it is handed): when a dead data
/// connection is dialled again, the backoff, the daily logoff's quiet hint and when the outage is over.
pub(crate) mod redial;
/// The ONE data connection per gateway session that the price stream, the any-symbol lookup and the grid
/// pre-fetch share (error 326 otherwise: IB refuses a second client under an id in use).
#[cfg(feature = "ibkr-socket")]
pub(crate) mod session;

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use vike_bridge_core::data_client::bus::{
    DataClientBusSink, DataClientBusSpec, DataClientBusSpecDeliver, DataClientBusSpecEvent,
    DataClientBusSpecHold, DataClientBusSpecKey,
};
use vike_data::{DataClientError, DataClientSink, DataClientSubscriptionId};

use crate::config::IbkrConfig;
use crate::feed::price::redial::{PROBE_EVERY, PriceRedial, PriceRedialStep};
use crate::feed::{VENUE, connect_data_client, redial_data_client, redial_ready};
use crate::transport::error_codes::IbkrError;

/// How long a failed first dial is remembered, so a burst of orders does not become a burst of dials.
pub const REDIAL_AFTER: Duration = Duration::from_secs(30);

/// How often the supervisor looks at the held symbols.
const TICK: Duration = Duration::from_secs(1);

/// Why the client is being dialled: the first dial of the stream, or the supervisor's re-dial of a connection it had
/// (which spends the re-dial lane's share of the gateway's hourly data budget).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialKind {
    /// The first dial, made by the first symbol asked for.
    First,
    /// A re-dial after the connection died.
    Redial,
}

/// **One backend's dialled data connection, as [`DataClientBusSpecIbkr`] drives it** (the module doc): the socket's
/// (`crate::feed::socket`'s `DataClientBusSpecBackendIbkrSocket`) or the Client Portal's (`crate::feed::cpapi`'s
/// `DataClientBusSpecBackendIbkrCpapi`), delivering into the spec's fixed sink. Each serves quotes, trades and bars
/// and refuses a book; the spec's `BARS` and `TRADES` keep the price stream quotes-only (owner ruling B-12), so only
/// a quotes key reaches a backend today.
pub(crate) trait DataClientBusSpecBackendIbkr: Send {
    /// Open `key`'s stream. The id names it to [`Self::unwant`] and [`Self::is_dead`]; it is this connection's own
    /// (the caller's stays stable across a re-dial, the module doc).
    fn want(
        &mut self,
        key: &DataClientBusSpecKey,
    ) -> Result<DataClientSubscriptionId, DataClientError>;
    /// Stop the stream `id` names and nothing else. An unknown id is a no-op.
    fn unwant(&mut self, id: DataClientSubscriptionId);
    /// Whether `id`'s stream ended because its CONNECTION is gone: the supervisor re-dials on it. The default `false`
    /// is the Client Portal's, whose pump redials by itself.
    fn is_dead(&self, _id: DataClientSubscriptionId) -> bool {
        false
    }
    /// Phase one of a teardown: raise the stop flags and return at once. The connection stays usable.
    fn begin_shutdown(&mut self) {}
    /// Stop every stream and give the connection back.
    fn shutdown(&mut self);
}

/// How the source opens its connection over the sink (the production dial is [`DataClientBusSpecIbkr::for_config`];
/// a test passes a scripted one).
pub(crate) type Dial = Box<
    dyn Fn(
            Arc<dyn DataClientSink>,
            DialKind,
        ) -> Result<Box<dyn DataClientBusSpecBackendIbkr>, IbkrError>
        + Send
        + Sync,
>;

/// Whether the gateway can take a re-dial right now: `Ok`, or how long to wait before asking again (its port is
/// closed, a turn is held, the data lane's backoff or budget says no). Asked before every re-dial; spends nothing.
pub type Ready = Box<dyn Fn() -> Result<(), Duration> + Send + Sync>;

/// What the lock guards: the client, the symbols held, and the outage state.
struct Core {
    client: Option<Box<dyn DataClientBusSpecBackendIbkr>>,
    /// The keys a stream is held for (their symbols upper case, the engine's spelling), each with the id the CURRENT
    /// client gave its stream.
    held: HashMap<DataClientBusSpecKey, DataClientSubscriptionId>,
    /// When the last FIRST dial failed and with what words.
    failed: Option<(Instant, String)>,
    policy: PriceRedial,
    closed: bool,
}

/// **ibkr's price stream as the shared engine's source** (`vike_bridge_core::data_client::bus::DataClientBus` over
/// this; the module doc). The engine counts the holders of each symbol, mints the ids (stable for the caller across
/// every re-subscribe: the client's own change under them), caps the symbols, refuses every lane but quotes, and
/// discloses an outage per held symbol; this source dials the one data client, re-dials a dead one and subscribes
/// every held symbol again. The client delivers into the engine's fixed sink, so its quotes, labels and statuses meet
/// the engine's rules.
pub struct DataClientBusSpecIbkr {
    dial: Dial,
    ready: Ready,
    /// The sink the dialled client delivers into, and the supervisor says the connection's `Down`/`Up` through.
    relay: Arc<DataClientBusSink>,
    core: Mutex<Core>,
    /// Raised to stop the supervisor, with the condvar that wakes its wait.
    stop: (Mutex<bool>, Condvar),
    driver: Mutex<Option<JoinHandle<()>>>,
    /// Whether the supervisor thread is started (a test drives [`DataClientBusSpecIbkr::supervise`] by hand instead).
    driven: bool,
}

impl DataClientBusSpecIbkr {
    /// A source that opens its connection with `dial` on the first symbol subscribed and re-dials a dead one when
    /// `ready` allows.
    #[must_use]
    pub(crate) fn new(dial: Dial, ready: Ready) -> Self {
        Self::build(dial, ready, true)
    }

    fn build(dial: Dial, ready: Ready, driven: bool) -> Self {
        let core = Core {
            client: None,
            held: HashMap::new(),
            failed: None,
            policy: PriceRedial::default(),
            closed: false,
        };
        Self {
            dial,
            ready,
            relay: Arc::default(),
            core: Mutex::new(core),
            stop: (Mutex::new(false), Condvar::new()),
            driver: Mutex::new(None),
            driven,
        }
    }

    /// The production source: the account's data connection by its backend (the socket's or the Client Portal's,
    /// `DataClientBusSpecBackendIbkr`), first dialled through `connect_data_client` and re-dialled through the
    /// re-dial lane, which asks the gateway's readiness first.
    #[must_use]
    pub fn for_config(config: IbkrConfig) -> Self {
        let ready_config = config.clone();
        let dial: Dial = Box::new(move |sink, kind| match kind {
            DialKind::First => connect_data_client(&config, sink),
            DialKind::Redial => redial_data_client(&config, sink),
        });
        Self::new(dial, Box::new(move || redial_ready(&ready_config)))
    }

    fn lock(&self) -> MutexGuard<'_, Core> {
        self.core.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Start the supervisor with the first dial (never before: a box that holds nothing runs no thread).
    fn ensure_driver(self: &Arc<Self>) {
        let mut driver = self.driver.lock().unwrap_or_else(PoisonError::into_inner);
        if !self.driven || driver.is_some() {
            return;
        }
        let source = Arc::clone(self);
        *driver = std::thread::Builder::new()
            .name("ibkr-px-redial".into())
            .spawn(move || source.run())
            .map_err(|e| {
                tracing::warn!(error = %e, "ibkr price stream: could not start its supervisor; a dead data connection will not be re-dialled");
            })
            .ok();
    }

    /// The client closed: the supervisor stops, the data client is shut down, the supervisor is joined (a re-dial in
    /// flight is waited for no longer than the gate's open deadline), and nothing more reaches the closed client.
    fn close(&self) {
        let (stopped, wake) = &self.stop;
        *stopped.lock().unwrap_or_else(PoisonError::into_inner) = true;
        wake.notify_all();
        let client = {
            let mut core = self.lock();
            core.closed = true;
            core.held.clear();
            core.client.take()
        };
        if let Some(mut client) = client {
            client.shutdown();
        }
        if let Some(driver) = self.driver.lock().unwrap_or_else(PoisonError::into_inner).take() {
            let _ = driver.join();
        }
        self.relay.detach();
    }

    /// The supervisor's loop: one [`Self::supervise`] per `TICK` until the stop.
    fn run(&self) {
        let (stopped, wake) = &self.stop;
        loop {
            let guard = stopped.lock().unwrap_or_else(PoisonError::into_inner);
            let (guard, _) = wake
                .wait_timeout_while(guard, TICK, |stopped| !*stopped)
                .unwrap_or_else(PoisonError::into_inner);
            if *guard {
                return;
            }
            drop(guard);
            self.supervise(Instant::now(), vike_model::now_ms());
        }
    }

    /// The client, dialled now if there is none (a failed dial is remembered for [`REDIAL_AFTER`]).
    fn ensure_client<'a>(
        &self,
        core: &'a mut Core,
        symbol: &str,
    ) -> Result<&'a mut Box<dyn DataClientBusSpecBackendIbkr>, DataClientError> {
        if core.client.is_none() {
            if let Some((at, why)) = &core.failed
                && at.elapsed() < REDIAL_AFTER
            {
                return Err(DataClientError::Subscribe(why.clone()));
            }
            match (self.dial)(Arc::clone(&self.relay) as Arc<dyn DataClientSink>, DialKind::First) {
                Ok(client) => {
                    core.client = Some(client);
                    core.failed = None;
                }
                Err(why) => {
                    let why = format!("ibkr: no market-data connection for {symbol}: {why}");
                    core.failed = Some((Instant::now(), why.clone()));
                    return Err(DataClientError::Subscribe(why));
                }
            }
        }
        core.client.as_mut().ok_or_else(|| {
            DataClientError::Subscribe("ibkr: no market-data connection".to_string())
        })
    }

    /// Whether a held symbol's stream says its connection is gone.
    fn any_dead(core: &Core) -> bool {
        core.client
            .as_ref()
            .is_some_and(|client| core.held.values().any(|inner| client.is_dead(*inner)))
    }

    /// Subscribe every held key on `fresh`, then make it the client and hand back the old one (to be shut down
    /// outside the lock). A key `fresh` refuses fails the whole install: `fresh` is shut down and the old client
    /// stays.
    fn install(
        core: &mut Core,
        mut fresh: Box<dyn DataClientBusSpecBackendIbkr>,
    ) -> Result<Option<Box<dyn DataClientBusSpecBackendIbkr>>, String> {
        let mut ids = Vec::with_capacity(core.held.len());
        for key in core.held.keys() {
            match fresh.want(key) {
                Ok(id) => ids.push((key.clone(), id)),
                Err(e) => {
                    fresh.shutdown();
                    let symbol = key.symbol();
                    return Err(format!("{symbol} was refused on the new connection: {e}"));
                }
            }
        }
        for (key, id) in ids {
            if let Some(inner) = core.held.get_mut(&key) {
                *inner = id;
            }
        }
        Ok(core.client.replace(fresh))
    }

    /// One look of the supervisor at `now` (`wall_ms` is the same instant on the wall clock): the policy
    /// (`crate::feed::price::redial`) decides; an outage is disclosed (the engine's `Down` opens one `GapStart` per held symbol)
    /// and a recovery told (`Up`, a `Live` echoing each), and a dial the gateway can take is made OUTSIDE the lock.
    fn supervise(&self, now: Instant, wall_ms: i64) {
        {
            let mut core = self.lock();
            if core.closed {
                return;
            }
            let dead = Self::any_dead(&core);
            match core.policy.observe(now, wall_ms, dead) {
                PriceRedialStep::Healthy => return,
                PriceRedialStep::Recovered => {
                    self.relay.send(DataClientBusSpecEvent::Up);
                    tracing::info!(
                        symbols = core.held.len(),
                        "ibkr price stream: the data connection is back and its quote streams are live again"
                    );
                    return;
                }
                PriceRedialStep::Down { opened, attempt } => {
                    self.relay.send(DataClientBusSpecEvent::Down { at_ms: wall_ms });
                    if opened {
                        let left = |at: Instant| at.saturating_duration_since(now).as_secs();
                        tracing::warn!(
                            symbols = core.held.len(),
                            next_dial_in_s = core.policy.next_dial().map_or(0, left),
                            daily_logoff_quiet_s = ?core.policy.quiet_until().map(left),
                            "ibkr price stream: the data connection is DOWN (GapStart disclosed for every held symbol); \
                             it is re-dialled on a spaced schedule and the prices go stale (none yet) meanwhile"
                        );
                    }
                    if !attempt {
                        return;
                    }
                }
            }
        }
        if let Err(wait) = (self.ready)() {
            if wait > PROBE_EVERY {
                tracing::warn!(
                    wait_s = wait.as_secs(),
                    "ibkr price stream: the data connection is down and the gate allows no re-dial for another {} s \
                     (its backoff, or the re-dial lane's share of the hourly data budget): the symbols stay down",
                    wait.as_secs()
                );
            }
            self.lock().policy.not_ready(now, wait);
            return;
        }
        let dialled =
            (self.dial)(Arc::clone(&self.relay) as Arc<dyn DataClientSink>, DialKind::Redial);
        let mut core = self.lock();
        if core.closed {
            drop(core);
            if let Ok(mut fresh) = dialled {
                fresh.shutdown();
            }
            return;
        }
        let outcome = dialled
            .map_err(|why| why.to_string())
            .and_then(|fresh| Self::install(&mut core, fresh));
        match outcome {
            Ok(old) => {
                core.policy.redialled(now);
                tracing::info!(
                    symbols = core.held.len(),
                    down_s = core.policy.down_for(now).as_secs(),
                    "ibkr price stream: re-dialled; every held symbol is subscribed again (live after a short probation)"
                );
                drop(core);
                if let Some(mut old) = old {
                    old.shutdown();
                }
            }
            Err(why) => {
                core.policy.dial_failed(now);
                tracing::warn!(
                    failed_dials = core.policy.failed(),
                    down_s = core.policy.down_for(now).as_secs(),
                    "ibkr price stream: the re-dial failed ({why}); the next one waits its backoff (60 s, 2 and 5 min)"
                );
            }
        }
    }
}

impl DataClientBusSpec for DataClientBusSpecIbkr {
    type Hold = DataClientBusSpecHoldIbkr;
    const VENUE: &'static str = VENUE;
    const CLOSED: &'static str = "ibkr: the price stream is shut down";
    const BARS: Result<(), &'static str> = Err("ibkr's price stream carries quotes only (no bars)");
    const TRADES: Result<(), &'static str> =
        Err("ibkr's price stream carries quotes only (no trades)");
    const BOOK: Result<(), &'static str> = Err("ibkr's price stream carries quotes only (no book)");
    /// The engine's own cap on admitted symbols: IBKR's market-data line limit is UNMEASURED, so the client never
    /// asks for more lines than the engine could hold.
    const MAX_SYMBOLS: Option<(usize, &'static str)> =
        Some((vike_execution::MAX_EXTRA_SYMBOLS, "the market-data line limit is unmeasured"));
    /// Owner ruling B-11: the engine's one price check drops a one-sided, crossed or non-finite quote (the socket's
    /// and the Client Portal's pumps check little).
    const PRICE_CHECK: bool = true;
    /// A shutdown shuts the data client down whole (the hold's drop), not symbol by symbol.
    const UNWANT_AT_SHUTDOWN: bool = false;

    fn consume(self: &Arc<Self>, deliver: DataClientBusSpecDeliver) -> DataClientBusSpecHoldIbkr {
        self.relay.attach(deliver);
        DataClientBusSpecHoldIbkr { source: Arc::clone(self) }
    }
}

/// The engine's hold on [`DataClientBusSpecIbkr`]; dropping it closes the source.
pub struct DataClientBusSpecHoldIbkr {
    source: Arc<DataClientBusSpecIbkr>,
}

impl DataClientBusSpecHold for DataClientBusSpecHoldIbkr {
    /// A symbol's first holder dials the client if there is none (LAZY, the module doc) and opens its quote stream; a
    /// later holder of it opens nothing. The supervisor starts with the first stream.
    fn ask(&self, key: &DataClientBusSpecKey, streaming: bool) -> Result<(), DataClientError> {
        if streaming {
            return Ok(());
        }
        let symbol = key.symbol();
        let source = &self.source;
        {
            let mut core = source.lock();
            let client = source.ensure_client(&mut core, symbol)?;
            let inner = client.want(key).map_err(|e| {
                DataClientError::Subscribe(format!(
                    "ibkr: the quote stream of {symbol} was refused: {e}"
                ))
            })?;
            core.held.insert(key.clone(), inner);
        }
        source.ensure_driver();
        Ok(())
    }

    /// The symbol's last holder left: its one stream stops (one `cancelMarketData` on the socket, `umd` on the Client
    /// Portal with the last stream of its conId) and nothing else does.
    fn unwant(&self, key: &DataClientBusSpecKey) {
        let mut core = self.source.lock();
        let Some(inner) = core.held.remove(key) else { return };
        if let Some(client) = core.client.as_mut() {
            client.unwant(inner);
        }
    }

    /// ⚠ Not the end of this client (the crate's `CLAUDE.md`): the wire's `BeginShutdown` keeps the session usable, so
    /// it reaches the data client alone and the supervisor goes on.
    fn begin_shutdown(&self) {
        if let Some(client) = self.source.lock().client.as_mut() {
            client.begin_shutdown();
        }
    }
}

impl Drop for DataClientBusSpecHoldIbkr {
    fn drop(&mut self) {
        self.source.close();
    }
}

#[cfg(test)]
#[path = "price_tests.rs"]
mod price_tests;
