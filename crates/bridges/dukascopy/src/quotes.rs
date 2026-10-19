//! The PRICE LANE's rendezvous (any-symbol, decision 0174): how a live quote of an instrument the engine was not
//! mounted on gets from the JForex sidecar to a consumer, and what stands between them.
//!
//! The sidecar is ONE child process per gatewayhub (`crate::adapter`'s one-sidecar rule), owned by the exec
//! client the trade lane starts; the consumer is the data lane's credentialed stream (`crates/vike-gatewayhub/src/data/dukascopy_quotes.rs`),
//! which opens on another thread and another schedule. They share nothing but this bus:
//!
//! * the **exec client's reader thread** attaches the sidecar's command port when its `ready` line arrives
//!   (`QuoteBus::attach`, with the feature words the jar announced), hands every `tick` envelope to
//!   `QuoteBus::deliver` and every `subscription` answer to `QuoteBus::subscription`, and detaches at EOF. The same
//!   thread does all of it, so a tick can never be seen before its attach or after its detach;
//! * the **consumer** takes a [`Listening`] (`QuoteBus::listen`) and asks prices for symbols through it
//!   ([`Listening::want`]); accepted ticks come back through the listener it handed in, on a thread of the bus's
//!   own (below).
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
//! * **A consumer fault can never stop the reader's other work** (fills, positions, the in-flight drain). An
//!   accepted tick goes into a BOUNDED queue (`try_send`: never blocks, never grows) that a thread of the bus's own
//!   drains into the listener. A listener that stalls fills its queue and the surplus ticks are DROPPED and counted
//!   (said at most once a minute); one that panics is caught, logged at `error` and detached (its wants stopped at
//!   the sidecar), and the reader never notices either.
//! * **One consumer at a time, and a takeover is loud.** A second `listen` replaces the first (a redialling tradehub
//!   needs exactly that) and says so at `warn`, naming both streams and what the older one held; the replaced
//!   [`Listening`] refuses from then on ([`Listening::ensure_current`]), it is never answered as if it still priced.
//! * **A decline is heard.** A jar that announces `tick-acks` answers each symbol of a `subscribe` with a
//!   `subscription` line; [`Listening::want`] waits a bounded time for it, hands a decline back as the refusal and
//!   frees the slot it had taken. A jar without the word, or one that does not answer in time, behaves as it always did
//!   (the want stands), the second with one `warn`.
//! * **Silence is said.** A wanted symbol that sends no tick for `SILENT_AFTER` while the sidecar is up is warned
//!   about once (a closed market, or a subscription the SDK lost across a reconnect: the jar re-asserts its
//!   subscriptions itself after one, `Bridge.java`'s `onDisconnect`), and a tick re-arms it.

use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::{Duration, Instant};

use crate::proto::{Command, encode_line};

/// Shared tracing target for this module's diagnostics.
const TARGET: &str = "vike_dukascopy::quotes";

/// The READY line's feature word for the price lane (`Proto.java`'s `FEATURES`): the jar understands the
/// `subscribe` / `unsubscribe` commands and emits `tick` envelopes. A jar that does not announce it cannot be priced from.
pub const FEATURE_TICKS: &str = "ticks";

/// The READY line's feature word for the answers to a `subscribe` (`Proto.java`'s `FEATURES`): the jar writes one
/// `subscription` envelope per requested symbol, `ok` or a decline with its reason. A jar that does not announce it
/// is never waited for: its `subscribe` is fire-and-forget, as it was before the word existed.
pub const FEATURE_TICK_ACKS: &str = "tick-acks";

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

/// How many accepted ticks may wait for the consumer before the surplus is dropped. A tick is a few dozen bytes and
/// eight pairs tick a few times a second each, so a consumer would have to stall for minutes to fill it.
const LISTENER_QUEUE: usize = 1024;

