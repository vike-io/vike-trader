//! FXCM's PRICE LANE (any-symbol, decision 0174): how a live two-sided quote of a pair the engine was not started on
//! reaches the control door, from a venue that has no market-data seam at all (`DataClientCapabilities::NONE`).
//!
//! # No second login, no new FFI
//!
//! FXCM sessions are scarce and the daemon already holds the account's RECONCILE session, logged in for read-only
//! table questions ([`crate::account::reads`]). The lane **polls the Offers table on that session** (`forex_connect_offers`, the shim's
//! optional read that the conversion rate already uses: one refresh request, one 1 MiB buffer, one call), so it
//! opens nothing and adds no export to the shim (the one write the lane may make, subscribing a pair FXCM holds as
//! not tradable, is [`crate::account::reads::AccountReads::priced_offers`]'s: `crate::instrument::subscription`). It never touches the EXEC session: a price read can never delay an
//! order. The consumer is the gatewayhub's credentialed price stream
//! (the PRICE STREAM below); the two meet on this [`PriceDesk`], as dukascopy's stream and
//! sidecar meet on `vike_dukascopy::feed::QuoteBus`.
//!
//! # What the desk guarantees, and the fail-closed rules
//!
//! * **LAZY.** Nothing is polled until a symbol is asked for ([`Listening::want`]), and the poller thread ends when
//!   the last wanted symbol is let go ([`Listening::unwant`], the idle reaper's close): an unused lane costs no
//!   thread and no venue request.
//! * **BOUNDED.** At most [`FXCM`]'s `max_streams` symbols are wanted at once (the gatewayhub client's row, the ONE
//!   number both daemons read: `crates/vike-gatewayhub-client/src/credentialed_limits.rs`'s `FXCM`; the Offers table is large and the door
//!   admits [`vike_execution`]'s sixteen at most per account); only the WANTED symbols are looked up in the table the
//!   read returned, never the whole of it. One request at a time, and [`POLL_EVERY`] of quiet after each (a request
//!   that fails waits [`FAILURE_BACKOFF`], and a table in which EVERY wanted pair's market is closed waits
//!   [`CLOSED_EVERY`]: a weekend costs a request every half minute, not every two seconds): the lane's load on the
//!   shared session is a request every two seconds at most, whatever the number of symbols. A pair newly wanted
//!   ends the slow wait at once (still [`POLL_EVERY`] after the last request). ⚠ FXCM's own request limits are
//!   UNMEASURED, and the desk reaps nothing itself (it cannot see the engine's holdings): the TRADEHUB lets an
//!   ADMITTED pair go (`TradeLeaseRegistry::reap_idle_admitted`, since v0.1.97) once no account holds anything in it, and
//!   while one is wanted in an open market the poll runs.
//! * **ONE spelling.** The USER name (`USDJPY`, `USOIL.CFD`; decision 0290), the engine's own and the quote's key; the
//!   venue's row (`USD/JPY`, `USOil`) is the one whose user name it is (`crate::account::quote_rate`'s `row_for`).
//! * **A QUOTE IS JUDGED BY ITS OWN TIME** ([`judge`]): the row must be a regular forex instrument
//!   (`InstrumentType` 1: a metal, an index or a crypto CFD subscribed directly is not priced here), subscribed for
//!   trading, its market open (`trading_status` `O`), both sides TRADABLE (`T`: indicative and unflagged sides are
//!   not), positive and uncrossed ([`crate::account::quote_rate::live_sides`]), and its age
//!   ([`crate::account::quote_rate::quote_age`], the row's own time against the session's server time) known, no more
//!   than [`vike_execution::DEFAULT_QUOTE_RATE_TTL_MS`] (120 s) and no more than
//!   [`vike_execution::QUOTE_RATE_FUTURE_SKEW_MS`] (5 s) ahead. The tick is stamped `asked - age`: the moment the
//!   venue was ASKED minus the quote's age, never newer than the quote. A closed market, a frozen quote, a row with
//!   no time and a missing symbol each give NO PRICE, never a stale price made fresh by the refresh that fetched
//!   it. Said once when a market closes (`info`, until it reopens) and, for anything else, at `warn` at most every
//!   ten minutes per pair.
//! * **A live quote is delivered at every poll** (the consumer's own age bound counts from the venue's time, which
//!   the tradehub's tap reads from `ts`: `PriceTap::venue_clocked`). `ts` is `asked - age`, which jitters by the
//!   request's latency from poll to poll for a quote that did not move, so "unchanged" cannot be told by `ts`.
//! * **A FROZEN QUOTE IS CAUGHT TWICE.** The age above rests on two UNMEASURED assumptions: that the row's `getTime()`
//!   is the time of its last price update, and that `getServerTime()` keeps ticking. The backstop needs neither: a
//!   pair whose bid AND ask are bit-identical at every poll for more than [`FROZEN_AFTER`] (120 s) while its market
//!   reads open is refused as frozen until either side moves. (A genuinely static quote of a thin pair is refused
//!   too: fail closed.)
//! * **No session, no price.** With reconciliation off, or the reconcile login failed, no session is registered:
//!   [`Listening::want`] refuses in words and nothing is assumed. A second login is never opened.
//! * **ONE poller, and a dead one is restarted.** A poller's bookkeeping is its identity (a stale one cannot clear a
//!   live one's claim), so a `want` between a poller's last step and its exit cannot start a second. A poller that
//!   dies on a panic (a bug in the session read) leaves its wants in place and RESTARTS ITSELF
//!   ([`Pace::restart_every`], [`RESTART_EVERY`]: production order flow never re-sends a want, so waiting for one
//!   would leave every wanted pair silent); one that dies again inside the throttle stays down, said at `error`,
//!   until the next `want` of ANY wanted pair, a repeat of an old one included, starts a new one.
//! * **A new deadline is never slept past**: the poller re-checks `next_poll` under the lock the wait releases, so a
//!   newly wanted pair that ends a closed-market wait is not missed by a poller already on its way to sleep.
//! * **A consumer fault never stops the poller's thread**: a panicking listener is caught, logged at `error` and
//!   detached (its wants stopped).
//! * **One consumer at a time, and a takeover is loud** (a restarted or redialled datahub replaces its earlier
//!   session; the datahub's broker holds ONE client of this venue for all its holders, decision 0193): the replaced
//!   [`Listening`] refuses while the newer one holds the desk, and takes a free desk back on its next ask
//!   ([`Listening::ensure_current`]), so it is never answered as if it still priced.
//! * It discloses NO link state: a dead session and a closed market both read as silence, so the `fxcm` row of
//!   `vike_venues::link_deadman_default` stays `Inert` and the two hub links speak for the venue (the tradehub's
//!   `price_only_link_disclosure`). What is said is said HERE, in this daemon's log: the failed read once a minute, a
//!   closed market once per closing, any other refusal every ten minutes. Each holder above reads silence (the recorder
//!   writes no row and no marker: `crates/vike-store/src/live_rec.rs`'s `RecorderSink` keeps no status of a quote lane).
//!
//! The session itself is SCRIPTED in every test (docs-shaped rows, `feed_tests.rs`): nothing here has met a
//! ForexConnect session.
//!
//! # The PRICE STREAM over this desk (the consumer, any-symbol, decision 0174)
//!
//! The credentialed stream the datahub's broker opens for `cs_venue=fxcm`, so the tradehub's control door can price a
//! pair its engine was not started on, is `vike_bridge_core::data_client::bus::DataClientBus` over this desk: the
//! shared engine, with everything venue-specific (its words, whether a repeat ask goes back to the source) said by the
//! `DataClientBusSpec` and `DataClientBusSpecHold` impls at the end of this file. MOVED here from the gatewayhub's
//! `history::fxcm_quotes` (owner ruling of 2026-11-12: one-venue code does not live in a shared crate), with its suite
//! (`feed_stream_tests.rs`). Every holder of the broker shares that one client (decision 0193): a second client
//! would be a second consumer of the desk, and would silence the first. The same kind of stream as dukascopy's and
//! for the same reason: nothing is mounted on fxcm (a profile cannot name it: the venue has no market-data seam) and
//! this stream **logs in to nothing**; FXCM sessions are scarce, so the quotes come off the Offers table of the
//! account's RUNNING reconcile session, which the trade lane's reconcile client registered (`crate::account::reads`):
//!
//! * **LAZY, end to end.** Building it dials nothing and asks the venue nothing; the first `subscribe_quotes` (the
//!   door asking a price for a pair) starts the desk's poller, which reads the table every couple of seconds for the
//!   pairs wanted and for no others. No reconcile session (reconciliation off, a failed login, a box whose only FXCM
//!   account is labelled) means no price, said in words ([`Listening::want`]'s refusals): a second login is never
//!   opened.
//! * **QUOTES ONLY.** Bars, trades and the book are refused by name; the price lane needs nothing else.
//! * **REAL-TIME, two-sided quotes only**: the desk has already refused a row that [`judge`] refuses, so every quote
//!   the stream emits is a live one, and the stream's label is sent as real-time on a symbol's first quote
//!   ([`vike_model::StreamDelay::RealTime`]).
//! * **NO link state is disclosed** (the desk's rule above); the words are the desk's, in the gatewayhub's log.
//! * **Bounded** by the desk ([`FXCM`]'s `max_streams`), one request every couple of seconds at most, and one stream at a
//!   time owns the desk (a newer one replaces the older, which the desk warns about and whose asks refuse while the
//!   newer one stands, the symbols it already streamed too). The gatewayhub's session guard holds its ceiling row equal
//!   to that cap (`crates/vike-gatewayhub/src/data/session_guard_bus_tests.rs`).
//!
//! ⚠ **What this needs from the box**: a shim that exports `forex_connect_offers` (an older one is refused in words by the read,
//! once a minute), and an FXCM `demo` account running with reconciliation ON.

