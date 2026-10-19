//! **The price stream for symbols the engine was not mounted on** (any-symbol, decision 0174): ONE data client for
//! every symbol, dialled on the first symbol asked for and not before, and SUPERVISED: a connection that dies is
//! re-dialled and every held symbol subscribed again.
//!
//! The control door prices a foreign symbol before it admits it (an opening order needs a mark: the collar and the
//! exposure caps judge it). [`LazyQuotes`] is the venue half of that price lane for IBKR: the [`DataClient`] the
//! gatewayhub serves as ibkr's credentialed stream (`crates/vike-gatewayhub/src/data/credentialed.rs`'s
//! `build_credentialed`), built on the seam the data lane already has - [`connect_data_client`] (`IbkrFeeds` on the
//! socket, `CpapiFeeds` on the Client Portal) - and on nothing new at the wire:
//!
//! * **LAZY.** Building it dials nothing and subscribes nothing (the datahub opens the stream when the tradehub
//!   dials its strategy lane at boot, long before any order names a stock). The first `subscribe_quotes` dials the
//!   client through the gate's data lane (`GatewayGate::dial_data`: it never delays an order-lane dial, and spends
//!   the data lane's own hourly budget), so a box that never admits a stock holds no connection.
//! * **ONE data client for every symbol, dialled once.** Every later symbol is a `subscribe_quotes` on the same
//!   client, so N symbols are one dial and N subscriptions, never N dials. The connection is the gateway
//!   session's ([`crate::data_session`]): the any-symbol lookup of the same stock JOINS it instead of dialling a
//!   second client under `data_client_id` (error 326). A dial that failed is REMEMBERED for [`REDIAL_AFTER`]:
//!   every ask inside it answers the same sentence without touching the gateway (the gate's own backoff sits
//!   under this one).
//! * **At most `vike_exec::MAX_EXTRA_SYMBOLS` symbols** (the engine's own cap on admitted symbols): IBKR's
//!   market-data line limit is UNMEASURED, so the client never asks for more lines than the engine could hold.
//! * **QUOTES only.** Bars, trades, book and depth are refused by name; the price lane needs nothing else.
//!
//! What turns a quote into a PRICE is not here: the quotes and the stream's real-time label travel the tube to the
//! tradehub, whose price tap (`crates/vike-tradehub/src/feeds/price_tap.rs`'s `PriceTap::real_time_only`) prices
//! only from a two-sided quote of a stream labelled real-time and refuses a delayed one by name. Whether the paper
//! account is entitled to real-time stocks is UNMEASURED (decision 0140 records delayed stocks): on a box without
//! the entitlement every stock answers "delayed" and the door refuses the opening order, the safe direction.
//!
//! # The supervisor: a dead connection is re-dialled (and nothing here has run against a gateway)
//!
//! A thread (`ibkr-px-redial`, started with the first dial, joined at the shutdown) looks at the held symbols
//! every second. When `IbkrFeeds::is_dead` says a stream's connection is gone (IB's daily logoff, a lost link, a
//! closed socket), the policy in `crate::redial` decides: the outage is DISCLOSED at once (`stream_status`
//! `GapStart` on the `quotes` lane of every held symbol, once), a dial is made when the gateway can take it (the
//! schedule, the budget it shares with the lookups, the port answering), every held symbol is subscribed again on
//! the new connection, and `Live` follows when the connection has stood `crate::redial`'s `LIVE_AFTER`. The policy
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

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use vike_data::{DataClient, LiveDataError, LiveDataSink, StreamLabel, SubscriptionId};
use vike_model::StreamStatus;

use crate::config::IbkrConfig;
use crate::connect_data_client;
use crate::data_door::{VENUE, redial_data_client, redial_ready};
use crate::error::IbkrError;
use crate::redial::{PROBE_EVERY, Redial, Step};

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

/// How the client opens its connection over the sink (the production dial is [`LazyQuotes::for_config`]; a test
/// passes a scripted one).
pub type Dial = Box<
    dyn Fn(Arc<dyn LiveDataSink>, DialKind) -> Result<Box<dyn DataClient + Send>, IbkrError>
        + Send
        + Sync,
>;

