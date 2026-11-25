//! The PRICE LANE's rendezvous (any-symbol, decision 0174): how a live quote of an instrument the engine was not
//! mounted on gets from the JForex sidecar to a consumer, and what stands between them.
//!
//! The sidecar is ONE child process per gatewayhub (`crate::adapter`'s one-sidecar rule), owned by the exec
//! client the trade lane starts; the consumer is the data lane's credentialed stream (the PRICE STREAM below, which
//! the gatewayhub's `crates/vike-gatewayhub/src/data/credentialed.rs`'s `build_credentialed` builds), which opens on
//! another thread and another schedule. They share nothing but this bus:
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
//! * **BOUNDED.** At most `credentialed_limits::DUKASCOPY.max_streams` symbols are priced at once (the gatewayhub
//!   client's row, the ONE number both daemons read: `crates/vike-gatewayhub-client/src/credentialed_limits.rs`'s
//!   `DUKASCOPY`), held equal to the Java side's `TickLane.MAX_INSTRUMENTS` by a test. The JForex subscription limit
//!   itself is UNMEASURED.
//! * **ONE spelling per instrument: the bus speaks the WIRE**, the sidecar's spelling of a kept row
//!   (`crate::instrument::names`: `EURUSD`, `XAUUSD`, `AAPL.US/USD`), exactly as the sidecar's `tick`, `terms` and
//!   `subscription` lines carry it. The consumer speaks the USER name (`AAPL.CFD`, decision 0290): the PRICE STREAM's
//!   impls at the end of this file turn a subscription's user name into its wire through the kept row (no list, no row:
//!   refused in words) and key each tick by its user name again. The rate reader and the terms reader ask by the wire
//!   the account's list states.
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
//!   the amend feature, `crate::execution`'s `amend_supported`).
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
//!   [`Listening`] refuses while the newer one holds the lane, and takes a free lane back on its next ask
//!   ([`Listening::ensure_current`]), so it is never answered as if it still priced.
//! * **A decline is heard.** A jar that announces `tick-acks` answers each symbol of a `subscribe` with a
//!   `subscription` line; [`Listening::want`] waits a bounded time for it, hands a decline back as the refusal and
//!   frees the slot it had taken. A jar without the word, or one that does not answer in time, behaves as it always did
//!   (the want stands), the second with one `warn`.
//! * **Silence is said.** A wanted symbol that sends no tick for `SILENT_AFTER` while the sidecar is up is warned
//!   about once (a closed market, or a subscription the SDK lost across a reconnect: the jar re-asserts its
//!   subscriptions itself after one, `Bridge.java`'s `onDisconnect`), and a tick re-arms it.
//!
//! # The second reader: a RATE (multi-currency, decision 0183)
//!
//! The consumer above is the data lane's stream, one at a time. A quote-currency CONVERSION needs a price of
//! the conversion pair (`USDJPY` for a JPY-quoted pair on a USD account) from the trade lane's facts, which must
//! never take the consumer's place (a second `listen` replaces the first). So the bus has a second, read-only
//! door: [`QuoteBus::fresh_tick`] subscribes the pair at the sidecar if nobody has, waits a bounded time for a tick
//! the bus took within [`MAX_TICK_AGE_MS`] of the tick's OWN time, and returns it. Nothing about the consumer
//! changes: a rate pair is never forwarded to it, an `unwant` of a pair the rate reader also holds does not stop it,
//! and a replaced consumer does not unsubscribe it. [`QuoteBus::account_currency`] is the currency the attached
//! sidecar's `ready` line stated (`None` for a jar from before it, which is "unknown", never a default).
//!
//! **The rate reader's pairs are a CACHE, not a claim on the sidecar's eight places.** A conversion pair is asked
//! for on demand (an admission or a quote, never continuously) and the node holds the rate it was given, so a pair
//! is kept only as long as it is being used: at most [`MAX_RATED_SYMBOLS`] at once (asking for another gives the
//! least recently asked one back), none longer than [`RATE_IDLE`] without an ask, and every one yields to the
//! consumer, whose `want` finds the oldest idle rate pair unsubscribed to make room. Whatever the rate reader holds,
//! then, the consumer can still have all the bus's places of its own; a pair is given back to the sidecar with
//! an `unsubscribe` (the Java lane returns a place on it) except one the consumer also holds. Looking at five
//! foreign pairs in a row therefore costs a few seconds of subscriptions, not places for good.
//!
//! # The third reader: the venue's TERMS (metals and CFDs)
//!
//! A metal or a CFD is admitted on the amounts and price grid the VENUE states for it, which the sidecar writes as a
//! `terms` line at the instrument's first tick ([`FEATURE_TERMS`], `crate::instrument::terms`). [`QuoteBus::terms`] keeps each
//! line (per sidecar: a new one states them again) and [`QuoteBus::venue_terms`] asks for one, subscribing the
//! instrument exactly as the rate reader does (the same cache places and give-back) and waiting for the line rather than
//! for a fresh tick.
//!
//! # The PRICE STREAM over this bus (the consumer, any-symbol, decision 0174)
//!
//! The credentialed stream the gatewayhub's strategy lane opens for `cs_venue=dukascopy`, so the tradehub's control
//! door can price a pair its engine was not started on, is `vike_bridge_core::data_client::bus::DataClientBus` over
//! this bus: the shared engine, with everything venue-specific (its words, whether a repeat ask goes back to the
//! source) said by the `DataClientBusSpec` and `DataClientBusSpecHold` impls at the end of this file. MOVED here from
//! the gatewayhub's `history::dukascopy_quotes` (owner ruling of 2026-11-12: one-venue code does not live in a shared
//! crate), with its suite (`feed_stream_tests.rs`). A different kind of credentialed stream from the account-backed
//! ones: nothing is mounted on dukascopy (a profile cannot name it), and this stream **logs in to nothing**.
//!
//! * **LAZY, end to end.** Building it dials nothing and subscribes nothing; the first `subscribe_quotes` (the door
//!   asking a price for a pair) writes the `subscribe` command to the sidecar that is already up. No armed account and
//!   no sidecar means no price, said in words ([`Listening::want`]'s refusals), never a second login: that would put
//!   two JVMs on one platform cache (`crates/bridges/dukascopy/src/adapter.rs`'s module doc).
//! * **QUOTES ONLY.** Bars, trades and the book are refused by name; the price lane needs nothing else.
//! * **REAL-TIME, two-sided quotes only**: the bus has already dropped any tick that is one-sided, crossed, untimed or
//!   not current ([`accept`]), so every quote the stream emits is one, and the stream's label is sent as real-time on a
//!   symbol's first quote ([`vike_model::StreamDelay::RealTime`], read off that tick's own time).
//! * **NO link state is disclosed**: no `GapStart`, no `Live` (the bus's rule above), and the tradehub's price tap ages
//!   the last quote out (fail-closed) while the two hub links speak for the venue.
//! * **Bounded** by the bus (`credentialed_limits::DUKASCOPY.max_streams`), and one stream at a time owns the bus (a newer one replaces the
//!   older, which the bus warns about, naming both, and whose asks refuse while the newer one stands: the symbols it
//!   already streamed too, `DataClientBus::subscribe_quotes`).
//! * **One session for every holder** (decision 0193): the datahub's broker shares one client of this venue, and the
//!   gatewayhub's session guard keeps one session. A replaced session's wind-down reaches the sidecar with nothing,
//!   because its hold on the bus is stale, and once its replacement has ended its next subscribe takes the free bus
//!   back ([`Listening::ensure_current`]); the guard's half is pinned in the gatewayhub
//!   (`crates/vike-gatewayhub/src/data/session_guard_bus_tests.rs`).
//! * **The sink is fed from the bus's own thread**, never the sidecar's reader (the consumer-fault rule above): a slow
//!   or panicking sink costs the stream its quotes and nothing else.
//!
//! ⚠ **What this needs from the box**: a `jforex-bridge.jar` of the release that announces `ticks` (an older jar is
//! refused in words), and a dukascopy account armed at `demo` in THE SAME gatewayhub process.