/// How long [`Listening::want`] waits for the jar's answer to a `subscribe` (only for a jar that announced
/// [`FEATURE_TICK_ACKS`]). The answer is normally a few milliseconds away; the bound exists because the jar's strategy
/// thread can be held for seconds by an order waiting for its first tick, and a price request must not hang behind it.
const ACK_WAIT: Duration = Duration::from_secs(5);

/// How long a wanted symbol may send no tick, with the sidecar up, before it is warned about (once). Majors tick
/// several times a second; two minutes is far past any quiet hour of a pair this lane admits, and still prompt
/// against the price tap's 60 s bound the consumer already ages a quote out at.
const SILENT_AFTER: Duration = Duration::from_secs(120);

/// How often the silence of the wanted symbols is looked at.
const SILENCE_CHECK_EVERY: Duration = Duration::from_secs(10);

/// What a replaced (or faulted) consumer is told when it asks for anything.
const NOT_CURRENT: &str = "dukascopy: this price stream was replaced by a newer one (or ended after a fault in \
                           its consumer, see the log), so it prices nothing from now on";

/// The bus's bounds and clocks, a value so a test can shorten them.
#[derive(Clone, Copy)]
struct Tuning {
    queue: usize,
    ack_wait: Duration,
    silent_after: Duration,
    check_every: Duration,
}

impl Tuning {
    const PRODUCTION: Self = Self {
        queue: LISTENER_QUEUE,
        ack_wait: ACK_WAIT,
        silent_after: SILENT_AFTER,
        check_every: SILENCE_CHECK_EVERY,
    };
}

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

/// Where accepted ticks go: called on a thread of the bus's own, one tick at a time and in order, never on the exec
/// reader's. It should still be cheap (a slow one lets its queue fill, and the surplus ticks are dropped), and a
/// panic in it detaches the listener (the module doc's "consumer fault").
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
    /// Whether it also announced [`FEATURE_TICK_ACKS`] (only meaningful with `ticks`): it answers a `subscribe`.
    acks: bool,
}

/// The consumer: the token of its [`Listening`] and the way in to the thread that drains its ticks. Dropping it
/// (a replacement, an `unlisten`, a fault) closes the queue, which ends that thread once the queue is empty.
struct Consumer {
    token: u64,
    queue: SyncSender<PriceTick>,
}

/// When a wanted symbol last sent a tick (or was asked for, or its sidecar attached), and whether its silence has
/// been said.
struct Quiet {
    since: Instant,
    warned: bool,
}

impl Quiet {
    fn now() -> Self {
        Self { since: Instant::now(), warned: false }
    }
}

#[derive(Default)]
struct State {
    /// The symbols a consumer asked prices for (canonical), whether or not a sidecar is up to serve them.
    wanted: BTreeSet<String>,
    /// The wanted symbols whose `subscribe` the sidecar has not answered yet, for a sidecar that answers.
    awaiting: BTreeSet<String>,
    /// The reasons of declines that landed while a `want` was waiting for its answer (taken by that `want`).
    declined: BTreeMap<String, String>,
    /// The silence clock of each wanted symbol.
    quiet: BTreeMap<String, Quiet>,
    sidecar: Option<Attached>,
    listener: Option<Consumer>,
    /// One counter for listener tokens and attach ids (never reused).
    next: u64,
    dropped: u64,
    last_warn: Option<Instant>,
    /// Ticks the consumer's full queue turned away.
    overflowed: u64,
    last_overflow_warn: Option<Instant>,
}

impl State {
    /// Forget every want (and everything kept about one) and return the symbols that were held.
    fn drop_wants(&mut self) -> Vec<String> {
        self.awaiting.clear();
        self.declined.clear();
        self.quiet.clear();
        std::mem::take(&mut self.wanted).into_iter().collect()
    }

    /// Forget every want and return what must be told to the sidecar: its port and the symbols (none when
    /// nothing was wanted or no sidecar is up).
    fn take_wants(&mut self) -> Option<(Arc<dyn LinePort>, Vec<String>)> {
        let symbols = self.drop_wants();
        let port = self.sidecar.as_ref().map(|s| Arc::clone(&s.port))?;
        (!symbols.is_empty()).then_some((port, symbols))
    }

