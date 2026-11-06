//! `strat` - the STRATEGY LANE of the data daemon: the tradehub's price path (decision
//! `docs/decisions/0164-one-daemon-owns-every-venue-connection-vike-gatewayhub.md`, verdict 2, and the
//! tube `docs/decisions/0167-one-live-data-tube-vike-market-stream-on-a-data-plane-tier.md`).
//!
//! # What it is
//!
//! A tradehub dials this daemon under [`vike_hub_proto::auth::Scope::Trade`] (its own key,
//! `VIKE_DATAHUB_TRADE_KEY`), opens one stream per venue with `Request::StratOpen`, and from the reply
//! on speaks the TUBE's frames (`vike_market_stream::wire`): its `Sub`s arrive in-band, each answered
//! `SubOk` / `SubErr`, and the venue's `quote` / `trade` / `book` / `seed_bars` ... verbs come back as
//! the same frames a gatewayhub stream carries. It is the gatewayhub's `DataOpen` session on the datahub:
//! the same tube, a different front door, and the same two contracts the tube holds - a sink call on the
//! venue side is lock-and-enqueue and nothing slower, and a loss is a disclosed `Gap`, never a silent hole.
//!
//! # Where it sits
//!
//! * [`StratHub`] owns the lane's bookkeeping: the session slots, the lane's own key caps, the venues
//!   it serves and, when the build has a pump, the tick-pump client table.
//! * Each SESSION (`session.rs`) is one tube sender: a `WireSink` (the bounded queue), a writer thread
//!   and the reader loop on the connection thread. It subscribes through a
//!   [`crate::feeds::FeedHandle`] held as [`crate::feeds::Holder::Strat`], so a key the market-data hub
//!   or the recorder already holds is subscribed ONCE.
//! * [`sink::StratSink`] is the sink the broker delivers into, and fans each frame to the sessions that
//!   hold its key.
//! * [`history::StartHistory`] is the START HISTORY of a strategy (`history.rs`'s module doc is the authority):
//!   the closed bars of the keys the sessions hold on a saved venue are written to the store as they close,
//!   the venue's own seed is compared with the store (one journal line), and, behind
//!   `flags.start_history_from_store`, a bar subscription is preloaded from the store before the venue is
//!   asked. Attached by the composition root; absent, the lane is what it was.
//! * The tick PUMP is the one lane that does not go through the broker: it is not a `DataClient` verb
//!   (the tube carries it as `SubOp::Ticks`), a pump's frames are one tradehub's sized to its own grid,
//!   and so each session builds its own pump client ([`TicksBuilder`]) delivering straight into the
//!   session's wire sink.
//!
//! # What it refuses, and what fails closed
//!
//! * **No key, no lane.** The hub is mounted only by a datahub that holds the strategy-lane key
//!   (`datahub_cli`'s `start_strategy_lane`), and the verb is refused on a key-less server whatever is
//!   mounted (`crate::server`'s `strat_open_verb`). `Welcome` advertises the lane only then.
//! * **A slow tradehub never blocks a venue pump or the md plane.** The queue is bounded in bytes and
//!   frames; overflow evicts the oldest lossless frame of the pushing key and owes a `Gap`; a session
//!   that keeps lapsing is ended with `Bye(TooSlow)` and the client reconnects and replays. A lapse is a
//!   `Disconnected` for the consumer's dead-man, which is why [`limits`] holds the write timeout and
//!   the heartbeat under the dead-man's grace.
//! * **Its keys are its own.** They draw on [`limits::SL_MAX_KEYS_TOTAL`] and
//!   [`limits::SL_MAX_KEYS_PER_VENUE`] and never on the market-data plane's caps.
//!
//! The numbers are [`limits`]'; the protocol's own (frame ceilings, symbol guard, heartbeat range) are
//! `vike_datahub_client::proto`'s `SL_*`.

pub mod history;
pub mod limits;
mod session;
pub(crate) mod sink;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use vike_data::LiveDataSink;
use vike_datahub_client::proto::{STRAT_NETWORK_MAINNET, STRAT_NETWORK_TESTNET};
use vike_market_stream::market::{CAP_MARK, RemoteDataClient};
use vike_market_stream::wire::SubErrKind;