/// Whether the gateway can take a re-dial right now: `Ok`, or how long to wait before asking again (its port is
/// closed, a turn is held, the data lane's backoff or budget says no). Asked before every re-dial; spends nothing.
pub type Ready = Box<dyn Fn() -> Result<(), Duration> + Send + Sync>;

/// One symbol the stream is held for.
struct Held {
    /// The id the caller was given: stable across every re-subscribe.
    id: SubscriptionId,
    /// The id the CURRENT client gave this symbol's stream.
    inner: SubscriptionId,
    /// When the outage that took this symbol down was disclosed (epoch ms): the `Live` that ends it echoes it.
    gap: Option<i64>,
}

/// What the lock guards: the client, the symbols held, and the outage state.
struct Core {
    client: Option<Box<dyn DataClient + Send>>,
    /// The symbols a quote stream is held for (upper case, the engine's spelling).
    held: HashMap<String, Held>,
    next_id: u64,
    /// When the last FIRST dial failed and with what words.
    failed: Option<(Instant, String)>,
    policy: Redial,
    closed: bool,
}

/// What the caller's thread and the supervisor's share.
struct Shared {
    dial: Dial,
    ready: Ready,
    sink: Arc<dyn LiveDataSink>,
    core: Mutex<Core>,
    /// Raised to stop the supervisor, with the condvar that wakes its wait.
    stop: (Mutex<bool>, Condvar),
}

/// See the module doc.
pub struct LazyQuotes {
    shared: Arc<Shared>,
    driver: Option<JoinHandle<()>>,
    /// Whether the supervisor thread is started (a test drives [`Shared::supervise`] by hand instead).
    driven: bool,
}

impl LazyQuotes {
    /// A client that opens its connection with `dial` on the first symbol subscribed, delivering into `sink`, and
    /// re-dials a dead one when `ready` allows.
    #[must_use]
    pub fn new(sink: Arc<dyn LiveDataSink>, dial: Dial, ready: Ready) -> Self {
        Self::build(sink, dial, ready, true)
    }

    fn build(sink: Arc<dyn LiveDataSink>, dial: Dial, ready: Ready, driven: bool) -> Self {
        let core = Core {
            client: None,
            held: HashMap::new(),
            next_id: 0,
            failed: None,
            policy: Redial::default(),
            closed: false,
        };
        let shared = Shared {
            dial,
            ready,
            sink,
            core: Mutex::new(core),
            stop: (Mutex::new(false), Condvar::new()),
        };
        Self { shared: Arc::new(shared), driver: None, driven }
    }

    /// The production client: `cfg`'s own product through the one door (`connect_data_client`), re-dialled through
    /// the gate's re-dial lane.
    #[must_use]
    pub fn for_config(cfg: IbkrConfig, sink: Arc<dyn LiveDataSink>) -> Self {
        let ready_cfg = cfg.clone();
        let dial: Dial = Box::new(move |sink, kind| match kind {
            DialKind::First => connect_data_client(&cfg, sink),
            DialKind::Redial => redial_data_client(&cfg, sink),
        });
        Self::new(sink, dial, Box::new(move || redial_ready(&ready_cfg)))
    }

    /// Start the supervisor once there is a connection to supervise.
    fn ensure_driver(&mut self) {
        if !self.driven || self.driver.is_some() {
            return;
        }
        let shared = Arc::clone(&self.shared);
        self.driver = std::thread::Builder::new()
            .name("ibkr-px-redial".into())
            .spawn(move || shared.run())
            .map_err(|e| {
                tracing::warn!(error = %e, "ibkr price stream: could not start its supervisor; a dead data connection will not be re-dialled");
            })
            .ok();
    }

    fn close(&mut self) {
        self.shared.halt();
        let client = self.shared.lock().close(self.shared.sink.as_ref());
        if let Some(mut client) = client {
            client.shutdown();
        }
        if let Some(driver) = self.driver.take() {
            let _ = driver.join();
        }
    }
}

impl Core {
    fn fresh_id(&mut self) -> SubscriptionId {
        self.next_id += 1;
        SubscriptionId(self.next_id)
    }

    /// Whether any held symbol's stream is dead (its connection is gone).
    fn any_dead(&self) -> bool {
        self.client
            .as_ref()
            .is_some_and(|client| self.held.values().any(|held| client.is_dead(held.inner)))
    }