    /// Forget one want (and what is kept about it).
    fn drop_want(&mut self, symbol: &str) {
        self.wanted.remove(symbol);
        self.awaiting.remove(symbol);
        self.quiet.remove(symbol);
    }

    /// A tick of `symbol` arrived: it is alive, and a silence already said is over.
    fn heard(&mut self, symbol: &str) {
        if let Some(quiet) = self.quiet.get_mut(symbol) {
            if quiet.warned {
                tracing::info!(
                    target: TARGET,
                    symbol,
                    silent_secs = quiet.since.elapsed().as_secs(),
                    "dukascopy: ticks resumed after a silence that was warned about"
                );
            }
            *quiet = Quiet::now();
        }
    }

    fn note_overflow(&mut self, symbol: &str) {
        self.overflowed += 1;
        if self.last_overflow_warn.is_none_or(|at| at.elapsed() >= WARN_EVERY) {
            self.last_overflow_warn = Some(Instant::now());
            tracing::warn!(
                target: TARGET,
                symbol,
                overflowed = self.overflowed,
                "dukascopy: the price consumer is not keeping up: its queue is full, so ticks are \
                 DROPPED (said once a minute, with the count so far)"
            );
        }
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
    /// Signalled (with `state`) when a `subscription` answer lands, a sidecar detaches or the wants are dropped: what a
    /// `want` waiting for its answer sleeps on.
    answered: Condvar,
    tuning: Tuning,
}

impl QuoteBus {
    /// A bus with no sidecar and no consumer.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Self::tuned(Tuning::PRODUCTION)
    }

