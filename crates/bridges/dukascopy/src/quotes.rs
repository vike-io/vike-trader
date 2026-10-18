//! The PRICE LANE's rendezvous (any-symbol, decision 0174): how a live quote of an instrument the engine was not
//! mounted on gets from the JForex sidecar to a consumer, and what stands between them.
//!
//! The sidecar is ONE child process per gatewayhub (`crate::adapter`'s one-sidecar rule), owned by the exec
//! client the trade lane starts; the consumer is the data lane's credentialed stream (`crates/vike-gatewayhub/src/data/dukascopy_quotes.rs`),
//! which opens on another thread and another schedule. They share nothing but this bus:
//!
//! * the **exec client's reader thread** attaches the sidecar's command port when its `ready` line arrives
//!   (`QuoteBus::attach`, with whether the jar announced `ticks`), hands every `tick` envelope to
//!   `QuoteBus::deliver`, and detaches at EOF. The same thread does all three, so a tick can never be seen
//!   before its attach or after its detach;
//! * the **consumer** takes a [`Listening`] (`QuoteBus::listen`) and asks prices for symbols through it
//!   ([`Listening::want`]); accepted ticks come back through the listener it handed in.
//!
//! # What the bus guarantees, and the fail-closed rules
//!
//! * **LAZY.** Nothing is subscribed at login or at `listen`; a symbol is subscribed by the first `want` and by
//!   nothing else. The sidecar logs in for the exec lane alone: this lane NEVER starts one, so a box with no
//!   armed dukascopy account has no price and says so in words ([`Listening::want`]'s refusals).
//! * **BOUNDED.** At most [`MAX_PRICED_SYMBOLS`] symbols are priced at once, held equal to the Java side's
//!   `TickLane.MAX_INSTRUMENTS` by a test. The JForex subscription limit itself is UNMEASURED.
//! * **ONE spelling.** Six ASCII letters (`EURUSD`, `XAUUSD`): the only spelling the sidecar round-trips
//!   (`crate::any_symbol`'s module doc). The door has already canonicalised; anything else is refused here.
//! * **REAL-TIME, two-sided quotes only** ([`accept`]): a tick with a missing, zero, negative or non-finite side,
//!   a crossed one, one with no time of its own, and one whose OWN time is further than [`MAX_TICK_AGE_MS`] from
//!   this box's clock are all dropped and never reach the consumer. The age bound is what keeps a cached "last
//!   tick" the SDK may replay on a fresh subscription, a backlog and a skewed clock from becoming a price: a
//!   dropped tick says so, at most once a minute, with a count.
//! * **A sidecar that goes away does not forget the wants.** They stay on the bus and are replayed to the next
//!   sidecar that attaches, so a restart of the exec lane (an account turned off and on, a respawn) resumes the
//!   prices the door is holding. Until it does, a `want` is refused in words.
//! * **An old jar is refused, not written to.** A jar whose `ready` line announces no `ticks` would log the
//!   `subscribe` command as unknown and never answer it, so the bus refuses in words instead (the same shape as
//!   the amend feature, `crate::exec`'s `amend_supported`).
//! * It discloses NO link state: a dead sidecar and a closed market look the same on this lane (silence), and the
//!   `dukascopy` row of `vike_venues::link_deadman_default` is `Inert` for exactly that reason. The consumer's
//!   price simply ages out (the tradehub's price tap bound), which is fail-closed.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use crate::proto::{Command, encode_line};

/// Shared tracing target for this module's diagnostics.
const TARGET: &str = "vike_dukascopy::quotes";

/// The READY line's feature word for the price lane (`Proto.java`'s `FEATURES`): the jar understands the
/// `subscribe` / `unsubscribe` commands and emits `tick` envelopes. A jar that does not announce it cannot be priced from.
pub const FEATURE_TICKS: &str = "ticks";

/// At most this many symbols are priced at once. Held equal to `TickLane.MAX_INSTRUMENTS` (the sidecar's own cap)
/// by `the_caps_and_the_feature_word_agree_with_the_java`. The JForex subscription limit is UNMEASURED (a live login).
pub const MAX_PRICED_SYMBOLS: usize = 8;

/// How far a tick's OWN time may be from this box's clock, either way, and still be a quote. A real-time tick
/// arrives within the network and queueing latency of its stamp; ten seconds is far above that and far below the
/// price tap's own age bound (`vike-tradehub`'s `MAX_PRICE_AGE`, 60 s), so a tick this lane lets through is
/// current by a margin the consumer does not have to re-judge. A box whose clock is off by more than this prices
/// nothing, and says why.
pub const MAX_TICK_AGE_MS: i64 = 10_000;