use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::{Duration, Instant};

use vike_bridge_core::data_client::bus::{
    DataClientBusSpec, DataClientBusSpecDeliver, DataClientBusSpecHold, DataClientBusSpecKey,
    deliver_quote,
};
use vike_bridge_core::venue_link::VenueLinkState;
use vike_data::DataClientError;
use vike_gatewayhub_client::credentialed_limits::DUKASCOPY;
use vike_model::StreamDelay;

use crate::instrument::grid::{JforexInstrument, not_listed};
use crate::instrument::terms::VenueTerms;
use crate::refusals::{DukascopyPriceLaneRefusal, DukascopyStartRefusal};
use crate::transport::{SidecarCommand, encode_line};

/// Shared tracing target for this module's diagnostics.
const TARGET: &str = "vike_dukascopy::feed";

/// The READY line's feature word for the price lane (`Proto.java`'s `FEATURES`): the jar understands the
/// `subscribe` / `unsubscribe` commands and emits `tick` envelopes. A jar that does not announce it cannot be priced from.
pub const FEATURE_TICKS: &str = "ticks";

/// The READY line's feature word for the answers to a `subscribe` (`Proto.java`'s `FEATURES`): the jar writes one
/// `subscription` envelope per requested symbol, `ok` or a decline with its reason. A jar that does not announce it
/// is never waited for: its `subscribe` is fire-and-forget, as it was before the word existed.
pub const FEATURE_TICK_ACKS: &str = "tick-acks";

/// The READY line's feature word for the venue's terms (`Proto.java`'s `FEATURES`): at an instrument's first tick the
/// jar writes a `terms` line (`crate::instrument::terms`) with what the venue states about its amounts and price grid, and
/// spells a CFD with the SDK's slashed name. [`QuoteBus::venue_terms`] refuses in words against a jar without it, and a
/// metal or a CFD is admitted only on a jar that announces it.
pub const FEATURE_TERMS: &str = "terms";

/// At most this many of the bus's places ([`DUKASCOPY`]'s `max_streams`, the gatewayhub client's row, which both daemons
/// count against too and `the_capabilities_and_the_feature_word_agree_with_the_java` holds equal to the sidecar's own
/// `TickLane.MAX_INSTRUMENTS`; the JForex subscription limit is UNMEASURED) may be held for the RATE reader ([`QuoteBus::fresh_tick`]) at
/// once: one conversion pair per quote currency in use (JPY, GBP, CHF, CAD on a USD account). A fifth ask gives the
/// least recently asked one back instead of being refused; the consumer's own pairs can take any of them
/// (module doc).
pub const MAX_RATED_SYMBOLS: usize = 4;

/// How long a conversion pair nobody asked a rate of stays subscribed for the rate reader before it is given back to
/// the sidecar. The node asks a rate when it admits a symbol or refreshes one (minutes apart at the closest), so ten
/// minutes covers a burst of looks and orders and returns the place long before it is missed.
pub const RATE_IDLE: Duration = Duration::from_secs(600);

/// How long [`QuoteBus::fresh_tick`] waits for a tick of a pair it has just subscribed (a cold lane: the answer, then
/// the first tick of an open market). The rate is asked of a facts lookup that holds a daemon slot while it waits.
pub const RATE_WAIT: Duration = Duration::from_secs(4);

/// How many instruments' terms the bus keeps. The SDK predefines 180 and the venue lists on the order of seven hundred;
/// the cap only stops a sidecar that never stops naming new ones from growing the map (the surplus is dropped, said once).
pub const MAX_KEPT_TERMS: usize = 1024;

/// How far a tick's OWN time may be from this box's clock, either way, and still be a quote. A real-time tick
/// arrives within the network and queueing latency of its stamp; ten seconds is far above that and far below the
/// price tap's own age bound (`vike_core::MAX_PRICE_AGE`, 60 s), so a tick this lane lets through is
/// current by a margin the consumer does not have to re-judge. A box whose clock is off by more than this prices
/// nothing, and says why.
pub const MAX_TICK_AGE_MS: i64 = 10_000;

/// **Why [`QuoteBus::fresh_tick`] has no tick**, the refusal and of which kind: a caller tells "the pair was never
/// priced" from "it was priced and sent no tick in time" by the variant, never by the words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FreshTickError {
    /// The pair could not be SUBSCRIBED: every place it could take is in use (by lookups waiting on their ticks, or
    /// by the data lane), or the sidecar's command pipe is closed.
    NotSubscribed(DukascopyPriceLaneRefusal),
    /// Anything else: no sidecar running, a jar that predates the price lane, the sidecar declined the pair or went
    /// away while it was being priced, or no tick arrived in time.
    NoTick(DukascopyPriceLaneRefusal),
}

impl FreshTickError {
    /// The refusal, whichever kind it is.
    #[must_use]
    pub fn refusal(&self) -> &DukascopyPriceLaneRefusal {
        match self {
            Self::NotSubscribed(refusal) | Self::NoTick(refusal) => refusal,
        }
    }
}

impl std::fmt::Display for FreshTickError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self.refusal(), f)
    }
}

impl std::error::Error for FreshTickError {}

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

/// The bus's bounds and clocks, a value so a test can shorten them.
#[derive(Clone, Copy)]
struct Tuning {
    queue: usize,
    ack_wait: Duration,
    silent_after: Duration,
    check_every: Duration,
    /// The oldest a tick may be, by its OWN time, and still answer [`QuoteBus::fresh_tick`] from the bus's kept copy:
    /// its age when this box took it (already within [`MAX_TICK_AGE_MS`] by `accept`) PLUS the time since, so the
    /// bound is on the tick's own stamp and a kept tick is never twice that old.
    rate_age: Duration,
    /// How long a conversion pair may go without an ask before it is given back ([`RATE_IDLE`]).
    rate_idle: Duration,
}