use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::{Duration, Instant};

use vike_bridge_core::data_client::bus::{
    DataClientBusSpec, DataClientBusSpecDeliver, DataClientBusSpecHold, DataClientBusSpecKey,
    deliver_quote,
};
use vike_data::DataClientError;
use vike_gatewayhub_client::credentialed_limits::FXCM;
use vike_model::StreamDelay;

use crate::account::quote_rate::{InstrumentType, Offer, live_sides, quote_age};
use crate::refusals::FxcmPriceRefusal;

/// Shared tracing target (the crate's other session-side modules log on `vike_fxcm`).
const TARGET: &str = "vike_fxcm";

/// The registry label of the account whose session is polled: the DEFAULT account's (`""`), the account the
/// gatewayhub's other credentialed streams log in as. A box whose only FXCM account is labelled prices nothing,
/// and [`Listening::want`] says so.
pub const PRICED_ACCOUNT: &str = "";

/// The quiet after each Offers request, so a request is made every two seconds at most (a request is a table
/// refresh the shared session pays for, and a quote that moves faster than this is not a collar's concern).
pub const POLL_EVERY: Duration = Duration::from_secs(2);

/// The quiet after a request that failed (no session, a shim without `forex_connect_offers`, a refresh the venue refused).
pub const FAILURE_BACKOFF: Duration = Duration::from_secs(10);