/// How often a run of dropped ticks is said (the first one is said at once).
const WARN_EVERY: Duration = Duration::from_secs(60);

/// One live quote the lane accepted: two-sided, uncrossed, current.
#[derive(Debug, Clone, PartialEq)]
pub struct PriceTick {
    /// Canonical symbol (`EURUSD`).
    pub symbol: String,
    pub bid: f64,
    pub ask: f64,
    /// The tick's own time, epoch ms (the venue's stamp).
    pub ts: i64,
}

/// Why a tick is not a quote ([`accept`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickRefusal {
    /// A side is missing, zero, negative or not a number.
    OneSided,
    /// The ask is below the bid.
    Crossed,
    /// The tick carries no time of its own, so nothing says it is current.
    NoTime,
    /// The tick's own time is more than [`MAX_TICK_AGE_MS`] behind this box's clock.
    Stale,
    /// The tick's own time is more than [`MAX_TICK_AGE_MS`] ahead of this box's clock.
    FromTheFuture,
}

impl std::fmt::Display for TickRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            TickRefusal::OneSided => "a side of the quote is missing or not a positive number",
            TickRefusal::Crossed => "the ask is below the bid",
            TickRefusal::NoTime => "the tick carries no time of its own",
            TickRefusal::Stale => {
                "the tick's own time is more than ten seconds behind this box's clock (a cached \
                 tick, a backlog, or a skewed clock)"
            }
            TickRefusal::FromTheFuture => {
                "the tick's own time is more than ten seconds ahead of this box's clock (a skewed clock)"
            }
        })
    }
}

/// The one judgement of a tick: a real-time, two-sided, uncrossed quote, or the reason it is not one.
///
/// # Errors
/// The [`TickRefusal`] naming the first rule the tick breaks.
pub fn accept(
    symbol: &str,
    bid: f64,
    ask: f64,
    time: i64,
    now_ms: i64,
) -> Result<PriceTick, TickRefusal> {
    if !(bid.is_finite() && ask.is_finite() && bid > 0.0 && ask > 0.0) {
        return Err(TickRefusal::OneSided);
    }
    if ask < bid {
        return Err(TickRefusal::Crossed);
    }
    if time <= 0 {
        return Err(TickRefusal::NoTime);
    }
    let age = now_ms.saturating_sub(time);
    if age > MAX_TICK_AGE_MS {
        return Err(TickRefusal::Stale);
    }
    if age < -MAX_TICK_AGE_MS {
        return Err(TickRefusal::FromTheFuture);
    }
    Ok(PriceTick { symbol: symbol.to_string(), bid, ask, ts: time })
}

/// Where accepted ticks go: called on the exec reader's thread, so it must only enqueue.
pub type TickListener = Arc<dyn Fn(&PriceTick) + Send + Sync>;

/// How a command line reaches the sidecar. The exec client implements it over the child's stdin; it is public so
/// a consumer's test (the gatewayhub's stream) can attach a recording pipe of its own.
pub trait LinePort: Send + Sync {
    /// Write one line; `false` means the sidecar is gone (and the port stays gone).
    fn send(&self, line: &str) -> bool;
}

/// The one spelling the sidecar round-trips: six ASCII letters, upper-cased.
fn canonical(symbol: &str) -> Result<String, String> {
    let up = symbol.trim().to_ascii_uppercase();
    if up.len() == 6 && up.bytes().all(|b| b.is_ascii_uppercase()) {
        Ok(up)
    } else {
        Err(format!(
            "dukascopy: `{symbol}` is not a six-letter pair like EURUSD, the one spelling the \
             sidecar round-trips, so it is not priced"
        ))
    }
}

/// The sidecar currently attached.
struct Attached {
    id: u64,
    port: Arc<dyn LinePort>,
    /// Whether its `ready` line announced [`FEATURE_TICKS`].
    ticks: bool,
}

#[derive(Default)]
struct State {
    /// The symbols a consumer asked prices for (canonical), whether or not a sidecar is up to serve them.
    wanted: BTreeSet<String>,
    sidecar: Option<Attached>,
    /// The consumer, with the token of its [`Listening`].
    listener: Option<(u64, TickListener)>,
    /// One counter for listener tokens and attach ids (never reused).
    next: u64,
    dropped: u64,
    last_warn: Option<Instant>,
}