    /// Tell the sink the outage of every held symbol that has not been told: ONE `GapStart` per symbol per outage.
    fn disclose_gaps(&mut self, sink: &dyn LiveDataSink, wall_ms: i64) {
        for (symbol, held) in &mut self.held {
            if held.gap.is_none() {
                held.gap = Some(wall_ms);
                let status = StreamStatus::GapStart { at_ts_ms: wall_ms };
                sink.stream_status(VENUE, symbol, StreamLabel::Quotes.as_str(), status);
            }
        }
    }

    /// Tell the sink the outage is over for every symbol that was told it began.
    fn disclose_live(&mut self, sink: &dyn LiveDataSink) {
        for (symbol, held) in &mut self.held {
            close_gap(sink, symbol, held.gap.take());
        }
    }

    /// Put every held symbol on `fresh` and make it the client; the OLD client is handed back for the caller to shut
    /// down once it has let go of the lock (its release can wait on the session's). All or nothing: a symbol `fresh`
    /// refuses leaves the old state as it was and `fresh` shut down (the words say which).
    fn install(
        &mut self,
        mut fresh: Box<dyn DataClient + Send>,
    ) -> Result<Option<Box<dyn DataClient + Send>>, String> {
        let mut ids = Vec::with_capacity(self.held.len());
        for symbol in self.held.keys() {
            match fresh.subscribe_quotes(symbol) {
                Ok(id) => ids.push((symbol.clone(), id)),
                Err(e) => {
                    fresh.shutdown();
                    return Err(format!("{symbol} was refused on the new connection: {e}"));
                }
            }
        }
        for (symbol, id) in ids {
            if let Some(held) = self.held.get_mut(&symbol) {
                held.inner = id;
            }
        }
        Ok(self.client.replace(fresh))
    }

    /// Release every key (closing the window of one that is down); the client is handed back for the caller to shut
    /// down once it has let go of the lock.
    fn close(&mut self, sink: &dyn LiveDataSink) -> Option<Box<dyn DataClient + Send>> {
        self.closed = true;
        for (symbol, held) in self.held.drain() {
            close_gap(sink, &symbol, held.gap);
        }
        self.client.take()
    }
}

