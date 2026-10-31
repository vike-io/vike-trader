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
//! * **ONE spelling per instrument**, the one the sidecar round-trips (`OrderMap.canonicalSymbol`): six ASCII letters for
//!   a currency pair or a metal (`EURUSD`, `XAUUSD`) and the SDK's slashed name for a CFD (`COPPER.CMD/USD`). The door has
//!   already canonicalised; anything else, the enum-style `COPPERCMDUSD` included, is refused here.
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
//! then, the consumer can still have all [`MAX_PRICED_SYMBOLS`] of its own; a pair is given back to the sidecar with
//! an `unsubscribe` (the Java lane returns a place on it) except one the consumer also holds. Looking at five
//! foreign pairs in a row therefore costs a few seconds of subscriptions, not places for good.
//!
//! # The third reader: the venue's TERMS (metals and CFDs)
//!
//! A metal or a CFD is admitted on the amounts and price grid the VENUE states for it, which the sidecar writes as a
//! `terms` line at the instrument's first tick ([`FEATURE_TERMS`], `crate::venue_terms`). [`QuoteBus::terms`] keeps each
//! line (per sidecar: a new one states them again) and [`QuoteBus::venue_terms`] asks for one, subscribing the
//! instrument exactly as the rate reader does (the same cache places and give-back) and waiting for the line rather than
//! for a fresh tick.

use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::{Duration, Instant};

use crate::exec::SidecarState;
use crate::proto::{Command, encode_line};
use crate::venue_terms::VenueTerms;

/// Shared tracing target for this module's diagnostics.
const TARGET: &str = "vike_dukascopy::quotes";

/// The READY line's feature word for the price lane (`Proto.java`'s `FEATURES`): the jar understands the
/// `subscribe` / `unsubscribe` commands and emits `tick` envelopes. A jar that does not announce it cannot be priced from.
pub const FEATURE_TICKS: &str = "ticks";

/// The READY line's feature word for the answers to a `subscribe` (`Proto.java`'s `FEATURES`): the jar writes one
/// `subscription` envelope per requested symbol, `ok` or a decline with its reason. A jar that does not announce it
/// is never waited for: its `subscribe` is fire-and-forget, as it was before the word existed.
pub const FEATURE_TICK_ACKS: &str = "tick-acks";

/// The READY line's feature word for the venue's terms (`Proto.java`'s `FEATURES`): at an instrument's first tick the
/// jar writes a `terms` line (`crate::venue_terms`) with what the venue states about its amounts and price grid, and
/// spells a CFD with the SDK's slashed name. [`QuoteBus::venue_terms`] refuses in words against a jar without it, and a
/// metal or a CFD is admitted only on a jar that announces it.
pub const FEATURE_TERMS: &str = "terms";

/// At most this many symbols are priced at once. Held equal to `TickLane.MAX_INSTRUMENTS` (the sidecar's own cap)
/// by `the_caps_and_the_feature_word_agree_with_the_java`. The JForex subscription limit is UNMEASURED (a live login).
pub const MAX_PRICED_SYMBOLS: usize = 8;

/// At most this many of the [`MAX_PRICED_SYMBOLS`] may be held for the RATE reader ([`QuoteBus::fresh_tick`]) at
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
/// price tap's own age bound (`vike-tradehub`'s `MAX_PRICE_AGE`, 60 s), so a tick this lane lets through is
/// current by a margin the consumer does not have to re-judge. A box whose clock is off by more than this prices
/// nothing, and says why.
pub const MAX_TICK_AGE_MS: i64 = 10_000;

/// The words every refusal to SUBSCRIBE a pair for the rate reader ends its sentence with (no place for it, the
/// sidecar gone): the one thing [`is_not_subscribed`] looks for, so a caller can tell "the pair was never priced"
/// from "it was priced and sent no tick in time" without a second error type.
const NOT_SUBSCRIBED: &str = "was not subscribed";

/// Whether `why`, an error of [`QuoteBus::fresh_tick`], says the pair could not be SUBSCRIBED (all the sidecar's
/// places are taken, or the sidecar went away) rather than that it was subscribed and sent no tick in time.
#[must_use]
pub fn is_not_subscribed(why: &str) -> bool {
    why.contains(NOT_SUBSCRIBED)
}

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