impl State {
    /// Forget every want and return what must be told to the sidecar: its port and the symbols (none when
    /// nothing was wanted or no sidecar is up).
    fn take_wants(&mut self) -> Option<(Arc<dyn LinePort>, Vec<String>)> {
        let symbols: Vec<String> = std::mem::take(&mut self.wanted).into_iter().collect();
        let port = self.sidecar.as_ref().map(|s| Arc::clone(&s.port))?;
        (!symbols.is_empty()).then_some((port, symbols))
    }

    fn note_dropped(&mut self, symbol: &str, why: TickRefusal) {
        self.dropped += 1;
        if self.last_warn.is_none_or(|at| at.elapsed() >= WARN_EVERY) {
            self.last_warn = Some(Instant::now());
            tracing::warn!(
                target: TARGET,
                symbol,
                dropped = self.dropped,
                "dukascopy: a tick was not priced: {why} (said once a minute, with the count so far)"
            );
        }
    }
}

/// See the module doc.
pub struct QuoteBus {
    state: Mutex<State>,
    /// Serialises the commands to the sidecar (`want`, `unwant`, `attach`, `listen`, `unlisten`) so their order on
    /// the wire is their order in the state, WITHOUT holding `state` across a write: `deliver` takes only
    /// `state`, so a slow pipe never stops the reader from draining ticks. Lock order: `io`, then `state`.
    io: Mutex<()>,
}

impl QuoteBus {
    /// A bus with no sidecar and no consumer.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self { state: Mutex::new(State::default()), io: Mutex::new(()) })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn io(&self) -> MutexGuard<'_, ()> {
        self.io.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Become the consumer: `listener` receives every accepted tick. A consumer already listening is REPLACED:
    /// its wants are stopped at the sidecar, its [`Listening`] refuses from now on, and dropping it later touches nothing of
    /// the new one's.
    pub fn listen(self: &Arc<Self>, listener: TickListener) -> Listening {
        let _io = self.io();
        let (token, stale) = {
            let mut st = self.state();
            st.next += 1;
            let token = st.next;
            st.listener = Some((token, listener));
            (token, st.take_wants())
        };
        stop(stale);
        Listening { bus: Arc::clone(self), token }
    }

    /// The exec reader thread's attach, on the sidecar's `ready` line: from here `want` writes to `port`, and the
    /// wants a previous sidecar held are replayed to this one. Returns the id [`Self::detach`] takes. (Public for the
    /// exec client and for a consumer's test; nothing else calls it.)
    pub fn attach(&self, port: Arc<dyn LinePort>, ticks: bool) -> u64 {
        let _io = self.io();
        let (id, replay) = {
            let mut st = self.state();
            st.next += 1;
            let id = st.next;
            st.sidecar = Some(Attached { id, port: Arc::clone(&port), ticks });
            let replay: Vec<String> =
                if ticks { st.wanted.iter().cloned().collect() } else { Vec::new() };
            (id, replay)
        };
        if !replay.is_empty() && !port.send(&encode_line(&Command::Subscribe { symbols: replay })) {
            self.detach(id);
        }
        id
    }

    /// The reader thread's detach, at EOF (or after a failed write): a no-op unless `id` is the sidecar attached.
    /// The wants STAY, for the next sidecar.
    pub fn detach(&self, id: u64) {
        let mut st = self.state();
        if st.sidecar.as_ref().is_some_and(|s| s.id == id) {
            st.sidecar = None;
        }
    }

    /// One `tick` envelope from the sidecar: judged ([`accept`]) and, when it is a quote of a symbol still
    /// wanted, handed to the consumer (outside every lock). A symbol nobody asks for (any more) is dropped without
    /// a word: it is the ordinary race of an `unsubscribe` still in flight.
    pub fn deliver(&self, symbol: &str, bid: f64, ask: f64, time: i64, now_ms: i64) {
        let hand_over = {
            let mut st = self.state();
            if !st.wanted.contains(symbol) {
                return;
            }
            match accept(symbol, bid, ask, time, now_ms) {
                Ok(tick) => st.listener.as_ref().map(|(_, l)| (Arc::clone(l), tick)),
                Err(why) => {
                    st.note_dropped(symbol, why);
                    None
                }
            }
        };
        if let Some((listener, tick)) = hand_over {
            listener(&tick);
        }
    }