    fn tuned(tuning: Tuning) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::default()),
            io: Mutex::new(()),
            answered: Condvar::new(),
            tuning,
        })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn io(&self) -> MutexGuard<'_, ()> {
        self.io.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Become the consumer: `listener` receives every accepted tick, on a thread of the bus's own. A consumer already
    /// listening is REPLACED, loudly (`warn`, naming both): its wants are stopped at the sidecar, its [`Listening`]
    /// refuses from now on, and dropping it later touches nothing of the new one's.
    pub fn listen(self: &Arc<Self>, listener: TickListener) -> Listening {
        let _io = self.io();
        let (queue, inbox) = sync_channel(self.tuning.queue);
        let (token, displaced, held, stale) = {
            let mut st = self.state();
            st.next += 1;
            let token = st.next;
            let displaced = st.listener.replace(Consumer { token, queue }).map(|c| c.token);
            let held: Vec<String> = st.wanted.iter().cloned().collect();
            (token, displaced, held, st.take_wants())
        };
        let (bus, check_every) = (Arc::downgrade(self), self.tuning.check_every);
        std::thread::Builder::new()
            .name("duka-quote-feed".into())
            .spawn(move || run_consumer(token, &inbox, &listener, &bus, check_every))
            .expect("spawn quote delivery thread");
        if let Some(older) = displaced {
            tracing::warn!(
                target: TARGET,
                older,
                newer = token,
                ?held,
                "dukascopy: price stream #{token} took over the JForex price lane from stream #{older}: \
                 the older stream's prices (the pairs listed) stop NOW and it is told so on its next \
                 ask. A redialled tradehub replaces its own earlier session this way; a SECOND tradehub \
                 on this gatewayhub loses its prices to the first"
            );
        }
        stop(stale);
        self.answered.notify_all(); // a `want` of the replaced stream still waiting gives up
        Listening { bus: Arc::clone(self), token }
    }

    /// The exec reader thread's attach, on the sidecar's `ready` line: from here `want` writes to `port`, and the
    /// wants a previous sidecar held are replayed to this one. `features` is what its `ready` line announced
    /// ([`FEATURE_TICKS`], [`FEATURE_TICK_ACKS`]). Returns the id [`Self::detach`] takes. (Public for the
    /// exec client and for a consumer's test; nothing else calls it.)
    pub fn attach(&self, port: Arc<dyn LinePort>, features: &[String]) -> u64 {
        let ticks = features.iter().any(|f| f == FEATURE_TICKS);
        let acks = ticks && features.iter().any(|f| f == FEATURE_TICK_ACKS);
        let _io = self.io();
        let (id, replay) = {
            let mut st = self.state();
            st.next += 1;
            let id = st.next;
            st.sidecar = Some(Attached { id, port: Arc::clone(&port), ticks, acks });
            // A new sidecar starts every silence clock over, and its replay is not waited for.
            st.awaiting.clear();
            for quiet in st.quiet.values_mut() {
                *quiet = Quiet::now();
            }
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
            st.awaiting.clear();
            drop(st);
            self.answered.notify_all(); // a `want` waiting on this sidecar stops waiting
        }
    }

    /// One `tick` envelope from the sidecar: judged ([`accept`]) and, when it is a quote of a symbol still
    /// wanted, queued for the consumer (`try_send`: the reader never waits for the consumer, and a full queue
    /// drops the tick and counts it). A symbol nobody asks for (any more) is dropped without
    /// a word: it is the ordinary race of an `unsubscribe` still in flight.
    pub fn deliver(&self, symbol: &str, bid: f64, ask: f64, time: i64, now_ms: i64) {
        let queued = {
            let mut st = self.state();
            if !st.wanted.contains(symbol) {
                return;
            }
            st.heard(symbol);
            match accept(symbol, bid, ask, time, now_ms) {
                Ok(tick) => st.listener.as_ref().map(|c| (c.queue.clone(), tick)),
                Err(why) => {
                    st.note_dropped(symbol, why);
                    None
                }
            }
        };
        if let Some((queue, tick)) = queued {
            match queue.try_send(tick) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => self.state().note_overflow(symbol),
                // The consumer's thread is gone (it panicked and is detaching itself): the tick has no taker.
                Err(TrySendError::Disconnected(_)) => {}
            }
        }
    }

    /// One `subscription` answer from the sidecar (a jar that announced [`FEATURE_TICK_ACKS`]): an `ok` ends a
    /// `want`'s wait; a decline frees the place the want took, is said at `warn` with the jar's reason, and is handed
    /// to the `want` still waiting for it (a late one, after the wait gave up, is only said and freed).
    pub fn subscription(&self, symbol: &str, ok: bool, reason: &str) {
        let mut st = self.state();
        let waited = st.awaiting.remove(symbol);
        if !ok && st.wanted.contains(symbol) {
            st.drop_want(symbol);
            if waited {
                st.declined.insert(symbol.to_string(), reason.to_string());
            }
            tracing::warn!(
                target: TARGET,
                symbol,
                reason,
                "dukascopy: the JForex sidecar declined to price {symbol}: {reason} (the place it took is free)"
            );
        }
        drop(st);
        self.answered.notify_all();
    }

    /// Warn, once per silence, about each wanted symbol with no tick for `silent_after` while a ticking sidecar is
    /// attached; returns the symbols warned about. The consumer's thread calls it every `check_every`.
    fn warn_silent(&self) -> Vec<String> {
        let silent: Vec<(String, u64)> = {
            let mut st = self.state();
            if !st.sidecar.as_ref().is_some_and(|s| s.ticks) {
                return Vec::new();
            }
            let after = self.tuning.silent_after;
            st.quiet
                .iter_mut()
                .filter(|(_, q)| !q.warned && q.since.elapsed() >= after)
                .map(|(symbol, q)| {
                    q.warned = true;
                    (symbol.clone(), q.since.elapsed().as_secs())
                })
                .collect()
        };
        for (symbol, secs) in &silent {
            tracing::warn!(
                target: TARGET,
                symbol = %symbol,
                silent_secs = *secs,
                "dukascopy: no tick of {symbol} for {secs} s while the JForex sidecar is up: a closed \
                 market, or a price subscription the SDK lost (the sidecar re-asserts its subscriptions \
                 after a reconnect; if this persists in an open market, restart the gatewayhub). Said \
                 once; a tick ends it"
            );
        }
        silent.into_iter().map(|(symbol, _)| symbol).collect()
    }

    fn want(&self, token: u64, symbol: &str) -> Result<(), String> {
        let symbol = canonical(symbol)?;
        match self.send_subscribe(token, &symbol)? {
            Sent::Answering => self.await_answer(token, &symbol),
            Sent::NotAnswering => Ok(()),
        }
    }

    /// The first half of a `want`: validate, take the place, write the `subscribe` (all under `io`, released on
    /// return so the wait for the answer holds nothing but the state lock the condvar gives up).
    fn send_subscribe(&self, token: u64, symbol: &str) -> Result<Sent, String> {
        let symbol = symbol.to_string();
        let _io = self.io();
        let (port, id, acks) = {
            let mut st = self.state();
            if st.listener.as_ref().map(|c| c.token) != Some(token) {
                return Err(NOT_CURRENT.to_string());
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
                return Ok(Sent::NotAnswering);
            }
            if st.wanted.len() >= MAX_PRICED_SYMBOLS {
                return Err(format!(
                    "dukascopy: at most {MAX_PRICED_SYMBOLS} instruments are priced at once (the \
                     JForex subscription limit is unmeasured); `{symbol}` was not subscribed"
                ));
            }
            let (port, id, acks) = (Arc::clone(&sidecar.port), sidecar.id, sidecar.acks);
            st.wanted.insert(symbol.clone());
            st.quiet.insert(symbol.clone(), Quiet::now());
            st.declined.remove(&symbol);
            if acks {
                st.awaiting.insert(symbol.clone());
            }
            (port, id, acks)
        };
        if port.send(&encode_line(&Command::Subscribe { symbols: vec![symbol.clone()] })) {
            return Ok(if acks { Sent::Answering } else { Sent::NotAnswering });
        }
        // The pipe is closed: the sidecar is gone, and nothing the wants hold is being served.
        self.state().drop_want(&symbol);
        self.detach(id);
        Err(format!(
            "dukascopy: the JForex sidecar is gone (its command pipe is closed), so `{symbol}` was \
             not subscribed"
        ))
    }

    /// The second half of a `want` on a sidecar that answers: wait (bounded) for its `subscription` line. A decline is
    /// the refusal; an `ok`, or the want having been dropped meanwhile, is `Ok`; a stream replaced meanwhile is told
    /// so; and no answer in time is `Ok` too, with one `warn`: the want stands, exactly as it does for a jar that never
    /// answers (a late `ok` changes nothing, a late decline frees the place).
    fn await_answer(&self, token: u64, symbol: &str) -> Result<(), String> {
        let deadline = Instant::now() + self.tuning.ack_wait;
        let mut st = self.state();
        loop {
            if st.listener.as_ref().map(|c| c.token) != Some(token) {
                return Err(NOT_CURRENT.to_string());
            }
            if let Some(reason) = st.declined.remove(symbol) {
                return Err(format!(
                    "dukascopy: the JForex sidecar declined to price `{symbol}`: {reason}"
                ));
            }
            if !st.awaiting.contains(symbol) {
                return Ok(());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                st.awaiting.remove(symbol);
                tracing::warn!(
                    target: TARGET,
                    symbol,
                    waited_secs = self.tuning.ack_wait.as_secs(),
                    "dukascopy: the JForex sidecar did not answer the subscription of {symbol} in time \
                     (its strategy thread may be held by an order); it stays subscribed as asked, and a \
                     decline arriving later will still free its place"
                );
                return Ok(());
            }
            st = self.answered.wait_timeout(st, left).unwrap_or_else(PoisonError::into_inner).0;
        }
    }

    fn unwant(&self, token: u64, symbol: &str) {
        let Ok(symbol) = canonical(symbol) else { return };
        let _io = self.io();
        let port = {
            let mut st = self.state();
            if st.listener.as_ref().map(|c| c.token) != Some(token) || !st.wanted.contains(&symbol)
            {
                return;
            }
            st.drop_want(&symbol);
            st.sidecar.as_ref().map(|s| Arc::clone(&s.port))
        };
        self.answered.notify_all(); // a `want` of this very symbol still waiting stops waiting
        if let Some(port) = port {
            let _ = port.send(&encode_line(&Command::Unsubscribe { symbols: vec![symbol] }));
        }
    }

    fn unlisten(&self, token: u64) {
        let _io = self.io();
        let stale = {
            let mut st = self.state();
            if st.listener.as_ref().map(|c| c.token) != Some(token) {
                return;
            }
            st.listener = None;
            st.take_wants()
        };
        self.answered.notify_all();
        stop(stale);
    }

    /// Whether `token` is still the consumer.
    fn is_current(&self, token: u64) -> bool {
        self.state().listener.as_ref().map(|c| c.token) == Some(token)
    }
}