/// The spellings the sidecar round-trips (`OrderMap.canonicalSymbol`), upper-cased: six ASCII letters for a currency pair
/// or a metal (`EURUSD`, `XAUUSD`; the slashed `EUR/USD` is read as `EURUSD`, so one pair is one symbol), and the SDK's own
/// slashed name for every other instrument, a CFD (`COPPER.CMD/USD`, `AAPL.US/USD`): a code of letters, digits and dots, a
/// slash and a three-letter currency. Never the enum-style name of a CFD (`COPPERCMDUSD`): the sidecar cannot resolve it.
fn canonical(symbol: &str) -> Result<String, String> {
    let up = symbol.trim().to_ascii_uppercase();
    if up.len() == 6 && up.bytes().all(|b| b.is_ascii_uppercase()) {
        return Ok(up);
    }
    if let Some((code, currency)) = up.split_once('/') {
        let currency_ok = currency.len() == 3 && currency.bytes().all(|b| b.is_ascii_uppercase());
        if currency_ok && code.len() == 3 && code.bytes().all(|b| b.is_ascii_uppercase()) {
            return Ok(format!("{code}{currency}"));
        }
        let code_ok = (1..=16).contains(&code.len())
            && code.bytes().next().is_some_and(|b| b.is_ascii_uppercase())
            && code.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'.');
        if currency_ok && code_ok {
            return Ok(up);
        }
    }
    Err(format!(
        "dukascopy: `{symbol}` is not a six-letter pair like EURUSD or an SDK name like COPPER.CMD/USD, \
         the spellings the sidecar round-trips, so it is not priced"
    ))
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
    account_ccy: Option<String>,
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
    /// The symbols a consumer asked prices for (canonical), whether or not a sidecar is up to serve them.
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
        let age_on_arrival_ms = now_ms.saturating_sub(tick.ts).max(0);
        match self.last.get_mut(&tick.symbol) {
            Some(kept) => {
                kept.tick.bid = tick.bid;
                kept.tick.ask = tick.ask;
                kept.tick.ts = tick.ts;
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
    /// Where the exec client's sidecar is ([`Self::sidecar`]): the client writes it, the gatewayhub's trade lane
    /// reads it for its status report. It lives on the bus because the bus is the one thing the two share (the
    /// client sits behind a `Box<dyn ExecutionClient>` the lane cannot reach).
    sidecar: Mutex<SidecarState>,
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
            sidecar: Mutex::new(SidecarState::Up),
        })
    }

    /// Where the exec client that owns this bus's sidecar says it is: [`SidecarState::Up`] (also while no client
    /// has been started: there is nothing to report), or [`SidecarState::Down`] with the sidecar's own words.
    #[must_use]
    pub fn sidecar(&self) -> SidecarState {
        self.sidecar.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// The exec client's word on its sidecar. (Public for the exec client and for a consumer's test; nothing
    /// else calls it.)
    pub fn set_sidecar(&self, state: SidecarState) {
        *self.sidecar.lock().unwrap_or_else(PoisonError::into_inner) = state;
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
                account_ccy: None,
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
        if !replay.is_empty() && !port.send(&encode_line(&Command::Subscribe { symbols: replay })) {
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
            sidecar.account_ccy = code;
        }
    }

    /// The currency the attached sidecar stated for its account (an upper-case code), or `None`: no sidecar is up,
    /// or its jar predates the field. Unknown is never read as USD here.
    #[must_use]
    pub fn account_currency(&self) -> Option<String> {
        self.state().sidecar.as_ref().and_then(|s| s.account_ccy.clone())
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

    /// One `terms` envelope from the sidecar (a jar that announced [`FEATURE_TERMS`]): kept under its symbol, replacing an
    /// older statement of the same instrument, and a `venue_terms` waiting for it wakes. Judged by nothing here: the
    /// figures are the venue's, and `crate::any_symbol::facts_from_terms` decides whether they admit an instrument.
    /// A symbol that is not a spelling this lane knows is dropped, and the map is bounded ([`MAX_KEPT_TERMS`]).
    pub fn terms(&self, terms: VenueTerms) {
        let Ok(symbol) = canonical(&terms.symbol) else {
            tracing::warn!(
                target: TARGET,
                symbol = %terms.symbol,
                "dukascopy: the sidecar stated the terms of an instrument spelled in a way this lane does not know; dropped"
            );
            return;
        };
        {
            let mut st = self.state();
            if st.terms.len() >= MAX_KEPT_TERMS && !st.terms.contains_key(&symbol) {
                return;
            }
            st.terms.insert(symbol, terms);
        }
        self.answered.notify_all();
    }

    /// **The venue's own terms of `symbol`, asking the sidecar to subscribe it if nobody has** (what admits a metal or a
    /// CFD: `crate::any_symbol::facts_from_terms`). The sidecar states them at the instrument's first tick, before the
    /// tick, so this holds the instrument for the rate reader's cache (the same places, bounds and give-back as
    /// [`Self::fresh_tick`]: nothing is forwarded to the consumer) and waits up to `wait` for the line. Terms already
    /// stated by the attached sidecar answer at once and keep the held pair from being given back. The line does not need
    /// a FRESH tick: a closed market whose last tick the SDK replays still states them (the tick itself is judged and
    /// dropped as stale by [`accept`]).
    ///
    /// # Errors
    /// In words: the spelling is not one the sidecar round-trips, no sidecar is running, the jar predates the price lane
    /// or announces no `terms`, the places are all in use by lookups that are waiting, the sidecar is gone or declined
    /// the instrument, or no line arrived in time (a closed market, an instrument the account cannot trade, or a feed
    /// that is not live yet).
    pub fn venue_terms(&self, symbol: &str, wait: Duration) -> Result<VenueTerms, String> {
        let symbol = canonical(symbol)?;
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
                return Err(format!(
                    "dukascopy: the installed JForex sidecar jar predates the venue's terms (its ready line announces no \
                     `{FEATURE_TERMS}`), so the terms of `{symbol}` cannot be read; install the jar from this release \
                     (`just fetch-tools`) and restart"
                ));
            }
        }
        self.hold(&symbol, "subscribed to read its trade terms")?;
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
    ) -> Result<VenueTerms, String> {
        let mut st = self.state();
        loop {
            if let Some(terms) = st.terms.get(symbol) {
                return Ok(terms.clone());
            }
            if let Some(reason) = st.declined.remove(symbol) {
                return Err(format!(
                    "dukascopy: the JForex sidecar declined to subscribe `{symbol}`: {reason}"
                ));
            }
            if st.sidecar.is_none() {
                return Err(format!(
                    "dukascopy: the JForex sidecar went away while the terms of `{symbol}` were being read"
                ));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                st.awaiting.remove(symbol);
                return Err(format!(
                    "dukascopy: the sidecar stated no terms of `{symbol}` within {} ms of asking it to subscribe it \
                     (a closed market: the SDK states an instrument's terms at its first tick; or an instrument \
                     this account cannot trade; or its price feed is not live yet)",
                    wait.as_millis()
                ));
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
        let (port, id, acks, plan, back) = {
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
                if st.priced() >= MAX_PRICED_SYMBOLS
                    && let Some(oldest) = st.least_recently_rated(true)
                    && st.release_rated(&oldest)
                {
                    back.push(oldest);
                }
                if st.priced() >= MAX_PRICED_SYMBOLS {
                    Plan::Refused(format!(
                        "dukascopy: at most {MAX_PRICED_SYMBOLS} instruments are priced at once (the \
                         JForex subscription limit is unmeasured); `{symbol}` was not subscribed"
                    ))
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
            // A pair the rate reader holds too stays subscribed at the sidecar.
            let held_for_a_rate = st.rated.contains_key(&symbol);
            st.sidecar.as_ref().filter(|_| !held_for_a_rate).map(|s| Arc::clone(&s.port))
        };
        self.answered.notify_all(); // a `want` of this very symbol still waiting stops waiting
        if let Some(port) = port {
            let _ = port.send(&encode_line(&Command::Unsubscribe { symbols: vec![symbol] }));
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
    /// In words: the spelling is not one the sidecar round-trips, no sidecar is running, the jar predates the price lane, the
    /// places are all in use by lookups that are waiting, the sidecar is gone or declined the instrument, or no
    /// tick arrived in time (a closed market, or a feed that is not live yet).
    pub fn fresh_tick(&self, symbol: &str, wait: Duration) -> Result<PriceTick, String> {
        let symbol = canonical(symbol)?;
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
    ) -> Result<PriceTick, String> {
        let mut st = self.state();
        loop {
            if let Some(tick) = self.recent(&st, symbol) {
                return Ok(tick);
            }
            if let Some(reason) = st.declined.remove(symbol) {
                return Err(format!(
                    "dukascopy: the JForex sidecar declined to price `{symbol}`: {reason}"
                ));
            }
            if st.sidecar.is_none() {
                return Err(format!(
                    "dukascopy: the JForex sidecar went away while `{symbol}` was being priced"
                ));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                st.awaiting.remove(symbol);
                return Err(format!(
                    "dukascopy: no tick of `{symbol}` arrived within {} ms of asking the sidecar for it (a \
                     closed market, or its price feed is not live yet)",
                    wait.as_millis()
                ));
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
    fn hold_for_rate(&self, symbol: &str) -> Result<(), String> {
        self.hold(symbol, "priced for a conversion")
    }

    /// [`Self::hold_for_rate`] for any purpose that needs the instrument subscribed (`purpose` ends the sentence
    /// "`SYMBOL` cannot be ..."): a rate, or the reading of the instrument's terms ([`Self::venue_terms`]).
    fn hold(&self, symbol: &str, purpose: &str) -> Result<(), String> {
        let _io = self.io();
        let (port, id, plan, back) = {
            let mut st = self.state();
            let Some(sidecar) = st.sidecar.as_ref() else {
                return Err(format!(
                    "dukascopy: no JForex sidecar is running on this node, so `{symbol}` cannot be \
                     {purpose} (the account is not armed at `demo`, its sidecar did not \
                     start, or it is not up yet); the price lane never starts one"
                ));
            };
            if !sidecar.ticks {
                return Err(format!(
                    "dukascopy: the installed JForex sidecar jar predates the price lane (its ready line \
                     announces no `{FEATURE_TICKS}`), so `{symbol}` cannot be {purpose}; \
                     install the jar from this release (`just fetch-tools`) and restart"
                ));
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
                    && st.priced() >= MAX_PRICED_SYMBOLS
                    && let Some(oldest) = st.least_recently_rated(true)
                    && st.release_rated(&oldest)
                {
                    back.push(oldest);
                }
                if st.rated.len() >= MAX_RATED_SYMBOLS {
                    Plan::Refused(format!(
                        "dukascopy: {MAX_RATED_SYMBOLS} conversion pairs are being priced right now, each with a \
                         lookup waiting on its tick; `{symbol}` {NOT_SUBSCRIBED} (ask again)"
                    ))
                } else if !shared && st.priced() >= MAX_PRICED_SYMBOLS {
                    Plan::Refused(format!(
                        "dukascopy: at most {MAX_PRICED_SYMBOLS} instruments are priced at once (the \
                         JForex subscription limit is unmeasured); `{symbol}` {NOT_SUBSCRIBED}"
                    ))
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
            Plan::Refused(why) => return Err(why),
            Plan::Subscribe => {}
        }
        if port.send(&encode_line(&Command::Subscribe { symbols: vec![symbol.to_string()] })) {
            return Ok(());
        }
        // The pipe is closed: the sidecar is gone, and nothing it was asked for is being served.
        {
            let mut st = self.state();
            st.rated.remove(symbol);
            st.drop_want(symbol);
        }
        self.detach(id);
        Err(format!(
            "dukascopy: the JForex sidecar is gone (its command pipe is closed), so `{symbol}` \
             {NOT_SUBSCRIBED}"
        ))
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

/// What `send_subscribe` decided under the state lock, before anything is written.
enum Plan {
    /// Already priced (the consumer's own, or shared with the rate reader's): nothing to write.
    Nothing,
    /// No place for it, in words.
    Refused(String),
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
    /// Ask live quotes for `symbol` (a wire spelling, any case: six letters, or an SDK slashed name for a CFD).
    /// Idempotent for a symbol already asked.
    ///
    /// # Errors
    /// In words: the spelling is not one the sidecar round-trips, no sidecar is running, the installed jar predates the
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

#[cfg(test)]
#[path = "quotes_rate_tests.rs"]
mod quotes_rate_tests;