/// The quiet after a request whose table showed EVERY wanted pair's market closed (a weekend, a holiday): nothing
/// can have a price, so the lane asks half a minute apart. A newly wanted pair ends it at once.
pub const CLOSED_EVERY: Duration = Duration::from_secs(30);

/// How long a pair's bid and ask may stay bit-identical, at every poll, while its market reads open, before it is
/// refused as frozen. The quote's age ([`vike_execution::DEFAULT_QUOTE_RATE_TTL_MS`]) is the same figure.
pub const FROZEN_AFTER: Duration = Duration::from_secs(120);

/// The least time between two RESTARTS of a poller that died on a panic (the first is made at once): a deterministic
/// panic must not become a tight respawn loop. A poller that dies again inside it stays dead until a `want`.
pub const RESTART_EVERY: Duration = Duration::from_secs(10);

/// How often the same Offers-read failure is said at `warn` (the first is said at once).
const SAY_EVERY: Duration = Duration::from_secs(60);

/// How often a pair's lack of a price (anything but a closed market, which is said once per closing) is said at
/// `warn`.
const REFUSED_SAY_EVERY: Duration = Duration::from_secs(600);

/// One live quote the lane accepted: two-sided, uncrossed, from an open market, current by its own time.
#[derive(Debug, Clone, PartialEq)]
pub struct PriceTick {
    /// Canonical symbol (`USDJPY`).
    pub symbol: String,
    /// The bid.
    pub bid: f64,
    /// The ask.
    pub ask: f64,
    /// The quote's own moment, epoch ms on this process's clock: when the venue was asked minus the row's age.
    pub timestamp: i64,
}

/// Where accepted ticks go: called on the poller's thread, one tick at a time. It should be cheap (a slow one
/// delays the next request), and a panic in it detaches the listener.
pub type TickListener = Arc<dyn Fn(&PriceTick) + Send + Sync>;

/// What the desk may ask of the running session: implemented over [`crate::account::reads`] in production, scripted in
/// the tests.
pub trait OffersSource: Send + Sync {
    /// Whether a session exists to ask, judged WITHOUT asking it.
    ///
    /// # Errors
    /// The reason, in words (no reconcile session is running).
    fn ready(&self) -> Result<(), String>;

    /// The CURRENT Offers table, parsed: one refresh through the session (bounded in the shim and by
    /// [`crate::account::reads::OFFERS_WAIT`]).
    ///
    /// # Errors
    /// The session is gone, the installed shim exports no `forex_connect_offers`, or the refresh or the parse failed.
    fn offers(&self) -> Result<Vec<Offer>, String>;

    /// [`Self::offers`], after the rows among `symbols` (user names) that FXCM holds as not tradable have been
    /// subscribed (once per symbol per session): the lane that is asked for a price makes the symbol tradable
    /// itself instead of reporting it "not subscribed". A source that cannot subscribe reads the table as it is.
    ///
    /// # Errors
    /// As [`Self::offers`].
    fn offers_for(&self, _symbols: &[String]) -> Result<Vec<Offer>, String> {
        self.offers()
    }
}

/// The default account's running reconcile session ([`PRICED_ACCOUNT`]).
struct OffersSourceSession;

fn no_session() -> String {
    FxcmPriceRefusal::NoSession.to_string()
}

impl OffersSource for OffersSourceSession {
    fn ready(&self) -> Result<(), String> {
        crate::account::reads::account_reads(PRICED_ACCOUNT).map(|_| ()).ok_or_else(no_session)
    }

    fn offers(&self) -> Result<Vec<Offer>, String> {
        crate::account::reads::account_reads(PRICED_ACCOUNT)
            .ok_or_else(no_session)?
            .offers()
            .map_err(|why| why.to_string())
    }

    fn offers_for(&self, symbols: &[String]) -> Result<Vec<Offer>, String> {
        crate::account::reads::account_reads(PRICED_ACCOUNT)
            .ok_or_else(no_session)?
            .priced_offers(symbols)
            .map_err(|why| why.to_string())
    }
}