    fn want(&self, token: u64, symbol: &str) -> Result<(), String> {
        let symbol = canonical(symbol)?;
        let _io = self.io();
        let (port, id) = {
            let mut st = self.state();
            if st.listener.as_ref().map(|(t, _)| *t) != Some(token) {
                return Err("dukascopy: this price stream was replaced by a newer one".to_string());
            }
            let Some(sidecar) = st.sidecar.as_ref() else {
                return Err(format!(
                    "dukascopy: no JForex sidecar is running on this node, so `{symbol}` cannot be \
                     priced (the account is not armed at `demo`, its sidecar did not start, or it is \
                     not up yet); the price lane never starts one"
                ));
            };
            if !sidecar.ticks {
                return Err(format!(
                    "dukascopy: the installed JForex sidecar jar predates the price lane (its ready \
                     line announces no `{FEATURE_TICKS}`), so `{symbol}` cannot be priced; install \
                     the jar from this release (`just fetch-tools`) and restart"
                ));
            }
            if st.wanted.contains(&symbol) {
                return Ok(());
            }
            if st.wanted.len() >= MAX_PRICED_SYMBOLS {
                return Err(format!(
                    "dukascopy: at most {MAX_PRICED_SYMBOLS} instruments are priced at once (the \
                     JForex subscription limit is unmeasured); `{symbol}` was not subscribed"
                ));
            }
            let (port, id) = (Arc::clone(&sidecar.port), sidecar.id);
            st.wanted.insert(symbol.clone());
            (port, id)
        };
        if port.send(&encode_line(&Command::Subscribe { symbols: vec![symbol.clone()] })) {
            return Ok(());
        }
        // The pipe is closed: the sidecar is gone, and nothing the wants hold is being served.
        self.state().wanted.remove(&symbol);
        self.detach(id);
        Err(format!(
            "dukascopy: the JForex sidecar is gone (its command pipe is closed), so `{symbol}` was \
             not subscribed"
        ))
    }

    fn unwant(&self, token: u64, symbol: &str) {
        let Ok(symbol) = canonical(symbol) else { return };
        let _io = self.io();
        let port = {
            let mut st = self.state();
            if st.listener.as_ref().map(|(t, _)| *t) != Some(token) || !st.wanted.remove(&symbol) {
                return;
            }
            st.sidecar.as_ref().map(|s| Arc::clone(&s.port))
        };
        if let Some(port) = port {
            let _ = port.send(&encode_line(&Command::Unsubscribe { symbols: vec![symbol] }));
        }
    }

    fn unlisten(&self, token: u64) {
        let _io = self.io();
        let stale = {
            let mut st = self.state();
            if st.listener.as_ref().map(|(t, _)| *t) != Some(token) {
                return;
            }
            st.listener = None;
            st.take_wants()
        };
        stop(stale);
    }
}

/// Tell the sidecar to stop the ticks of `wants` (best effort: a gone sidecar has nothing to stop).
fn stop(wants: Option<(Arc<dyn LinePort>, Vec<String>)>) {
    if let Some((port, symbols)) = wants {
        let _ = port.send(&encode_line(&Command::Unsubscribe { symbols }));
    }
}

/// The consumer's hold on the bus: the only way to ask for a price, and the owner of what was asked. Dropping it
/// (the stream closing) stops every price it asked for and removes the listener.
pub struct Listening {
    bus: Arc<QuoteBus>,
    token: u64,
}

impl Listening {
    /// Ask live quotes for `symbol` (six letters, any case). Idempotent for a symbol already asked.
    ///
    /// # Errors
    /// In words: the spelling is not a six-letter pair, no sidecar is running, the installed jar predates the
    /// price lane, the cap of [`MAX_PRICED_SYMBOLS`] is reached, the sidecar is gone, or this stream was replaced.
    pub fn want(&self, symbol: &str) -> Result<(), String> {
        self.bus.want(self.token, symbol)
    }

    /// Stop the quotes of `symbol` (a symbol not asked for is a no-op).
    pub fn unwant(&self, symbol: &str) {
        self.bus.unwant(self.token, symbol);
    }
}

impl Drop for Listening {
    fn drop(&mut self) {
        self.bus.unlisten(self.token);
    }
}

/// The process's ONE price lane: the exec client the gatewayhub's trade lane starts attaches its sidecar here
/// (`crate::adapter`'s `start`), and the data lane's credentialed stream listens here. Process-wide because the
/// two are started by different parts of the daemon that share nothing else, and the sidecar itself is
/// process-wide (one per process). A test builds a [`QuoteBus::new`] of its own instead.
pub fn process_bus() -> Arc<QuoteBus> {
    static BUS: OnceLock<Arc<QuoteBus>> = OnceLock::new();
    Arc::clone(BUS.get_or_init(QuoteBus::new))
}

#[cfg(test)]
#[path = "quotes_tests.rs"]
mod quotes_tests;