impl Tuning {
    const PRODUCTION: Self = Self {
        queue: LISTENER_QUEUE,
        ack_wait: ACK_WAIT,
        silent_after: SILENT_AFTER,
        check_every: SILENCE_CHECK_EVERY,
        rate_age: Duration::from_millis(MAX_TICK_AGE_MS as u64),
        rate_idle: RATE_IDLE,
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
    pub timestamp: i64,
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
                 tick, a backlog, a skewed clock, or a stamp in seconds instead of milliseconds)"
            }
            TickRefusal::FromTheFuture => {
                "the tick's own time is more than ten seconds ahead of this box's clock (a skewed \
                 clock, or a stamp in microseconds or nanoseconds instead of milliseconds)"
            }
        })
    }
}

/// The one judgement of a tick: a real-time, two-sided, uncrossed quote, or the reason it is not one.
///
/// ⚠ **`time` is read as epoch MILLISECONDS, which the vendor's Javadoc never states** (`ITick.getTime()` has no
/// text; a wiki example and a 2009 support post say milliseconds). The bound is the guard against a wrong unit,
/// in BOTH directions and with no unit-specific code: a stamp in seconds is fifty years behind this box's clock
/// ([`TickRefusal::Stale`]) and one in microseconds or nanoseconds is a thousand or a million times ahead of it
/// ([`TickRefusal::FromTheFuture`]), so a mis-scaled tick is dropped and never becomes a price or a rate.
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
    Ok(PriceTick { symbol: symbol.to_string(), bid, ask, timestamp: time })
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

/// The sidecar currently attached.
struct Attached {
    id: u64,
    port: Arc<dyn LinePort>,
    /// Whether its `ready` line announced [`FEATURE_TICKS`].
    ticks: bool,
    /// Whether it also announced [`FEATURE_TICK_ACKS`] (only meaningful with `ticks`): it answers a `subscribe`.
    acks: bool,
    /// Whether it announced [`FEATURE_TERMS`]: it writes a `terms` line at an instrument's first tick.
    terms: bool,
    /// The currency its `ready` line stated for the account (`QuoteBus::set_account_currency`), as an upper-case
    /// three-letter code; `None` = not stated (a jar from before the field), which is unknown and never USD.
    account_currency: Option<String>,
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

/// A conversion pair the rate reader holds: when it was last asked a rate of, and how many asks are waiting on its
/// tick right now (a pair somebody is waiting on is not given back).
struct Rated {
    asked: Instant,
    waiting: u32,
}

/// The last tick accepted for a symbol, when this bus took it, and how old it already was by its own stamp then.
struct Kept {
    tick: PriceTick,
    at: Instant,
    /// `arrival clock - tick.ts` in ms, never negative (a tick a little ahead of the clock is as young as it gets).
    age_on_arrival_ms: i64,
}

#[derive(Default)]
struct State {
    /// The symbols a consumer asked prices for (wire names), whether or not a sidecar is up to serve them.
    wanted: BTreeSet<String>,
    /// The symbols the RATE reader holds ([`QuoteBus::fresh_tick`]), subscribed at the sidecar exactly like a want
    /// but never forwarded to the consumer; a symbol may be in both sets, and the sidecar keeps it until neither
    /// holds it. A cache: see the module doc for when one is given back.
    rated: BTreeMap<String, Rated>,
    /// The last tick accepted for each wanted or rated symbol: what [`QuoteBus::fresh_tick`] answers from while it
    /// is young (by the tick's own stamp).
    last: BTreeMap<String, Kept>,
    /// The wanted symbols whose `subscribe` the sidecar has not answered yet, for a sidecar that answers.
    awaiting: BTreeSet<String>,
    /// The reasons of declines that landed while a `want` was waiting for its answer (taken by that `want`).
    declined: BTreeMap<String, String>,
    /// The silence clock of each wanted symbol.
    quiet: BTreeMap<String, Quiet>,
    /// The venue's terms of each instrument the attached sidecar has stated them for ([`QuoteBus::terms`]); emptied
    /// when a sidecar attaches (a new jar states them again at the first tick). Bounded by [`MAX_KEPT_TERMS`].
    terms: BTreeMap<String, VenueTerms>,
    /// The account's instrument list as the last sidecar stated it ([`QuoteBus::set_instrument_list`]): kept across a
    /// sidecar's death and replaced by the next login's, never emptied (the list is the account's, not the process's).
    listed: Option<Arc<[JforexInstrument]>>,
    sidecar: Option<Attached>,
    listener: Option<Consumer>,
    /// The consumers a newer one replaced, while their [`Listening`] is held: each may take a free lane back
    /// ([`QuoteBus::rejoin`]). One a fault detached, or that let go, is not here.
    displaced: BTreeSet<u64>,
    /// One counter for listener tokens and attach ids (never reused).
    next: u64,
    dropped: u64,
    last_warn: Option<Instant>,
    /// Ticks the consumer's full queue turned away.
    overflowed: u64,
    last_overflow_warn: Option<Instant>,
}

impl State {
    /// How many symbols the sidecar is asked to price: the consumer's and the rate reader's, a shared one once.
    fn priced(&self) -> usize {
        self.wanted.len()
            + self.rated.keys().filter(|symbol| !self.wanted.contains(*symbol)).count()
    }

    /// Forget every want (and everything kept about one) and return the symbols that were held.
    fn drop_wants(&mut self) -> Vec<String> {
        self.awaiting.clear();
        self.declined.clear();
        self.quiet.clear();
        let rated = &self.rated;
        self.last.retain(|symbol, _| rated.contains_key(symbol));
        std::mem::take(&mut self.wanted).into_iter().collect()
    }

    /// Forget every want and return what must be told to the sidecar: its port and the symbols (none when
    /// nothing was wanted or no sidecar is up). A symbol the rate reader holds is NOT in the list: it stays
    /// subscribed.
    fn take_wants(&mut self) -> Option<(Arc<dyn LinePort>, Vec<String>)> {
        let mut symbols = self.drop_wants();
        symbols.retain(|symbol| !self.rated.contains_key(symbol));
        let port = self.sidecar.as_ref().map(|s| Arc::clone(&s.port))?;
        (!symbols.is_empty()).then_some((port, symbols))
    }

    /// Forget one want (and what is kept about it; the last tick stays while the rate reader holds the symbol).
    fn drop_want(&mut self, symbol: &str) {
        self.wanted.remove(symbol);
        self.awaiting.remove(symbol);
        self.quiet.remove(symbol);
        if !self.rated.contains_key(symbol) {
            self.last.remove(symbol);
        }
    }