/// The one spelling the lane prices and keys a quote by: the USER name (decision 0290: `EURUSD`, `XAUUSD`,
/// `US30.CFD`, `USOIL.CFD`, `NVDA.EXT.CFD`), whose fxcm grammar is upper case, letters, digits and dots. The row it
/// names is found in the table each poll ([`judge`]); the venue's own spelling (`EUR/USD`) is no user name.
fn canonical(symbol: &str) -> Result<String, FxcmPriceRefusal> {
    let up = symbol.trim().to_ascii_uppercase();
    if !up.is_empty()
        && up.len() <= 32
        && up.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.')
    {
        Ok(up)
    } else {
        Err(FxcmPriceRefusal::NotAUserName { symbol: symbol.to_string() })
    }
}

/// **The one judgement of a symbol's quote**: the row of `offers` that names it, as a live two-sided quote
/// stamped with its own moment, or the reason it is not a price.
///
/// # Errors
/// The symbol is not in the table (or names several rows), is of no class the SDK names, is not subscribed for
/// trading, its market is not open, a side is missing, untradable or crossed, or its own time is absent, older than 120 s
/// or more than 5 s ahead of the server. Every class the SDK names is priced like a forex pair: a CFD's quote is the same
/// two tradable sides of the same row.
pub fn judge(symbol: &str, offers: &[Offer], asked_ms: i64) -> Result<PriceTick, FxcmPriceRefusal> {
    let refused = |why| FxcmPriceRefusal::Row { why };
    let row = match crate::account::quote_rate::row_for(offers, symbol).map_err(refused)? {
        Some(row) => row,
        None => return Err(FxcmPriceRefusal::NotListed { symbol: symbol.to_string() }),
    };
    let name = row.instrument.clone();
    if InstrumentType::of(row.instrument_type).is_none() {
        return Err(FxcmPriceRefusal::UnknownType { name, instrument_type: row.instrument_type });
    }
    if row.subscription != "T" {
        return Err(FxcmPriceRefusal::NotSubscribed {
            name,
            subscription: row.subscription.clone(),
        });
    }
    let (bid, ask) = live_sides(row).map_err(refused)?;
    let age = quote_age(row).map_err(refused)?;
    Ok(PriceTick { symbol: symbol.to_string(), bid, ask, timestamp: asked_ms.saturating_sub(age) })
}

struct Listener {
    token: u64,
    call: TickListener,
}

/// The last bid and ask a pair showed, and since when they have been exactly those (the frozen-quote backstop).
struct HeldQuote {
    bits: (u64, u64),
    since: Instant,
}

/// That a pair has no price was said at `at`; `closed` tells a closed market (said once per closing) from any other
/// reason (said at most every [`REFUSED_SAY_EVERY`]).
struct Said {
    at: Instant,
    closed: bool,
}

#[derive(Default)]
struct State {
    /// The symbols a consumer asked prices for (canonical).
    wanted: BTreeSet<String>,
    listener: Option<Listener>,
    /// The consumers a newer one replaced, while their [`Listening`] is held: each may take a free desk back
    /// ([`Listening::ensure_current`]). One a fault detached, or that let go, is not here.
    displaced: BTreeSet<u64>,
    /// One counter for listener tokens (never reused).
    next_token: u64,
    /// Which pairs' lack of a price is already said (cleared when the price returns).
    refused: BTreeMap<String, Said>,
    /// The bid and ask each wanted pair showed at its last poll ([`FROZEN_AFTER`]).
    held: BTreeMap<String, HeldQuote>,
    /// The identity of the poller thread that is running, if one is (it ends, with this cleared, when nothing is
    /// wanted). A poller clears ONLY its own claim, so a stale one can never clear a live one's.
    poller: Option<u64>,
    /// One counter for poller identities (never reused).
    next_poller: u64,
    /// When a poller that died on a panic was last restarted by its own guard ([`Pace::restart_every`]).
    last_restart: Option<Instant>,
    /// Not before: the pacing between two requests. `None` = now.
    next_poll: Option<Instant>,
    /// When the last request ended, and whether the wait after it is the closed-market one (a newly wanted pair
    /// shortens that to `poll_every` after the request).
    last_poll_end: Option<Instant>,
    closed_wait: bool,
    /// The last failure said at `warn`.
    failure_said: Option<(Instant, String)>,
}

/// The desk's clocks, a value so a test can shorten them (production's is [`Pace::PRODUCTION`]).
#[derive(Clone, Copy, Debug)]
pub struct Pace {
    /// The quiet after a request that read a table.
    pub poll_every: Duration,
    /// The quiet after a request that failed.
    pub failure_backoff: Duration,
    /// The quiet after a table that showed every wanted pair's market closed.
    pub closed_every: Duration,
    /// How long a bit-identical bid and ask may stand at every poll before the pair is refused as frozen.
    pub frozen_after: Duration,
    /// The least time between two restarts of a poller that died on a panic.
    pub restart_every: Duration,
}

impl Pace {
    /// [`POLL_EVERY`], [`FAILURE_BACKOFF`], [`CLOSED_EVERY`], [`FROZEN_AFTER`], [`RESTART_EVERY`].
    pub const PRODUCTION: Self = Self {
        poll_every: POLL_EVERY,
        failure_backoff: FAILURE_BACKOFF,
        closed_every: CLOSED_EVERY,
        frozen_after: FROZEN_AFTER,
        restart_every: RESTART_EVERY,
    };
}