use crate::feeds::venues::NETWORK_VENUE;
use crate::feeds::{FeedBroker, Holder};

use history::StartHistory;
pub use limits::StratLimits;
pub(crate) use session::serve;
use sink::StratSink;

/// How the hub obtains one session's tick-pump client: the venue and the session's own wire sink. The
/// production table is `crate::feeds::venues::real_ticks_table`; a test hands one over a fake peer.
pub type TicksBuilder =
    Box<dyn Fn(&str, Arc<dyn LiveDataSink>) -> Result<RemoteDataClient, String> + Send + Sync>;

/// The lane. Cheap to share (an `Arc`); built once by the composition root.
pub struct StratHub {
    broker: Arc<FeedBroker>,
    sink: Arc<StratSink>,
    served: Vec<String>,
    ticks: Option<TicksBuilder>,
    limits: StratLimits,
    sessions: AtomicUsize,
    next_session: AtomicU64,
    keys: Mutex<KeyBook>,
    /// Open sessions per route (the broker's client label): a session's `begin_shutdown` reaches the
    /// shared venue client only when it is the route's SOLE session ([`StratHub::sole_on_route`]).
    routes: Mutex<HashMap<String, usize>>,
    /// The start-history holder ([`history`]), attached once at startup by a process that has a store to
    /// save into. `None` is the lane exactly as it was: nothing is saved, compared or preloaded.
    history: OnceLock<Arc<StartHistory>>,
}

impl std::fmt::Debug for StratHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StratHub")
            .field("served", &self.served)
            .field("ticks", &self.ticks.is_some())
            .finish_non_exhaustive()
    }
}

/// The lane's key accounting: how many sessions hold each `(venue, symbol)`, so the caps count DISTINCT
/// keys and not subscriptions, and a key held by two sessions is one key.
#[derive(Default)]
struct KeyBook {
    refs: HashMap<String, HashMap<String, usize>>,
    total: usize,
}

impl StratHub {
    /// A hub over `broker`, serving `served` venues, with `ticks` as its pump table (`None`: this build
    /// has no pump and refuses the `Ticks` lane by name). Attaches the lane's sink to the broker as
    /// [`Holder::Strat`]; a second hub on the same broker would be ignored by the broker's own rule
    /// (one sink per holder), so a process builds one.
    pub fn new(
        broker: Arc<FeedBroker>,
        served: Vec<String>,
        ticks: Option<TicksBuilder>,
        limits: StratLimits,
    ) -> Arc<Self> {
        let sink = Arc::new(StratSink::default());
        broker.attach_sink(Holder::Strat, Arc::clone(&sink) as Arc<dyn LiveDataSink>);
        Arc::new(StratHub {
            broker,
            sink,
            served,
            ticks,
            limits,
            sessions: AtomicUsize::new(0),
            next_session: AtomicU64::new(1),
            keys: Mutex::new(KeyBook::default()),
            routes: Mutex::new(HashMap::new()),
            history: OnceLock::new(),
        })
    }

    /// Attach the start-history holder (once; a second call is ignored): from then on the lane saves the
    /// closed bars of the keys its sessions hold on a saved venue, compares the venue's seed with the store,
    /// and, if the holder serves, preloads a bar subscription's start history before the venue is asked.
    /// Mounted by `datahub_cli/lanes.rs`'s `arm_start_history`, only in a process that compacts its store.
    pub fn attach_start_history(&self, history: Arc<StartHistory>) {
        self.sink.attach_history(Arc::clone(&history));
        let _ = self.history.set(history);
    }

    /// The attached start-history holder, if any.
    pub(crate) fn history(&self) -> Option<Arc<StartHistory>> {
        self.history.get().cloned()
    }

    /// The venues the lane serves, in advertisement order.
    pub fn served_venues(&self) -> &[String] {
        &self.served
    }

    /// Whether this hub serves the tick pump lane.
    pub fn serves_ticks(&self) -> bool {
        self.ticks.is_some()
    }