    /// Keep `tick` as the last one of its symbol, in place (no allocation after the first). `now_ms` is the clock the
    /// tick was judged against: its age by its own stamp is kept, so a kept tick is judged on THAT and not on the
    /// time since this box took it alone.
    fn note_last(&mut self, tick: &PriceTick, now_ms: i64) {
        let at = Instant::now();
        let age_on_arrival_ms = now_ms.saturating_sub(tick.timestamp).max(0);
        match self.last.get_mut(&tick.symbol) {
            Some(kept) => {
                kept.tick.bid = tick.bid;
                kept.tick.ask = tick.ask;
                kept.tick.timestamp = tick.timestamp;
                kept.at = at;
                kept.age_on_arrival_ms = age_on_arrival_ms;
            }
            None => {
                self.last.insert(
                    tick.symbol.clone(),
                    Kept { tick: tick.clone(), at, age_on_arrival_ms },
                );
            }
        }
    }

    /// Stop holding `symbol` for the rate reader. Returns whether the sidecar must be told to stop it: only when the
    /// consumer does not also want it (then the subscription is the consumer's alone and stays).
    fn release_rated(&mut self, symbol: &str) -> bool {
        if self.rated.remove(symbol).is_none() {
            return false;
        }
        if self.wanted.contains(symbol) {
            return false;
        }
        self.last.remove(symbol);
        self.awaiting.remove(symbol);
        self.declined.remove(symbol);
        true
    }

    /// The conversion pair to give back first: the least recently asked one nobody is waiting on. With
    /// `rate_only`, only a pair the consumer does not also hold (the one whose release frees a place of the eight).
    fn least_recently_rated(&self, rate_only: bool) -> Option<String> {
        self.rated
            .iter()
            .filter(|(symbol, rated)| {
                rated.waiting == 0 && !(rate_only && self.wanted.contains(*symbol))
            })
            .min_by_key(|(_, rated)| rated.asked)
            .map(|(symbol, _)| symbol.clone())
    }

    /// Give back every conversion pair nobody has asked a rate of for `idle`; returns the symbols to unsubscribe.
    fn release_idle_rated(&mut self, idle: Duration) -> Vec<String> {
        let stale: Vec<String> = self
            .rated
            .iter()
            .filter(|(_, rated)| rated.waiting == 0 && rated.asked.elapsed() >= idle)
            .map(|(symbol, _)| symbol.clone())
            .collect();
        stale.into_iter().filter(|symbol| self.release_rated(symbol)).collect()
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
            st.displaced.extend(displaced);
            let held: Vec<String> = st.wanted.iter().cloned().collect();
            (token, displaced, held, st.take_wants())
        };
        self.drain_into(token, inbox, Arc::clone(&listener));
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
        Listening { bus: Arc::clone(self), token, listener }
    }

    /// The thread that drains consumer `token`'s queue into `listener`.
    fn drain_into(
        self: &Arc<Self>,
        token: u64,
        inbox: Receiver<PriceTick>,
        listener: TickListener,
    ) {
        let (bus, check_every) = (Arc::downgrade(self), self.tuning.check_every);
        std::thread::Builder::new()
            .name("duka-quote-feed".into())
            .spawn(move || run_consumer(token, &inbox, &listener, &bus, check_every))
            .expect("spawn quote delivery thread");
    }

    /// A consumer a newer one replaced takes the lane back, once nobody holds it (decision 0193 verdict 4: the
    /// replacement lasts as long as the replacing stream). There is still one consumer: an empty slot is the only
    /// way in, and the next [`Self::listen`] replaces this one like any other. Its wants went with the takeover,
    /// so what it held stays stopped until it asks again.
    fn rejoin(self: &Arc<Self>, token: u64, listener: &TickListener) {
        let _io = self.io();
        let mut st = self.state();
        if st.listener.is_some() || !st.displaced.remove(&token) {
            return;
        }
        let (queue, inbox) = sync_channel(self.tuning.queue);
        st.listener = Some(Consumer { token, queue });
        drop(st);
        self.drain_into(token, inbox, Arc::clone(listener));
        tracing::info!(
            target: TARGET,
            token,
            "dukascopy: price stream #{token} took the JForex price lane back: the stream that replaced it has \
             ended. The pairs it held stay stopped until it asks for them again"
        );
    }

    /// The exec reader thread's attach, on the sidecar's `ready` line: from here `want` writes to `port`, and the
    /// wants a previous sidecar held are replayed to this one. `features` is what its `ready` line announced
    /// ([`FEATURE_TICKS`], [`FEATURE_TICK_ACKS`]). Returns the id [`Self::detach`] takes. (Public for the
    /// exec client and for a consumer's test; nothing else calls it.)
    pub fn attach(&self, port: Arc<dyn LinePort>, features: &[String]) -> u64 {
        let ticks = features.iter().any(|f| f == FEATURE_TICKS);
        let acks = ticks && features.iter().any(|f| f == FEATURE_TICK_ACKS);
        let terms = ticks && features.iter().any(|f| f == FEATURE_TERMS);
        let _io = self.io();
        let (id, replay) = {
            let mut st = self.state();
            st.next += 1;
            let id = st.next;
            st.sidecar = Some(Attached {
                id,
                port: Arc::clone(&port),
                ticks,
                acks,
                terms,
                account_currency: None,
            });
            // A new sidecar states every instrument's terms again at its first tick of it.
            st.terms.clear();
            // A new sidecar starts every silence clock over, and its replay is not waited for.
            st.awaiting.clear();
            for quiet in st.quiet.values_mut() {
                *quiet = Quiet::now();
            }
            // The rate reader's pairs are replayed with the consumer's: they are held across a sidecar too.
            let replay: Vec<String> = if ticks {
                let mut held: BTreeSet<String> = st.wanted.clone();
                held.extend(st.rated.keys().cloned());
                held.into_iter().collect()
            } else {
                Vec::new()
            };
            (id, replay)
        };
        if !replay.is_empty()
            && !port.send(&encode_line(&SidecarCommand::Subscribe { symbols: replay }))
        {
            self.detach(id);
        }
        id
    }

    /// The currency the sidecar attached as `id` stated for its account on `ready`
    /// (`Envelope::Ready::currency`): kept as an upper-case three-letter code. Anything else (a blank, a longer
    /// word, digits) is NOT a currency and leaves it unstated. A no-op unless `id` is the sidecar attached.
    /// Called by the exec reader right after [`Self::attach`], before the start is told it succeeded.
    pub fn set_account_currency(&self, id: u64, currency: Option<&str>) {
        let code = currency
            .map(str::trim)
            .filter(|c| c.len() == 3 && c.bytes().all(|b| b.is_ascii_alphabetic()))
            .map(str::to_ascii_uppercase);
        if let Some(sent) = currency.filter(|_| code.is_none()) {
            tracing::warn!(
                target: TARGET,
                currency = sent,
                "dukascopy: the sidecar stated `{sent}` as the account's currency, which is not a three-letter \
                 code, so the account's currency stays UNKNOWN (a foreign-quoted pair is then refused)"
            );
        }
        if let Some(sidecar) = self.state().sidecar.as_mut().filter(|s| s.id == id) {
            sidecar.account_currency = code;
        }
    }