/// What the poller does next.
enum PricePollStep {
    /// Nothing is wanted: the thread ends.
    Done,
    /// Not yet: park until this deadline (`next_poll` as the step read it; [`PriceDesk::park`] re-checks it).
    Wait(Instant),
    /// A request was made and judged.
    Polled,
}

/// See the module doc.
pub struct PriceDesk {
    source: Arc<dyn OffersSource>,
    state: Mutex<State>,
    /// Signalled when a symbol is wanted or let go, and when the consumer changes: what a parked poller sleeps on.
    wake: Condvar,
    pace: Pace,
}

impl PriceDesk {
    /// A desk over `source` at the production pace ([`Pace::PRODUCTION`]).
    #[must_use]
    pub fn over(source: Arc<dyn OffersSource>) -> Arc<Self> {
        Self::tuned(source, Pace::PRODUCTION)
    }

    /// [`Self::over`] with the two request clocks chosen (a consumer's test shortens them; production never does).
    /// A closed market is not slowed here and the frozen-quote limit is [`FROZEN_AFTER`]: [`Self::tuned`] sets all
    /// four.
    #[must_use]
    pub fn paced(
        source: Arc<dyn OffersSource>,
        poll_every: Duration,
        failure_backoff: Duration,
    ) -> Arc<Self> {
        Self::tuned(
            source,
            Pace {
                poll_every,
                failure_backoff,
                closed_every: poll_every,
                frozen_after: FROZEN_AFTER,
                restart_every: RESTART_EVERY,
            },
        )
    }