/// `Live` for a symbol whose outage was disclosed at `gap`, if it was.
fn close_gap(sink: &dyn LiveDataSink, symbol: &str, gap: Option<i64>) {
    if let Some(at) = gap {
        let status = StreamStatus::Live { gap_started_ts_ms: Some(at) };
        sink.stream_status(VENUE, symbol, StreamLabel::Quotes.as_str(), status);
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Core> {
        self.core.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Ask the supervisor to stop (it is joined by [`LazyQuotes::close`]).
    fn halt(&self) {
        let (stopped, wake) = &self.stop;
        *stopped.lock().unwrap_or_else(PoisonError::into_inner) = true;
        wake.notify_all();
    }

    /// The supervisor thread: one look every [`TICK`] until halted.
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

    /// Dial the first client (under the caller's lock, as the first symbol always did).
    fn ensure_client<'a>(
        &self,
        core: &'a mut Core,
        symbol: &str,
    ) -> Result<&'a mut Box<dyn DataClient + Send>, LiveDataError> {
        if core.client.is_none() {
            if let Some((at, why)) = &core.failed
                && at.elapsed() < REDIAL_AFTER
            {
                return Err(LiveDataError::Subscribe(why.clone()));
            }
            match (self.dial)(Arc::clone(&self.sink), DialKind::First) {
                Ok(client) => {
                    core.client = Some(client);
                    core.failed = None;
                }
                Err(why) => {
                    let why = format!("ibkr: no market-data connection for {symbol}: {why}");
                    core.failed = Some((Instant::now(), why.clone()));
                    return Err(LiveDataError::Subscribe(why));
                }
            }
        }
        core.client
            .as_mut()
            .ok_or_else(|| LiveDataError::Subscribe("ibkr: no market-data connection".to_string()))
    }

    /// **One look at the held symbols** (the supervisor's tick; a test calls it with its own clock): disclose an
    /// outage, and when the policy says a dial is due and the gateway can take it, re-dial and re-subscribe.
    fn supervise(&self, now: Instant, wall_ms: i64) {
        {
            let mut core = self.lock();
            if core.closed {
                return;
            }
            let dead = core.any_dead();
            match core.policy.observe(now, wall_ms, dead) {
                Step::Healthy => return,
                Step::Recovered => {
                    core.disclose_live(self.sink.as_ref());
                    tracing::info!(
                        symbols = core.held.len(),
                        "ibkr price stream: the data connection is back and its quote streams are live again"
                    );
                    return;
                }
                Step::Down { opened, attempt } => {
                    core.disclose_gaps(self.sink.as_ref(), wall_ms);
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
        // The dial blocks (the gate's turn, up to the open deadline): never under the lock the caller's thread takes.
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
        let dialled = (self.dial)(Arc::clone(&self.sink), DialKind::Redial);
        let mut core = self.lock();
        if core.closed {
            // Shut down while the dial was out: give the new connection straight back.
            drop(core);
            if let Ok(mut fresh) = dialled {
                fresh.shutdown();
            }
            return;
        }
        let outcome = dialled.map_err(|why| why.to_string()).and_then(|fresh| core.install(fresh));
        match outcome {
            Ok(old) => {
                core.policy.redialled(now);
                tracing::info!(
                    symbols = core.held.len(),
                    down_s = core.policy.down_for(now).as_secs(),
                    "ibkr price stream: re-dialled; every held symbol is subscribed again (live after a short probation)"
                );
                // The dead client goes after the lock does: its release can wait on the session's.
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

impl DataClient for LazyQuotes {
    fn subscribe_bars(&mut self, _s: &str, _i: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("ibkr's price stream carries quotes only (no bars)"))
    }

    /// Start the quote stream of `symbol` (the engine's spelling, `TICKER.SMART.CCY`); a symbol already
    /// streaming answers its own id and subscribes nothing.
    fn subscribe_quotes(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        let symbol = symbol.to_ascii_uppercase();
        let id = {
            let mut core = self.shared.lock();
            if core.closed {
                return Err(LiveDataError::Subscribe("ibkr: the price stream is shut down".into()));
            }
            if let Some(held) = core.held.get(&symbol) {
                return Ok(held.id);
            }
            let cap = vike_exec::MAX_EXTRA_SYMBOLS;
            if core.held.len() >= cap {
                return Err(LiveDataError::Subscribe(format!(
                    "ibkr: at most {cap} symbols are priced at once (the market-data line limit is \
                     unmeasured); {symbol} was not subscribed"
                )));
            }
            let client = self.shared.ensure_client(&mut core, &symbol)?;
            let inner = client.subscribe_quotes(&symbol).map_err(|e| {
                LiveDataError::Subscribe(format!(
                    "ibkr: the quote stream of {symbol} was refused: {e}"
                ))
            })?;
            let id = core.fresh_id();
            core.held.insert(symbol, Held { id, inner, gap: None });
            id
        };
        self.ensure_driver();
        Ok(id)
    }

    fn subscribe_trades(&mut self, _s: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("ibkr's price stream carries quotes only (no trades)"))
    }

    fn subscribe_book(&mut self, _s: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("ibkr's price stream carries quotes only (no book)"))
    }

    fn unsubscribe(&mut self, id: SubscriptionId) {
        let mut core = self.shared.lock();
        let Some(symbol) = core.held.iter().find(|(_, held)| held.id == id).map(|(s, _)| s.clone())
        else {
            return;
        };
        let Some(held) = core.held.remove(&symbol) else { return };
        if let Some(client) = core.client.as_mut() {
            client.unsubscribe(held.inner);
        }
        // A key nobody holds any more must not leave its dead-man window open.
        close_gap(self.shared.sink.as_ref(), &symbol, held.gap);
    }

    fn begin_shutdown(&mut self) {
        self.shared.halt();
        if let Some(client) = self.shared.lock().client.as_mut() {
            client.begin_shutdown();
        }
    }

    fn shutdown(&mut self) {
        self.close();
    }
}

impl Drop for LazyQuotes {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
#[path = "quote_handle_tests.rs"]
mod quote_handle_tests;