    /// The currency the attached sidecar stated for its account (an upper-case code), or `None`: no sidecar is up,
    /// or its jar predates the field. Unknown is never read as USD here.
    #[must_use]
    pub fn account_currency(&self) -> Option<String> {
        self.state().sidecar.as_ref().and_then(|s| s.account_currency.clone())
    }

    /// Whether a sidecar is attached at all (it has said `ready` and not yet closed): what tells a jar that states no
    /// currency from a sidecar that is not up (starting, restarting, never started).
    #[must_use]
    pub fn sidecar_attached(&self) -> bool {
        self.state().sidecar.is_some()
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
        let mut rated_tick = false;
        let queued = {
            let mut st = self.state();
            let (wanted, rated) = (st.wanted.contains(symbol), st.rated.contains_key(symbol));
            if !wanted && !rated {
                return;
            }
            st.heard(symbol);
            match accept(symbol, bid, ask, time, now_ms) {
                Ok(tick) => {
                    // Kept for the rate reader (in place, no allocation after the first tick of a symbol).
                    st.note_last(&tick, now_ms);
                    rated_tick = rated;
                    if wanted {
                        st.listener.as_ref().map(|c| (c.queue.clone(), tick))
                    } else {
                        None
                    }
                }
                Err(why) => {
                    st.note_dropped(symbol, why);
                    None
                }
            }
        };
        if rated_tick {
            self.answered.notify_all(); // a `fresh_tick` waiting for this pair's tick wakes up
        }
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
        if !ok && (st.wanted.contains(symbol) || st.rated.contains_key(symbol)) {
            st.rated.remove(symbol);
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

    /// One `instruments` envelope from the sidecar: the ACCOUNT's instrument list as its login received it
    /// (`crate::instrument::grid`), kept until the next one replaces it. An EMPTY statement keeps the list already held
    /// (the rule the datahub folds its kept file by: an empty answer over a list is a failure, not a delisting), and says
    /// so. (Public for the exec client and for a consumer's test; nothing else calls it.)
    pub fn set_instrument_list(&self, rows: Vec<JforexInstrument>) {
        if rows.is_empty() {
            tracing::warn!(
                target: TARGET,
                "dukascopy: the sidecar stated an EMPTY instrument list; the list already held (if any) is kept"
            );
            return;
        }
        tracing::info!(
            target: TARGET,
            instruments = rows.len(),
            "dukascopy: the sidecar stated the account's instrument list"
        );
        self.state().listed = Some(rows.into());
    }

    /// **The account's instrument list**, as the last sidecar of this process stated it at its login: what every grid,
    /// class and conversion pair of this venue is read from (owner 2026-11-13: no symbol fact is compiled in).
    ///
    /// # Errors
    /// No sidecar has stated one yet, in words ([`not_listed`]): the reader answers "not now", never a default.
    pub fn instrument_list(&self) -> Result<Arc<[JforexInstrument]>, String> {
        self.state().listed.clone().ok_or_else(not_listed)
    }

    /// One `terms` envelope from the sidecar (a jar that announced [`FEATURE_TERMS`]): kept under its symbol, replacing an
    /// older statement of the same instrument, and a `venue_terms` waiting for it wakes. Judged by nothing here: the
    /// figures are the venue's, and `crate::instrument::any_symbol::facts_from_terms` decides whether they admit an instrument.
    /// Kept under the wire name the line carries; the map is bounded ([`MAX_KEPT_TERMS`]).
    pub fn terms(&self, terms: VenueTerms) {
        let symbol = terms.symbol.clone();
        {
            let mut st = self.state();
            if st.terms.len() >= MAX_KEPT_TERMS && !st.terms.contains_key(&symbol) {
                return;
            }
            st.terms.insert(symbol, terms);
        }
        self.answered.notify_all();
    }

    /// **The venue's own terms of the instrument of wire name `symbol`, asking the sidecar to subscribe it if nobody has**
    /// (what admits a metal or a CFD: `crate::instrument::any_symbol::facts_from_terms`). The sidecar states them at the
    /// instrument's first tick, before the tick, so this holds the instrument for the rate reader's cache (the same
    /// places, bounds and give-back as [`Self::fresh_tick`]: nothing is forwarded to the consumer) and waits up to `wait`
    /// for the line. Terms already stated by the attached sidecar answer at once and keep the held pair from being given
    /// back. The line does not need a FRESH tick: a closed market whose last tick the SDK replays still states them (the
    /// tick itself is judged and dropped as stale by [`accept`]).
    ///
    /// # Errors
    /// In words: no sidecar is running, the jar predates the price lane or announces no `terms`, the places are all in
    /// use by lookups that are waiting, the sidecar is gone or declined the instrument, or no line arrived in time (a
    /// closed market, an instrument the account cannot trade, or a feed that is not live yet).
    pub fn venue_terms(
        &self,
        symbol: &str,
        wait: Duration,
    ) -> Result<VenueTerms, DukascopyPriceLaneRefusal> {
        let symbol = symbol.to_string();
        let deadline = Instant::now() + wait;
        {
            let mut st = self.state();
            if let Some(terms) = st.terms.get(&symbol).cloned() {
                if let Some(rated) = st.rated.get_mut(&symbol) {
                    rated.asked = Instant::now();
                }
                return Ok(terms);
            }
            if st.sidecar.as_ref().is_some_and(|s| s.ticks && !s.terms) {
                return Err(DukascopyPriceLaneRefusal::JarPredatesTerms { symbol });
            }
        }
        self.hold(&symbol, "subscribed to read its trade terms")
            .map_err(|why| why.refusal().clone())?;
        let answer = self.await_terms(&symbol, deadline, wait);
        if let Some(rated) = self.state().rated.get_mut(&symbol) {
            rated.waiting = rated.waiting.saturating_sub(1); // nobody waits on it now: it may be given back
        }
        answer
    }

    /// The second half of [`Self::venue_terms`]: wait for the `terms` line of a symbol [`Self::hold`] took the place of.
    fn await_terms(
        &self,
        symbol: &str,
        deadline: Instant,
        wait: Duration,
    ) -> Result<VenueTerms, DukascopyPriceLaneRefusal> {
        let mut st = self.state();
        let symbol_owned = || symbol.to_string();
        loop {
            if let Some(terms) = st.terms.get(symbol) {
                return Ok(terms.clone());
            }
            if let Some(reason) = st.declined.remove(symbol) {
                return Err(DukascopyPriceLaneRefusal::DeclinedToSubscribe {
                    symbol: symbol_owned(),
                    reason,
                });
            }
            if st.sidecar.is_none() {
                return Err(DukascopyPriceLaneRefusal::WentAwayReadingTerms {
                    symbol: symbol_owned(),
                });
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                st.awaiting.remove(symbol);
                return Err(DukascopyPriceLaneRefusal::NoTermsInTime {
                    symbol: symbol_owned(),
                    millis: wait.as_millis(),
                });
            }
            st = self.answered.wait_timeout(st, left).unwrap_or_else(PoisonError::into_inner).0;
        }
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

    fn want(&self, token: u64, symbol: &str) -> Result<(), DukascopyPriceLaneRefusal> {
        match self.send_subscribe(token, symbol)? {
            Sent::Answering => self.await_answer(token, symbol),
            Sent::NotAnswering => Ok(()),
        }
    }

    /// The first half of a `want`: validate, take the place, write the `subscribe` (all under `io`, released on
    /// return so the wait for the answer holds nothing but the state lock the condvar gives up).
    fn send_subscribe(&self, token: u64, symbol: &str) -> Result<Sent, DukascopyPriceLaneRefusal> {
        let symbol = symbol.to_string();
        let _io = self.io();
        let (port, id, acks, plan, back) = {
            let mut st = self.state();
            if st.listener.as_ref().map(|c| c.token) != Some(token) {
                return Err(DukascopyPriceLaneRefusal::NotCurrent);
            }
            let Some(sidecar) = st.sidecar.as_ref() else {
                return Err(DukascopyPriceLaneRefusal::NoSidecar { symbol, purpose: "priced" });
            };
            if !sidecar.ticks {
                return Err(DukascopyPriceLaneRefusal::JarPredatesTicks {
                    symbol,
                    purpose: "priced",
                });
            }
            let (port, id, acks) = (Arc::clone(&sidecar.port), sidecar.id, sidecar.acks);
            // The consumer comes before the rate reader: the conversion pairs nobody is using are given back,
            // and when the eight places are still full the least recently used one of them yields this one.
            let mut back = st.release_idle_rated(self.tuning.rate_idle);
            let plan = if st.wanted.contains(&symbol) {
                Plan::Nothing
            } else if st.rated.contains_key(&symbol) {
                // The rate reader already has the sidecar pricing it: share that subscription.
                st.wanted.insert(symbol.clone());
                st.quiet.insert(symbol.clone(), Quiet::now());
                Plan::Nothing
            } else {
                if st.priced() >= DUKASCOPY.max_streams
                    && let Some(oldest) = st.least_recently_rated(true)
                    && st.release_rated(&oldest)
                {
                    back.push(oldest);
                }
                if st.priced() >= DUKASCOPY.max_streams {
                    Plan::Refused(DukascopyPriceLaneRefusal::AtMostPriced {
                        max: DUKASCOPY.max_streams,
                        symbol: symbol.clone(),
                    })
                } else {
                    st.wanted.insert(symbol.clone());
                    st.quiet.insert(symbol.clone(), Quiet::now());
                    st.declined.remove(&symbol);
                    if acks {
                        st.awaiting.insert(symbol.clone());
                    }
                    Plan::Subscribe
                }
            };
            (port, id, acks, plan, back)
        };
        if !back.is_empty() {
            stop(Some((Arc::clone(&port), back))); // before the subscribe: the sidecar's cap never overshoots
        }
        match plan {
            Plan::Nothing => return Ok(Sent::NotAnswering),
            Plan::Refused(why) => return Err(why),
            Plan::Subscribe => {}
        }
        if port.send(&encode_line(&SidecarCommand::Subscribe { symbols: vec![symbol.clone()] })) {
            return Ok(if acks { Sent::Answering } else { Sent::NotAnswering });
        }
        // The pipe is closed: the sidecar is gone, and nothing the wants hold is being served.
        self.state().drop_want(&symbol);
        self.detach(id);
        Err(DukascopyPriceLaneRefusal::SidecarGone { symbol })
    }

    /// The second half of a `want` on a sidecar that answers: wait (bounded) for its `subscription` line. A decline is
    /// the refusal; an `ok`, or the want having been dropped meanwhile, is `Ok`; a stream replaced meanwhile is told
    /// so; and no answer in time is `Ok` too, with one `warn`: the want stands, exactly as it does for a jar that never
    /// answers (a late `ok` changes nothing, a late decline frees the place).
    fn await_answer(&self, token: u64, symbol: &str) -> Result<(), DukascopyPriceLaneRefusal> {
        let deadline = Instant::now() + self.tuning.ack_wait;
        let mut st = self.state();
        loop {
            if st.listener.as_ref().map(|c| c.token) != Some(token) {
                return Err(DukascopyPriceLaneRefusal::NotCurrent);
            }
            if let Some(reason) = st.declined.remove(symbol) {
                return Err(DukascopyPriceLaneRefusal::DeclinedToPrice {
                    symbol: symbol.to_string(),
                    reason,
                });
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
        let symbol = symbol.to_string();
        let _io = self.io();
        let port = {
            let mut st = self.state();
            if st.listener.as_ref().map(|c| c.token) != Some(token) || !st.wanted.contains(&symbol)
            {
                return;
            }
            st.drop_want(&symbol);
            // A pair the rate reader holds too stays subscribed at the sidecar.
            let held_for_a_rate = st.rated.contains_key(&symbol);
            st.sidecar.as_ref().filter(|_| !held_for_a_rate).map(|s| Arc::clone(&s.port))
        };
        self.answered.notify_all(); // a `want` of this very symbol still waiting stops waiting
        if let Some(port) = port {
            let _ = port.send(&encode_line(&SidecarCommand::Unsubscribe { symbols: vec![symbol] }));
        }
    }

    /// **The freshest tick of `symbol`, asking the sidecar for it if nobody has** (the rate reader, module doc): the
    /// tick this bus took, if it is still within [`MAX_TICK_AGE_MS`] by its OWN time, else the pair is subscribed
    /// (held for the rate reader as a cache entry: [`MAX_RATED_SYMBOLS`] at most, given back after [`RATE_IDLE`]
    /// without an ask or to the consumer who needs the place) and this waits up to `wait` for a tick that
    /// [`accept`] let through. Every tick it returns passed the same judgement as one sent to the consumer:
    /// two-sided, uncrossed, with a time of its own within [`MAX_TICK_AGE_MS`] of this box's clock when it
    /// arrived, and it is never older than that bound when returned. It never takes the consumer's place and never
    /// forwards the pair to it.
    ///
    /// # Errors
    /// In words: no sidecar is running, the jar predates the price lane, the places are all in use by lookups that are
    /// waiting, the sidecar is gone or declined the instrument, or no tick arrived in time (a closed market, or a feed
    /// that is not live yet).
    pub fn fresh_tick(&self, symbol: &str, wait: Duration) -> Result<PriceTick, FreshTickError> {
        let symbol = symbol.to_string();
        let deadline = Instant::now() + wait;
        {
            let mut st = self.state();
            if let Some(tick) = self.recent(&st, &symbol) {
                // A rate asked of a held conversion pair keeps it (the idle clock starts over).
                if let Some(rated) = st.rated.get_mut(&symbol) {
                    rated.asked = Instant::now();
                }
                return Ok(tick);
            }
        }
        self.hold_for_rate(&symbol)?;
        let answer = self.await_tick(&symbol, deadline, wait);
        if let Some(rated) = self.state().rated.get_mut(&symbol) {
            rated.waiting = rated.waiting.saturating_sub(1); // nobody waits on it now: it may be given back
        }
        answer
    }

    /// The second half of [`Self::fresh_tick`]: wait for the tick of a pair [`Self::hold_for_rate`] took the place of.
    fn await_tick(
        &self,
        symbol: &str,
        deadline: Instant,
        wait: Duration,
    ) -> Result<PriceTick, FreshTickError> {
        let mut st = self.state();
        loop {
            if let Some(tick) = self.recent(&st, symbol) {
                return Ok(tick);
            }
            let symbol_owned = || symbol.to_string();
            if let Some(reason) = st.declined.remove(symbol) {
                return Err(FreshTickError::NoTick(DukascopyPriceLaneRefusal::DeclinedToPrice {
                    symbol: symbol_owned(),
                    reason,
                }));
            }
            if st.sidecar.is_none() {
                return Err(FreshTickError::NoTick(DukascopyPriceLaneRefusal::WentAwayPricing {
                    symbol: symbol_owned(),
                }));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                st.awaiting.remove(symbol);
                return Err(FreshTickError::NoTick(DukascopyPriceLaneRefusal::NoTickInTime {
                    symbol: symbol_owned(),
                    millis: wait.as_millis(),
                }));
            }
            st = self.answered.wait_timeout(st, left).unwrap_or_else(PoisonError::into_inner).0;
        }
    }

    /// The tick of `symbol` this bus holds, if it is not older than the rate age BY ITS OWN STAMP: the age it already
    /// had when this box took it, plus the time since (so a tick taken at 8 s old is good for 2 s more, not 10).
    fn recent(&self, st: &State, symbol: &str) -> Option<PriceTick> {
        let kept = st.last.get(symbol)?;
        let on_arrival = Duration::from_millis(u64::try_from(kept.age_on_arrival_ms).unwrap_or(0));
        (on_arrival + kept.at.elapsed() <= self.tuning.rate_age).then(|| kept.tick.clone())
    }

    /// The first half of [`Self::fresh_tick`]: take the rate reader's place for `symbol` (raising the count of
    /// asks waiting on it) and, unless the sidecar already prices it for someone, write the `subscribe` (under `io`,
    /// like a `want`). Conversion pairs nobody has asked a rate of for [`RATE_IDLE`] are given back first, and a full
    /// set (or the sidecar's eight places) gives back its least recently asked pair rather than refusing; only a
    /// set every member of which has a lookup waiting on its tick refuses.
    fn hold_for_rate(&self, symbol: &str) -> Result<(), FreshTickError> {
        self.hold(symbol, "priced for a conversion")
    }

    /// [`Self::hold_for_rate`] for any purpose that needs the instrument subscribed (`purpose` ends the sentence
    /// "`SYMBOL` cannot be ..."): a rate, or the reading of the instrument's terms ([`Self::venue_terms`]).
    fn hold(&self, symbol: &str, purpose: &'static str) -> Result<(), FreshTickError> {
        let _io = self.io();
        let (port, id, plan, back) = {
            let mut st = self.state();
            let Some(sidecar) = st.sidecar.as_ref() else {
                return Err(FreshTickError::NoTick(DukascopyPriceLaneRefusal::NoSidecar {
                    symbol: symbol.to_string(),
                    purpose,
                }));
            };
            if !sidecar.ticks {
                return Err(FreshTickError::NoTick(DukascopyPriceLaneRefusal::JarPredatesTicks {
                    symbol: symbol.to_string(),
                    purpose,
                }));
            }
            let (port, id, acks) = (Arc::clone(&sidecar.port), sidecar.id, sidecar.acks);
            let mut back = st.release_idle_rated(self.tuning.rate_idle);
            let plan = if let Some(rated) = st.rated.get_mut(symbol) {
                rated.asked = Instant::now();
                rated.waiting += 1;
                Plan::Nothing
            } else {
                if st.rated.len() >= MAX_RATED_SYMBOLS
                    && let Some(oldest) = st.least_recently_rated(false)
                    && st.release_rated(&oldest)
                {
                    back.push(oldest);
                }
                let shared = st.wanted.contains(symbol);
                if !shared
                    && st.priced() >= DUKASCOPY.max_streams
                    && let Some(oldest) = st.least_recently_rated(true)
                    && st.release_rated(&oldest)
                {
                    back.push(oldest);
                }
                if st.rated.len() >= MAX_RATED_SYMBOLS {
                    Plan::Refused(DukascopyPriceLaneRefusal::RatedPairsBusy {
                        max: MAX_RATED_SYMBOLS,
                        symbol: symbol.to_string(),
                    })
                } else if !shared && st.priced() >= DUKASCOPY.max_streams {
                    Plan::Refused(DukascopyPriceLaneRefusal::AtMostPriced {
                        max: DUKASCOPY.max_streams,
                        symbol: symbol.to_string(),
                    })
                } else {
                    st.rated
                        .insert(symbol.to_string(), Rated { asked: Instant::now(), waiting: 1 });
                    if shared {
                        Plan::Nothing // the consumer's subscription already serves it
                    } else {
                        st.declined.remove(symbol);
                        if acks {
                            st.awaiting.insert(symbol.to_string());
                        }
                        Plan::Subscribe
                    }
                }
            };
            (port, id, plan, back)
        };
        if !back.is_empty() {
            stop(Some((Arc::clone(&port), back))); // before the subscribe: the sidecar's cap never overshoots
        }
        match plan {
            Plan::Nothing => return Ok(()),
            // every refusal of this plan is a place the pair could not take
            Plan::Refused(why) => return Err(FreshTickError::NotSubscribed(why)),
            Plan::Subscribe => {}
        }
        if port.send(&encode_line(&SidecarCommand::Subscribe { symbols: vec![symbol.to_string()] }))
        {
            return Ok(());
        }
        // The pipe is closed: the sidecar is gone, and nothing it was asked for is being served.
        {
            let mut st = self.state();
            st.rated.remove(symbol);
            st.drop_want(symbol);
        }
        self.detach(id);
        Err(FreshTickError::NotSubscribed(DukascopyPriceLaneRefusal::SidecarGone {
            symbol: symbol.to_string(),
        }))
    }

    fn unlisten(&self, token: u64) {
        let _io = self.io();
        let stale = {
            let mut st = self.state();
            st.displaced.remove(&token);
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

    /// Whether the consumer's wire name `symbol` is wanted now.
    fn is_wanted(&self, symbol: &str) -> bool {
        self.state().wanted.contains(symbol)
    }
}

/// What `send_subscribe` decided under the state lock, before anything is written.
enum Plan {
    /// Already priced (the consumer's own, or shared with the rate reader's): nothing to write.
    Nothing,
    /// No place for it.
    Refused(DukascopyPriceLaneRefusal),
    /// A place was taken: write the `subscribe`.
    Subscribe,
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
        let _ = port.send(&encode_line(&SidecarCommand::Unsubscribe { symbols }));
    }
}

/// The consumer's hold on the bus: the only way to ask for a price, and the owner of what was asked. Dropping it
/// (the stream closing) stops every price it asked for and removes the listener.
pub struct Listening {
    bus: Arc<QuoteBus>,
    token: u64,
    /// Kept so a stream a newer one replaced can take a free lane back ([`QuoteBus::rejoin`]).
    listener: TickListener,
}

impl Listening {
    /// Ask live quotes for the instrument of wire name `symbol` (the sidecar's spelling of its kept row,
    /// `crate::instrument::names::wire_symbol`: `EURUSD`, `AAPL.US/USD`). Idempotent for a symbol already asked.
    ///
    /// # Errors
    /// In words: no sidecar is running, the installed jar predates the
    /// price lane, the cap of [`DUKASCOPY`]'s `max_streams` is reached, the sidecar is gone, the sidecar declined the
    /// instrument (a jar that answers: unknown to the SDK, past its own cap, the SDK refused; the place it took is
    /// freed), or this stream was replaced.
    ///
    /// With a jar that answers this waits for the answer, at most a few seconds (`ACK_WAIT`); it returns `Ok` on no
    /// answer in time, as it does for a jar that never answers, with one `warn`.
    pub fn want(&self, symbol: &str) -> Result<(), DukascopyPriceLaneRefusal> {
        self.bus.want(self.token, symbol)
    }

    /// Make sure this is the bus's consumer: a stream a newer one replaced takes the lane back if that one has ended
    /// (`QuoteBus::rejoin`), and otherwise `Err` in words while the newer stream holds the lane, or for good after a
    /// fault in its consumer. A stream that answers from its own books (an idempotent ask) checks this first, so a
    /// displaced stream is never told it still prices.
    ///
    /// # Errors
    /// This stream is not the bus's consumer.
    pub fn ensure_current(&self) -> Result<(), DukascopyPriceLaneRefusal> {
        self.bus.rejoin(self.token, &self.listener);
        if self.bus.is_current(self.token) {
            Ok(())
        } else {
            Err(DukascopyPriceLaneRefusal::NotCurrent)
        }
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

/// **Why the sidecar of the account `route_key` did not start, in its own words**: the cause behind `adapter failed`,
/// which the adapter's start cannot hand back (`AdapterOutcome::paper()` carries none) and which otherwise stays in the
/// gatewayhub's journal. The exec client claims the account's ONE cell (`vike_bridge_core::venue_link::venue_link` of
/// its route key) and writes its sidecar's state there, and the gatewayhub's status verb reads the same cell. `None`
/// when the sidecar is up, nothing was said yet, or no client of the account has started.
#[must_use]
pub fn start_failure(route_key: &str) -> Option<String> {
    start_failure_of(&vike_bridge_core::venue_link::venue_link(route_key).state())
}

fn start_failure_of(state: &VenueLinkState) -> Option<String> {
    match state {
        VenueLinkState::Up | VenueLinkState::Unknown => None,
        VenueLinkState::Down(down) => {
            Some(DukascopyStartRefusal::SidecarWouldNotStart { why: down.why.clone() }.to_string())
        }
    }
}

/// The bus as a price stream's source (`vike_bridge_core::data_client::bus`; the stream is the gatewayhub's
/// `cs_venue=dukascopy`, this module doc's "The PRICE STREAM over this bus").
impl DataClientBusSpec for QuoteBus {
    type Hold = Listening;
    const VENUE: &'static str = crate::reconcile::VENUE;
    const CLOSED: &'static str = "dukascopy: the price stream is closed";
    const BARS: Result<(), &'static str> =
        Err("dukascopy's price stream carries quotes only (no bars)");
    const TRADES: Result<(), &'static str> =
        Err("dukascopy's price stream carries quotes only (no trades)");
    const BOOK: Result<(), &'static str> =
        Err("dukascopy's price stream carries quotes only (no book)");
    /// The bus caps itself (`credentialed_limits::DUKASCOPY.max_streams`, in its own words).
    const MAX_SYMBOLS: Option<(usize, &'static str)> = None;
    /// `accept` judged every tick already.
    const PRICE_CHECK: bool = false;

    fn consume(self: &Arc<Self>, deliver: DataClientBusSpecDeliver) -> Listening {
        // JForex tick volumes are not carried: "the feed does not carry it". Every tick is real-time: `accept`
        // dropped any that is not current by its own time. A tick is keyed by the USER name its wire was asked by
        // (`ask` below remembered the pair: decision 0290).
        QuoteBus::listen(
            self,
            Arc::new(move |t: &PriceTick| {
                deliver_quote(
                    &deliver,
                    &crate::instrument::names::user_symbol_or_wire(&t.symbol),
                    t.timestamp,
                    t.bid,
                    t.ask,
                    StreamDelay::RealTime,
                );
            }),
        )
    }
}

impl DataClientBusSpecHold for Listening {
    fn ensure_current(&self) -> Result<(), String> {
        Listening::ensure_current(self).map_err(|refusal| refusal.to_string())
    }
    /// A symbol already streaming answers from the stream alone: `want` writes a `subscribe` command to the
    /// sidecar, one per symbol. A stream let back in holds no want (the takeover stopped them), so its books no
    /// longer say what is streaming and the bus is asked. Only the quote lane reaches here (the others are
    /// refused first). The key is the USER name (`AAPL.CFD`); the bus is asked by its kept row's wire
    /// (`crate::instrument::names::wire_symbol`: no list, no row or two rows is the refusal, in words).
    fn ask(&self, key: &DataClientBusSpecKey, streaming: bool) -> Result<(), DataClientError> {
        let wire = crate::instrument::names::wire_symbol(key.symbol()).map_err(|why| {
            DataClientError::Subscribe(DukascopyPriceLaneRefusal::NotPriced { why }.to_string())
        })?;
        if streaming && self.bus.is_wanted(&wire) {
            Ok(())
        } else {
            Listening::want(self, &wire)
                .map_err(|refusal| DataClientError::Subscribe(refusal.to_string()))
        }
    }
    fn unwant(&self, key: &DataClientBusSpecKey) {
        if let Ok(wire) = crate::instrument::names::wire_symbol(key.symbol()) {
            Listening::unwant(self, &wire);
        }
    }
}

#[cfg(test)]
#[path = "feed_tests.rs"]
mod feed_tests;

#[cfg(test)]
#[path = "feed_rate_tests.rs"]
mod feed_rate_tests;

#[cfg(test)]
#[path = "feed_stream_tests.rs"]
mod feed_stream_tests;