    /// [`Self::over`] with every clock chosen.
    #[must_use]
    pub fn tuned(source: Arc<dyn OffersSource>, pace: Pace) -> Arc<Self> {
        Arc::new(Self { source, state: Mutex::new(State::default()), wake: Condvar::new(), pace })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Become the consumer: `listener` receives every accepted tick, on the poller's thread. A consumer already
    /// listening is REPLACED, loudly (`warn`): its wants are dropped and its [`Listening`] refuses from now on.
    #[must_use]
    pub fn listen(self: &Arc<Self>, listener: TickListener) -> Listening {
        let mut st = self.state();
        st.next_token += 1;
        let token = st.next_token;
        let call = Arc::clone(&listener);
        let older = st.listener.replace(Listener { token, call }).map(|l| l.token);
        st.displaced.extend(older);
        let held: Vec<String> = std::mem::take(&mut st.wanted).into_iter().collect();
        st.refused.clear();
        st.held.clear();
        drop(st);
        if let Some(older) = older {
            tracing::warn!(
                target: TARGET,
                older,
                newer = token,
                ?held,
                "fxcm: price stream #{token} took over the price lane from stream #{older}: the older stream's \
                 prices (the pairs listed) stop NOW and it is told so on its next ask. A restarted or redialled \
                 datahub replaces its own earlier session this way"
            );
        }
        self.wake.notify_all();
        Listening { desk: Arc::clone(self), token, listener }
    }

    fn unlisten(&self, token: u64) {
        let mut st = self.state();
        st.displaced.remove(&token);
        if st.listener.as_ref().is_some_and(|l| l.token == token) {
            st.listener = None;
            st.wanted.clear();
            st.refused.clear();
            st.held.clear();
            drop(st);
            self.wake.notify_all();
        }
    }

    /// One scheduling step of poller `id`: end, wait, or make one request and judge what it returned.
    fn step(&self, id: u64) -> PricePollStep {
        let (wanted, token, call) = {
            let mut st = self.state();
            let held = st.listener.as_ref().map(|l| (l.token, Arc::clone(&l.call)));
            let Some((token, call)) = held.filter(|_| !st.wanted.is_empty()) else {
                // Decided and cleared in ONE hold of the lock, and only this poller's own claim: a `want` after
                // this sees no poller and starts the next one, and nothing the exiting thread does later can
                // clear that one's claim.
                if st.poller == Some(id) {
                    st.poller = None;
                }
                return PricePollStep::Done;
            };
            if let Some(at) = st.next_poll
                && at > Instant::now()
            {
                return PricePollStep::Wait(at);
            }
            (st.wanted.clone(), token, call)
        };
        // Read BEFORE the request goes out: a tick is never stamped newer than the answer.
        let asked_ms = vike_model::now_ms();
        let result = self.source.offers_for(&wanted.iter().cloned().collect::<Vec<_>>());
        let (quiet, closed) = match result {
            Ok(offers) => {
                let closed = self.deliver(&wanted, &offers, asked_ms, token, &call);
                (if closed { self.pace.closed_every } else { self.pace.poll_every }, closed)
            }
            Err(why) => {
                self.say_failure(&why);
                (self.pace.failure_backoff, false)
            }
        };
        let mut st = self.state();
        let now = Instant::now();
        st.next_poll = Some(now + quiet);
        st.last_poll_end = Some(now);
        st.closed_wait = closed && self.pace.closed_every > self.pace.poll_every;
        PricePollStep::Polled
    }

    /// Judge each wanted symbol against the table and hand the live quotes to the consumer. Returns whether EVERY
    /// wanted pair's market was found closed (nothing can be priced until one opens: the slow wait).
    fn deliver(
        &self,
        wanted: &BTreeSet<String>,
        offers: &[Offer],
        asked_ms: i64,
        token: u64,
        call: &TickListener,
    ) -> bool {
        let mut all_closed = true;
        for symbol in wanted {
            let judged = judge(symbol, offers, asked_ms);
            let closed = judged.is_err() && market_closed(symbol, offers);
            all_closed &= closed;
            let tick = {
                let mut st = self.state();
                // Let go (or the consumer replaced) while the request was out: say nothing about it.
                if st.listener.as_ref().map(|l| l.token) != Some(token)
                    || !st.wanted.contains(symbol)
                {
                    continue;
                }
                // A row that is no quote at all (a closed market, a missing or untimed row) restarts the frozen
                // clock; a quote that is one but frozen keeps it running.
                let judged = match judged {
                    Ok(tick) => self.not_frozen(&mut st, tick),
                    Err(why) => {
                        st.held.remove(symbol);
                        Err(why)
                    }
                };
                match judged {
                    Err(why) => {
                        let due = match st.refused.get(symbol) {
                            None => true,
                            Some(said) if said.closed != closed => true,
                            Some(said) => !closed && said.at.elapsed() >= REFUSED_SAY_EVERY,
                        };
                        if due {
                            if closed {
                                tracing::info!(
                                    target: TARGET,
                                    symbol = %symbol,
                                    "{why} (said once until the market reopens)"
                                );
                            } else {
                                tracing::warn!(
                                    target: TARGET,
                                    symbol = %symbol,
                                    "{why} (said at most every ten minutes; the price returns when the quote is live again)"
                                );
                            }
                            st.refused.insert(symbol.clone(), Said { at: Instant::now(), closed });
                        }
                        None
                    }
                    Ok(tick) => {
                        if st.refused.remove(symbol).is_some() {
                            tracing::info!(target: TARGET, symbol = %symbol, "fxcm: {symbol} is priced again");
                        }
                        Some(tick)
                    }
                }
            };
            if let Some(tick) = tick
                && !self.hand_over(token, call, &tick)
            {
                return false;
            }
        }
        all_closed
    }

    /// **The frozen-quote backstop** ([`FROZEN_AFTER`]): `tick` passes unless its bid and ask have been bit-identical
    /// at every poll for longer than the limit. The held pair's clock restarts whenever either side moves, and is
    /// dropped when the pair has no quote at all (a closed market, a missing row).
    fn not_frozen(&self, st: &mut State, tick: PriceTick) -> Result<PriceTick, FxcmPriceRefusal> {
        let bits = (tick.bid.to_bits(), tick.ask.to_bits());
        let since = match st.held.get(&tick.symbol) {
            Some(held) if held.bits == bits => held.since,
            _ => Instant::now(),
        };
        st.held.insert(tick.symbol.clone(), HeldQuote { bits, since });
        if since.elapsed() > self.pace.frozen_after {
            return Err(FxcmPriceRefusal::Frozen {
                symbol: tick.symbol,
                bid: tick.bid,
                ask: tick.ask,
                seconds: since.elapsed().as_secs(),
            });
        }
        Ok(tick)
    }

    /// Give `tick` to the consumer; `false` when it panicked (and was detached with its wants).
    fn hand_over(&self, token: u64, call: &TickListener, tick: &PriceTick) -> bool {
        if catch_unwind(AssertUnwindSafe(|| call(tick))).is_ok() {
            return true;
        }
        tracing::error!(
            target: TARGET,
            symbol = %tick.symbol,
            "fxcm: the price consumer panicked on a tick: it is detached and its prices stop (a newer stream \
             starts clean)"
        );
        self.unlisten(token);
        false
    }

    fn say_failure(&self, why: &str) {
        let mut st = self.state();
        let due = st
            .failure_said
            .as_ref()
            .is_none_or(|(at, said)| said != why || at.elapsed() >= SAY_EVERY);
        if due {
            st.failure_said = Some((Instant::now(), why.to_string()));
            drop(st);
            tracing::warn!(
                target: TARGET,
                "fxcm: the Offers table could not be read, so the wanted pairs have no price: {why} (said once \
                 a minute; the next try is in {} s)",
                self.pace.failure_backoff.as_secs()
            );
        }
    }

    /// Sleep until `until` (or a wake-up). The deadline is re-checked UNDER THE SAME HOLD OF THE LOCK that the wait
    /// releases (the review's F-5): a `want` that moved `next_poll` between the step that read it and this call
    /// has already notified, and a poller that slept on the old deadline would miss it (up to the 30 s closed wait).
    /// A moved deadline returns at once, and the loop steps again.
    fn park(&self, until: Instant) {
        let st = self.state();
        if st.next_poll != Some(until) {
            return;
        }
        let left = until.saturating_duration_since(Instant::now());
        drop(self.wake.wait_timeout(st, left).unwrap_or_else(PoisonError::into_inner));
    }

    /// Start a poller thread and record its claim (the caller holds the lock as `st`). The thread is detached; it
    /// holds a `Weak` and ends when nothing is wanted.
    ///
    /// # Errors
    /// The thread could not be started (the claim is then not recorded).
    fn start_poller(self: &Arc<Self>, st: &mut State) -> Result<(), FxcmPriceRefusal> {
        st.next_poller += 1;
        let id = st.next_poller;
        let weak = Arc::downgrade(self);
        match std::thread::Builder::new()
            .name("fxcm-quotes".into())
            .spawn(move || run_poller(&weak, id))
        {
            Ok(_) => {
                st.poller = Some(id);
                Ok(())
            }
            Err(e) => Err(FxcmPriceRefusal::PollerNotStarted { error: e.to_string() }),
        }
    }
}

/// Whether `symbol`'s row is in the table and its market is not open (`trading_status` other than `O`).
fn market_closed(symbol: &str, offers: &[Offer]) -> bool {
    crate::account::quote_rate::row_for(offers, symbol)
        .ok()
        .flatten()
        .is_some_and(|o| o.trading_status != "O")
}

/// Ends poller `id`'s claim however its thread ends, so a panic leaves the next `want` free to start another. Only
/// ITS OWN claim: the poller's last `step` already cleared it, and a `want` may have installed a successor since.
///
/// **A poller that dies on a PANIC (a bug in the session read) restarts ITSELF** (the cold re-review's F-4), when a
/// consumer and wants are still there: production order flow never re-sends a want (the tradehub's watcher
/// de-duplicates a repeat subscription), so waiting for one would leave every wanted pair silent until a restart.
/// The restart is throttled ([`Pace::restart_every`]): a poller that dies again inside it stays dead, said at
/// `error`, until a `want` (a deterministic panic is not a tight respawn loop).
struct PollerEnd(Weak<PriceDesk>, u64);

impl Drop for PollerEnd {
    fn drop(&mut self) {
        let Some(desk) = self.0.upgrade() else { return };
        let mut st = desk.state();
        if st.poller != Some(self.1) {
            return; // not this poller's claim any more (it ended cleanly, a successor holds it)
        }
        st.poller = None;
        if !std::thread::panicking() || st.listener.is_none() || st.wanted.is_empty() {
            return;
        }
        if st.last_restart.is_some_and(|at| at.elapsed() < desk.pace.restart_every) {
            tracing::error!(
                target: TARGET,
                "fxcm: the price poller died on a panic again within {} s of its last restart: it stays down \
                 until a pair is asked for again (see the panic above)",
                desk.pace.restart_every.as_secs()
            );
            return;
        }
        st.last_restart = Some(Instant::now());
        match desk.start_poller(&mut st) {
            Ok(()) => tracing::error!(
                target: TARGET,
                "fxcm: the price poller died on a panic (see above) and was restarted; its wants stand"
            ),
            Err(why) => tracing::error!(target: TARGET, "{why}; the wanted pairs have no price"),
        }
    }
}

fn run_poller(desk: &Weak<PriceDesk>, id: u64) {
    let _end = PollerEnd(desk.clone(), id);
    loop {
        // Strong only for one step: the thread never keeps a dropped desk alive.
        let Some(desk) = desk.upgrade() else { return };
        match desk.step(id) {
            PricePollStep::Done => return,
            PricePollStep::Wait(until) => desk.park(until),
            PricePollStep::Polled => {}
        }
    }
}

/// A consumer's hold on the desk ([`PriceDesk::listen`]). Dropping it stops its prices and lets the poller end.
pub struct Listening {
    desk: Arc<PriceDesk>,
    token: u64,
    /// Kept so a stream a newer one replaced can take a free desk back ([`Self::ensure_current`]).
    listener: TickListener,
}

impl Listening {
    /// Make sure this is the desk's consumer. A stream a newer one replaced takes the desk back once nobody holds it
    /// (decision 0193 verdict 4: the replacement lasts as long as the replacing stream). There is still one consumer:
    /// an empty slot is the only way in, and the next [`PriceDesk::listen`] replaces this one like any other. Its
    /// wants went with the takeover, so what it held stays stopped until it asks again.
    ///
    /// # Errors
    /// A newer stream holds the desk, or a fault in its consumer ended it.
    pub fn ensure_current(&self) -> Result<(), FxcmPriceRefusal> {
        let mut st = self.desk.state();
        if st.listener.is_none() && st.displaced.remove(&self.token) {
            st.listener = Some(Listener { token: self.token, call: Arc::clone(&self.listener) });
            tracing::info!(
                target: TARGET,
                token = self.token,
                "fxcm: price stream #{} took the price lane back: the stream that replaced it has ended. The \
                 pairs it held stay stopped until it asks for them again",
                self.token
            );
        }
        let current = st.listener.as_ref().map(|l| l.token) == Some(self.token);
        if current { Ok(()) } else { Err(FxcmPriceRefusal::NotCurrent) }
    }