/// What `send_subscribe` left behind for `want` to do.
enum Sent {
    /// The sidecar will answer this `subscribe`: wait for it.
    Answering,
    /// Nothing to wait for: the symbol was already wanted, or the sidecar does not answer.
    NotAnswering,
}

/// The consumer's thread: drain its queue into `listener` in order, and look at the silence of the wanted symbols
/// every `check_every`. Ends when the queue's last sender is dropped (the consumer was replaced, closed or detached)
/// and the queue is empty, or when `listener` panics: that is caught, logged at `error`, and the consumer is
/// detached (`unlisten`: its wants are stopped at the sidecar), so one fault in the consumer costs the consumer its
/// prices and nothing else. `bus` is weak so this thread never keeps a bus alive.
fn run_consumer(
    token: u64,
    inbox: &Receiver<PriceTick>,
    listener: &TickListener,
    bus: &Weak<QuoteBus>,
    check_every: Duration,
) {
    let mut checked = Instant::now();
    loop {
        match inbox.recv_timeout(check_every) {
            Ok(tick) => {
                if catch_unwind(AssertUnwindSafe(|| listener(&tick))).is_err() {
                    tracing::error!(
                        target: TARGET,
                        token,
                        symbol = %tick.symbol,
                        "dukascopy: the price consumer PANICKED on a tick: it is detached and its prices \
                         stop (fills, positions and the order lane are unaffected); reopen the price \
                         stream to price again"
                    );
                    if let Some(bus) = bus.upgrade() {
                        bus.unlisten(token);
                    }
                    return;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        if checked.elapsed() >= check_every {
            checked = Instant::now();
            let Some(bus) = bus.upgrade() else { return };
            bus.warn_silent();
        }
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
    /// price lane, the cap of [`MAX_PRICED_SYMBOLS`] is reached, the sidecar is gone, the sidecar declined the
    /// instrument (a jar that answers: unknown to the SDK, past its own cap, the SDK refused; the place it took is
    /// freed), or this stream was replaced.
    ///
    /// With a jar that answers this waits for the answer, at most a few seconds (`ACK_WAIT`); it returns `Ok` on no
    /// answer in time, as it does for a jar that never answers, with one `warn`.
    pub fn want(&self, symbol: &str) -> Result<(), String> {
        self.bus.want(self.token, symbol)
    }

    /// Whether this is still the bus's consumer: `Err` in words once it was replaced by a newer stream or detached
    /// after a fault in its consumer. A stream that answers from its own books (an idempotent ask) checks this
    /// first, so a displaced stream is never told it still prices.
    ///
    /// # Errors
    /// This stream is no longer the bus's consumer.
    pub fn ensure_current(&self) -> Result<(), String> {
        if self.bus.is_current(self.token) { Ok(()) } else { Err(NOT_CURRENT.to_string()) }
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