    /// The numbers this hub runs with.
    pub fn limits(&self) -> &StratLimits {
        &self.limits
    }

    /// How many sessions are open right now.
    pub fn sessions(&self) -> usize {
        self.sessions.load(Ordering::Acquire)
    }

    /// Whether `route` has no other open session than the caller's: the only case in which a session's
    /// `begin_shutdown` may drain the venue client it shares. All strategy sessions are ONE holder to the
    /// broker, which cannot tell them apart, so one session's shutdown (its end, a hot reload) would
    /// refuse the subscribes of another on the same client until real subscriptions dropped to zero.
    pub(crate) fn sole_on_route(&self, route: &str) -> bool {
        self.routes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(route)
            .is_none_or(|n| *n <= 1)
    }

    /// Reserve a session slot for `venue` (and `network`), or say why not. The reservation is the
    /// returned guard: it is TAKEN at the door, before the mode switch, so a refusal is a positional
    /// `Response::Error` on a connection that stays usable, and a guard dropped unused frees its slot
    /// (`crate::md::MdHub::open_session`'s shape).
    ///
    /// The refusals, in the order a caller can act on them: a venue this build does not serve (the
    /// served list is named), a `network` the venue does not take or this daemon cannot open, and a
    /// full lane (transient).
    pub fn open_session(
        self: &Arc<Self>,
        venue: &str,
        network: Option<&str>,
    ) -> Result<StratGuard, String> {
        if !self.served.iter().any(|v| v == venue) {
            return Err(format!(
                "strategy lane: no stream is served for venue `{venue}` (this datahub's Welcome \
                 advertises strat_venue= for: [{}]); nothing was opened and this connection is unchanged",
                self.served.join(", ")
            ));
        }
        let network = check_network(venue, network)?;
        let cap = self.limits.max_stream_conns;
        let mut held = self.sessions.load(Ordering::Acquire);
        loop {
            if held >= cap {
                return Err(format!(
                    "strategy lane: already serving {cap} streams (the lane's connection cap); \
                     {CAP_MARK}: raise the cap or close a stream"
                ));
            }
            match self.sessions.compare_exchange(
                held,
                held + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(now) => held = now,
            }
        }
        let id = self.next_session.fetch_add(1, Ordering::Relaxed);
        let label = crate::feeds::venues::client_label(venue, network);
        *self.routes.lock().unwrap_or_else(PoisonError::into_inner).entry(label).or_insert(0) += 1;
        Ok(StratGuard {
            hub: Arc::clone(self),
            id,
            venue: venue.to_string(),
            network: network.map(str::to_string),
        })
    }

    /// Count `(venue, symbol)` against the lane's caps. `Ok` when the key is already held by another
    /// session (it is one key) or there is room; a `Budget` naming the cap that is full otherwise.
    fn reserve_key(&self, venue: &str, symbol: &str) -> Result<(), SubErrKind> {
        let mut book = self.keys.lock().unwrap_or_else(PoisonError::into_inner);
        let held_on_venue = book.refs.get(venue).map_or(0, HashMap::len);
        let already = book.refs.get(venue).and_then(|m| m.get(symbol)).is_some();
        if !already {
            if book.total >= self.limits.max_keys_total {
                return Err(budget("strategy keys", book.total, self.limits.max_keys_total));
            }
            if held_on_venue >= self.limits.max_keys_per_venue {
                return Err(budget(
                    "strategy keys on this venue",
                    held_on_venue,
                    self.limits.max_keys_per_venue,
                ));
            }
            book.total += 1;
        }
        *book.refs.entry(venue.to_string()).or_default().entry(symbol.to_string()).or_default() +=
            1;
        Ok(())
    }

    /// Give back one session's hold on `(venue, symbol)`.
    fn release_key(&self, venue: &str, symbol: &str) {
        let mut book = self.keys.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(by_symbol) = book.refs.get_mut(venue) else { return };
        let Some(n) = by_symbol.get_mut(symbol) else { return };
        *n -= 1;
        if *n == 0 {
            by_symbol.remove(symbol);
            if by_symbol.is_empty() {
                book.refs.remove(venue);
            }
            book.total -= 1;
        }
    }