    /// Ask prices for `symbol` (an instrument's user name: `EURUSD`, `XAUUSD`, `US30.CFD`). A symbol already wanted costs no place and
    /// answers `Ok`, but still makes sure a poller is running (a poller that died leaves its wants in place, and
    /// this is what starts the next). The first want starts the poller; its first request is made at once unless
    /// the pacing says to wait, and a pair newly wanted ends a closed-market wait ([`CLOSED_EVERY`]) as soon as
    /// [`POLL_EVERY`] has passed since the last request.
    ///
    /// # Errors
    /// This consumer was replaced, `symbol` is not an instrument name, there is no session to ask, the cap
    /// ([`FXCM`]'s `max_streams`: the engine admits sixteen extra symbols per account, `vike_execution::MAX_EXTRA_SYMBOLS`,
    /// and the Offers table behind them holds the venue's whole universe) is reached, or the poller thread could not start.
    pub fn want(&self, symbol: &str) -> Result<(), FxcmPriceRefusal> {
        self.ensure_current()?;
        let symbol = canonical(symbol)?;
        self.desk.source.ready().map_err(|why| FxcmPriceRefusal::SourceNotReady { why })?;
        let mut st = self.desk.state();
        if st.listener.as_ref().map(|l| l.token) != Some(self.token) {
            return Err(FxcmPriceRefusal::NotCurrent);
        }
        let new = !st.wanted.contains(&symbol);
        if new && st.wanted.len() >= FXCM.max_streams {
            return Err(FxcmPriceRefusal::AtCap { cap: FXCM.max_streams, symbol });
        }
        st.wanted.insert(symbol.clone());
        if new && st.closed_wait {
            let poll_every = self.desk.pace.poll_every;
            st.next_poll = st.last_poll_end.map(|ended| ended + poll_every);
            st.closed_wait = false;
        }
        if st.poller.is_none()
            && let Err(why) = self.desk.start_poller(&mut st)
        {
            if new {
                st.wanted.remove(&symbol);
            }
            return Err(why);
        }
        drop(st);
        self.desk.wake.notify_all();
        Ok(())
    }