    /// Distinct keys held right now (the tests' witness).
    pub fn keys_held(&self) -> usize {
        self.keys.lock().unwrap_or_else(PoisonError::into_inner).total
    }
}

fn budget(scope: &str, held: usize, cap: usize) -> SubErrKind {
    SubErrKind::Budget {
        scope: scope.to_string(),
        held: u32::try_from(held).unwrap_or(u32::MAX),
        cap: u32::try_from(cap).unwrap_or(u32::MAX),
    }
}

/// Whether `network` is one this daemon opens for `venue`, and the network the session is ON when it
/// is not the venue's main one (`None`: the main network, which is what a request naming no network
/// and a request naming `mainnet` both get - ONE client, as before).
///
/// Only Hyperliquid's feed follows the account tier, so it is the only venue that takes a network;
/// any other venue given one is refused rather than ignored (a silently ignored network would put a
/// testnet strategy on a mainnet feed). Hyperliquid's test network is served as its OWN client:
/// the broker keys the strategy lane's clients by [`crate::feeds::venues::client_label`] (venue and
/// network), and the gatewayhub is asked for the network on `DataOpen`.
///
/// ⚠ **No network word is the MAIN network, here and at the gatewayhub** (`(_, None)` below;
/// `vike-gatewayhub`'s `build_client`, which has no network parameter and builds the feed's default,
/// `Network::Mainnet`). The bridge maps a `demo` AND a `paper` account to the TESTNET
/// (`Network::for_tier`), so a caller that builds a Hyperliquid feed for a demo account and forgets to name
/// the network is fed MAINNET prices without a word said: the tradehub names it for every Hyperliquid
/// feed (`HlNetwork::strat_network`), and a new caller must do the same.
fn check_network<'n>(venue: &str, network: Option<&'n str>) -> Result<Option<&'n str>, String> {
    match (venue == NETWORK_VENUE, network) {
        (_, None) => Ok(None),
        (false, Some(n)) => Err(format!(
            "strategy lane: venue `{venue}` takes no network (asked for `{n}`); only {NETWORK_VENUE} \
             has a feed that follows the account tier. Nothing was opened"
        )),
        (true, Some(n)) if n == STRAT_NETWORK_MAINNET => Ok(None),
        (true, Some(n)) if n == STRAT_NETWORK_TESTNET => Ok(Some(n)),
        (true, Some(n)) => Err(format!(
            "strategy lane: unknown network `{n}` for {NETWORK_VENUE} (the words are \
             `{STRAT_NETWORK_MAINNET}` and `{STRAT_NETWORK_TESTNET}`). Nothing was opened"
        )),
    }
}

/// One reserved session slot. Dropping it frees the slot, however the session ended (including a
/// panic): it rides the mode switch into the session and is dropped last.
#[derive(Debug)]
pub struct StratGuard {
    hub: Arc<StratHub>,
    id: u64,
    venue: String,
    /// The network the session is on when it is not the venue's main one.
    network: Option<String>,
}

impl StratGuard {
    /// The session's id.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// The venue the session serves.
    pub fn venue(&self) -> &str {
        &self.venue
    }

    /// The network the session is on when it is not the venue's main one.
    pub fn network(&self) -> Option<&str> {
        self.network.as_deref()
    }

    /// The name the broker and the lane's sink know this session's client by: the venue, and its
    /// network when it is not the main one ([`crate::feeds::venues::client_label`]).
    pub fn label(&self) -> String {
        crate::feeds::venues::client_label(&self.venue, self.network())
    }
}

impl Drop for StratGuard {
    fn drop(&mut self) {
        let mut routes = self.hub.routes.lock().unwrap_or_else(PoisonError::into_inner);
        let label = self.label();
        if let Some(n) = routes.get_mut(&label) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                routes.remove(&label);
            }
        }
        drop(routes);
        self.hub.sessions.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
#[path = "strat_tests.rs"]
mod strat_tests;