    /// Stop pricing `symbol` (a symbol not wanted is ignored). The place it held is free at once, and the poller
    /// ends when nothing is left.
    pub fn unwant(&self, symbol: &str) {
        let Ok(symbol) = canonical(symbol) else { return };
        let mut st = self.desk.state();
        if st.listener.as_ref().map(|l| l.token) == Some(self.token) {
            st.wanted.remove(&symbol);
            st.refused.remove(&symbol);
            st.held.remove(&symbol);
            drop(st);
            self.desk.wake.notify_all();
        }
    }
}

impl Drop for Listening {
    fn drop(&mut self) {
        self.desk.unlisten(self.token);
    }
}

/// The process's ONE price desk, over the default account's running reconcile session: the gatewayhub's price
/// stream listens here. Process-wide because the session is registered by the trade lane's reconcile client and
/// the stream is built by the data lane, on another thread and another schedule.
#[must_use]
pub fn process_desk() -> Arc<PriceDesk> {
    static DESK: OnceLock<Arc<PriceDesk>> = OnceLock::new();
    Arc::clone(DESK.get_or_init(|| PriceDesk::over(Arc::new(OffersSourceSession))))
}

/// The desk as a price stream's source (`vike_bridge_core::data_client::bus`; the stream is the gatewayhub's
/// `cs_venue=fxcm`, this module doc's "The PRICE STREAM over this desk").
impl DataClientBusSpec for PriceDesk {
    type Hold = Listening;
    const VENUE: &'static str = crate::reconcile::VENUE;
    const CLOSED: &'static str = "fxcm: the price stream is closed";
    const BARS: Result<(), &'static str> = Err("fxcm's price stream carries quotes only (no bars)");
    const TRADES: Result<(), &'static str> =
        Err("fxcm's price stream carries quotes only (no trades)");
    const BOOK: Result<(), &'static str> = Err("fxcm's price stream carries quotes only (no book)");
    /// The desk caps itself (`credentialed_limits::FXCM.max_streams`, in its own words).
    const MAX_SYMBOLS: Option<(usize, &'static str)> = None;
    /// `judge` judged every row already.
    const PRICE_CHECK: bool = false;

    fn consume(self: &Arc<Self>, deliver: DataClientBusSpecDeliver) -> Listening {
        // The Offers table carries no size. Every row is real-time: `judge` refused any that is not live.
        PriceDesk::listen(
            self,
            Arc::new(move |t: &PriceTick| {
                deliver_quote(
                    &deliver,
                    &t.symbol,
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
        Listening::ensure_current(self).map_err(|why| why.to_string())
    }
    /// A repeat ask still goes to the desk's `want` (idempotent there): a poller that died on a panic is started
    /// again by the next ask for ANY pair it was serving, not only by a first one. Only the quote lane reaches
    /// here (the others are refused first).
    fn ask(&self, key: &DataClientBusSpecKey, _streaming: bool) -> Result<(), DataClientError> {
        Listening::want(self, key.symbol())
            .map_err(|why| DataClientError::Subscribe(why.to_string()))
    }
    fn unwant(&self, key: &DataClientBusSpecKey) {
        Listening::unwant(self, key.symbol());
    }
}

#[path = "feed_tests.rs"]
#[cfg(test)]
mod feed_tests;

#[path = "feed_stream_tests.rs"]
#[cfg(test)]
mod feed_stream_tests;
