//! The feed-object types the daemon holds from mount to teardown.
//!
//! Every price feed this daemon holds is a dial to the datahub's STRATEGY LANE
//! (`vike_datahub_client::strat::StratFeed`, decision 0164 verdict 2): the venue sockets are the
//! gatewayhub's, and the datahub carries them to this process over the tube. What lives here is only
//! what the daemon must hold to stop them in the right order: the feed, and the subscriptions it took on
//! it.
//!
//! # Release before shutdown
//!
//! A feed that is shut down with its subscriptions still held leaves the strat-link gaps it disclosed
//! open (`StratFeed`'s link-loss rule: a `Disconnected` is always closed by a `Live` from the same source
//! or by process exit). Every feed here therefore **unsubscribes what it holds, then shuts down**
//! ([`Tracked`], [`CexFeed`]): a gap that is open when the daemon stops is closed by that last unsubscribe
//! rather than left for the dead-man to count.

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use vike_data::delivery::Delivery;
use vike_data::{DataClient, LiveDataError, StreamLabel, SubscriptionId};
use vike_datahub_client::strat::StratFeed;

use super::price_tap::PriceTap;

#[cfg(doc)]
use super::{FeedCtors, wire_venue_feeds};

/// A [`DataClient`] that releases every subscription it handed out before it shuts down.
///
/// It forwards every verb to the feed it wraps and remembers the ids that came back `Ok`; `unsubscribe`
/// forgets one; [`DataClient::shutdown`] unsubscribes what is left, THEN shuts the feed down. That is the
/// whole of it: it exists so the teardown order does not depend on each arm remembering its ids.
pub struct Tracked {
    inner: Box<dyn DataClient + Send>,
    ids: Vec<SubscriptionId>,
    /// Whether [`DataClient::shutdown`] ran: a feed dropped WITHOUT it (an arm that met a refusal after its first
    /// subscription and returned `Err`) is shut down in order by `Drop`; one that was shut down is not again.
    closed: bool,
}

impl Tracked {
    /// Track `inner`'s subscriptions.
    pub fn new(inner: Box<dyn DataClient + Send>) -> Self {
        Self { inner, ids: Vec::new(), closed: false }
    }

    fn keep(
        &mut self,
        r: Result<SubscriptionId, LiveDataError>,
    ) -> Result<SubscriptionId, LiveDataError> {
        if let Ok(id) = &r {
            self.ids.push(*id);
        }
        r
    }
}

impl DataClient for Tracked {
    fn subscribe_bars(
        &mut self,
        symbol: &str,
        interval: &str,
    ) -> Result<SubscriptionId, LiveDataError> {
        let r = self.inner.subscribe_bars(symbol, interval);
        self.keep(r)
    }
    fn subscribe_quotes(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        let r = self.inner.subscribe_quotes(symbol);
        self.keep(r)
    }
    fn subscribe_trades(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        let r = self.inner.subscribe_trades(symbol);
        self.keep(r)
    }
    fn subscribe_book(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        let r = self.inner.subscribe_book(symbol);
        self.keep(r)
    }
    fn subscribe_depth(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        let r = self.inner.subscribe_depth(symbol);
        self.keep(r)
    }
    fn unsubscribe(&mut self, id: SubscriptionId) {
        self.ids.retain(|held| *held != id);
        self.inner.unsubscribe(id);
    }
    fn set_delivery(
        &mut self,
        symbol: &str,
        lane: StreamLabel,
        class: Delivery,
    ) -> Result<(), LiveDataError> {
        self.inner.set_delivery(symbol, lane, class)
    }
    fn is_dead(&self, id: SubscriptionId) -> bool {
        self.inner.is_dead(id)
    }
    fn begin_shutdown(&mut self) {
        self.inner.begin_shutdown();
    }
    fn shutdown(&mut self) {
        self.closed = true;
        for id in std::mem::take(&mut self.ids) {
            self.inner.unsubscribe(id);
        }
        self.inner.shutdown();
    }
}

impl Drop for Tracked {
    /// **A partial feed is released in order too.** A feed arm that refuses on its second subscription drops the
    /// feed it built (`?` returns the `Err`); without this it was torn down by the feed's own `Drop` alone, which
    /// closes the strat-link gaps but unsubscribes nothing in the order the daemon keeps.
    fn drop(&mut self) {
        if !self.closed {
            DataClient::shutdown(self);
        }
    }
}

/// The CEX feed of one venue: ONE strategy-lane dial carrying both lanes the maker needs, and the
/// subscriptions it took on each, so they are released in the order the daemon has always stopped them.
///
/// ⚠ **Two lanes, and the pair is the whole point of the CEX arm.** binance/bybit/okx/aster declare
/// `live_data.quotes = false` and `live_data.book = false` (`vike_venues::venue_caps`), so
/// `DataClient::subscribe_quotes` is a caps-driven refusal and the hyperliquid arm's model cannot be
/// copied. The obvious substitute, `subscribe_depth`, is a SILENT NO-OP: it emits
/// `LiveDataSink::l2_snapshot`, whose body is the trait's default and which NEITHER sink this daemon owns
/// overrides (`vike_tradehub_core::CoreLaneSink` says so in its module doc). A depth-wired maker connects,
/// seeds, validates checksums, reports healthy, and posts zero orders forever, because
/// `vike_mm::SpreadMaker` reaches `requote` from exactly `on_quote_tick` and `on_order_book`.
///
/// * **BARS** (`subscribe_bars`) are the cadence of the account-wide margin-call watchdog and the
///   equity-drawdown latch (`CoreConfig::margin_call` / `max_drawdown`), which fire per CLOSED bar and
///   nowhere else. A quote-only CEX mount would run a live account with BOTH safety watchdogs dead.
/// * **TICKS** (`StratFeed::subscribe_ticks`, the tube's `SubOp::Ticks`) are the venue's quote/trade/book
///   pump: the lane the maker acts on, and this daemon's ONLY link-death signal for a CEX venue (the pump
///   discloses its own transport state, which arms the CONNECTION-state dead-man;
///   `crates/vike-tradehub/src/tradehub_cli/tests/deadman.rs`'s
///   `the_cex_arm_subscribes_the_tick_pump_that_reports_a_dead_link` is the gate that refuses to let that
///   happen quietly). Its book arrives as an anchor plus deltas and is folded back into the book the
///   engine always saw by `feeds/pump_book.rs`.
///
/// Every CEX venue here PUBLISHES a quote and drives BOTH maker verbs; what differs is only where the
/// quote comes from ([`crate::CexVenue::quote_source`]).
pub struct CexFeed {
    inner: Arc<CexShared>,
    tap: Arc<PriceTap>,
}

/// What the feed holds, shared with every [`CexWatcher`], so a symbol admitted at the control door can
/// subscribe its tick pump on the SAME dial and the teardown still releases it in order.
///
/// Two locks, deliberately: `client` is the dial (a subscribe or an unsubscribe is a datahub round trip
/// that needs `&mut StratFeed`, so those serialise on it, and only on it), and `book` is the
/// bookkeeping, which is NEVER held across one. The watcher\'s checks and the teardown\'s flag read the
/// book without queueing behind a subscribe.
struct CexShared {
    client: Mutex<StratFeed>,
    book: Mutex<Book>,
}

struct Book {
    bars: Vec<SubscriptionId>,
    ticks: Vec<SubscriptionId>,
    /// The tick pumps a [`CexWatcher`] subscribed for symbols that were not mounted, by symbol. Only
    /// [`CexWatcher::close_ticks`] unsubscribes one before the teardown, and its caller does that for a
    /// symbol that was never admitted and has no venue gap open: closing a lane under a gap would leave
    /// the core\'s link dead-man window for (venue, symbol) unclosed, and it trips after its grace.
    watched: HashMap<String, SubscriptionId>,
    /// Symbols a watcher is subscribing right now (the round trip runs with no `book` lock held).
    opening: std::collections::HashSet<String>,
    /// Whether [`CexFeed::shutdown`] ran (see [`Tracked`]\'s field of the same name).
    closed: bool,
}

impl CexShared {
    fn client(&self) -> MutexGuard<'_, StratFeed> {
        self.client.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn book(&self) -> MutexGuard<'_, Book> {
        self.book.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl CexFeed {
    /// A CEX feed over `client`, holding nothing yet; `tap` is where its sink records prices.
    pub fn new(client: StratFeed, tap: Arc<PriceTap>) -> Self {
        Self {
            inner: Arc::new(CexShared {
                client: Mutex::new(client),
                book: Mutex::new(Book {
                    bars: Vec::new(),
                    ticks: Vec::new(),
                    watched: HashMap::new(),
                    opening: std::collections::HashSet::new(),
                    closed: false,
                }),
            }),
            tap,
        }
    }

    /// Subscribe the BAR lane of `symbol` at `interval`.
    pub(super) fn subscribe_bars(
        &mut self,
        symbol: &str,
        interval: &str,
    ) -> Result<(), LiveDataError> {
        let id = self.inner.client().subscribe_bars(symbol, interval)?;
        self.inner.book().bars.push(id);
        Ok(())
    }

    /// Subscribe the tick pump of `symbol`, its book on a `tick_size` grid.
    pub(super) fn subscribe_ticks(
        &mut self,
        symbol: &str,
        tick_size: f64,
    ) -> Result<(), LiveDataError> {
        let id = self.inner.client().subscribe_ticks(symbol, tick_size)?;
        self.inner.book().ticks.push(id);
        Ok(())
    }

    /// Whether the strategy-lane session is open and acknowledged right now.
    pub fn is_up(&self) -> bool {
        self.inner.client().is_up()
    }

    /// A handle that subscribes more tick pumps on this dial and reads the prices they deliver
    /// (decision 0174: a symbol admitted at the control door needs a price).
    pub fn watcher(&self) -> CexWatcher {
        let inner = Arc::clone(&self.inner);
        let feed: Arc<dyn PriceLane> = inner;
        CexWatcher { feed, tap: Arc::clone(&self.tap) }
    }

    /// Release the pump FIRST (the lane the strategy acts on, so no quote can arrive for a bar window
    /// that is already closing), then the bars, then close the dial.
    pub fn shutdown(&mut self) {
        let (watched, ticks, bars) = {
            let mut book = self.inner.book();
            book.closed = true;
            (
                std::mem::take(&mut book.watched),
                std::mem::take(&mut book.ticks),
                std::mem::take(&mut book.bars),
            )
        };
        let mut client = self.inner.client();
        for id in watched.into_values().chain(ticks).chain(bars) {
            client.unsubscribe(id);
        }
        client.shutdown();
    }
}

impl Drop for CexFeed {
    /// A CEX feed whose pump subscription was refused after its kline subscription succeeded is released in
    /// the order a shutdown keeps (pump first, then bars, then the dial), not just dropped.
    fn drop(&mut self) {
        let closed = self.inner.book().closed;
        if !closed {
            self.shutdown();
        }
    }
}

/// What a [`CexWatcher`] opens a price subscription on: a venue's feed dial, shared with the feed's
/// teardown. A lane is closed only through [`PriceLane::close`], by [`CexWatcher::close_ticks`] (the idle reaper).
trait PriceLane: Send + Sync {
    /// Subscribe the price lane of `symbol` once (a repeat, or a subscribe already in flight, is `Ok` and
    /// does nothing); `tick_size` is the grid of a pump that books.
    fn subscribe(&self, symbol: &str, tick_size: f64) -> Result<(), LiveDataError>;

    /// Unsubscribe the lane a watcher opened for `symbol` (never a mount's own: those live to the
    /// teardown); `true` when one was closed. The datahub round trip runs with no `book` lock held. The
    /// caller owns the rule that no venue gap is open for the symbol.
    fn close(&self, symbol: &str) -> bool;
}

impl PriceLane for CexShared {
    fn close(&self, symbol: &str) -> bool {
        let id = self.book().watched.remove(symbol);
        if let Some(id) = id {
            self.client().unsubscribe(id);
        }
        id.is_some()
    }

    fn subscribe(&self, symbol: &str, tick_size: f64) -> Result<(), LiveDataError> {
        {
            let mut book = self.book();
            if book.closed {
                return Err(LiveDataError::Subscribe("the price feed is shut down".to_string()));
            }
            if book.watched.contains_key(symbol) || !book.opening.insert(symbol.to_string()) {
                return Ok(());
            }
        }
        // The round trip: the dial's lock only, no bookkeeping lock held.
        let subscribed = self.client().subscribe_ticks(symbol, tick_size);
        let mut book = self.book();
        book.opening.remove(symbol);
        let id = subscribed?;
        if book.closed {
            drop(book);
            self.client().unsubscribe(id);
        } else {
            book.watched.insert(symbol.to_string(), id);
        }
        Ok(())
    }
}

/// A cloneable handle on one venue's feed dial: subscribes the price lane of a symbol that was not
/// mounted (a CEX venue's tick pump, [`CexFeed`]; Hyperliquid's quote lane, [`QuoteFeed`]) and reads
/// its latest price. After the feed's shutdown it refuses.
#[derive(Clone)]
pub struct CexWatcher {
    feed: Arc<dyn PriceLane>,
    tap: Arc<PriceTap>,
}

impl CexWatcher {
    /// Subscribe the price lane of `symbol`, once: a CEX tick pump on a `tick_size` grid (Hyperliquid's
    /// quote lane has no grid and ignores it).
    pub fn subscribe_ticks(&self, symbol: &str, tick_size: f64) -> Result<(), LiveDataError> {
        self.feed.subscribe(symbol, tick_size)
    }

    /// The latest price the lane delivered for `symbol` on `venue`.
    pub fn price(&self, venue: &str, symbol: &str) -> Option<f64> {
        self.tap.price(venue, symbol)
    }

    /// **The price as the control door asks it**: [`Self::price`], or the refusal in words when the venue's tap
    /// refuses a stream (ibkr's real-time-only tap names a delayed or frozen one: [`PriceTap::door_price`]).
    ///
    /// # Errors
    /// The tap's refusal.
    pub fn door_price(&self, venue: &str, symbol: &str) -> Result<Option<f64>, String> {
        self.tap.door_price(venue, symbol)
    }

    /// Whether the venue's own socket has a gap open for `symbol` ([`PriceTap::gap_open`]).
    pub fn gap_open(&self, venue: &str, symbol: &str) -> bool {
        self.tap.gap_open(venue, symbol)
    }

    /// **Close the tick pump this watcher subscribed for `symbol`** (a symbol that was never admitted and
    /// that nobody asked about for a while), and forget its price. Only a pump of [`Self::subscribe_ticks`]
    /// is touched: the mount's own pumps are `CexFeed::subscribe_ticks`'s and live to the teardown. ⚠ The
    /// caller owns the rule that no venue gap is open for the symbol ([`Self::gap_open`]): a lane closed
    /// under one leaves the core's link dead-man window for it without its `Live`. `true` when a pump was
    /// closed. The datahub round trip runs with no `book` lock held.
    pub fn close_ticks(&self, venue: &str, symbol: &str) -> bool {
        let closed = self.feed.close(symbol);
        self.tap.forget(venue, symbol);
        closed
    }
}

/// Hyperliquid's feed: the [`Tracked`] dial the arm built, shared with the [`CexWatcher`] a symbol
/// admitted at the control door subscribes its quote lane through (decision 0174), so the teardown
/// still releases everything in order. Hyperliquid has no tick pump: a quote subscription of the
/// symbol delivers its price, and the tap the arm put in front of the core's sink records it. Two
/// locks, as [`CexFeed`]: the dial (a subscribe is a datahub round trip) and the bookkeeping, which is
/// never held across one.
pub struct QuoteFeed {
    inner: Arc<QuoteShared>,
    tap: Arc<PriceTap>,
}

/// How long a capped feed ([`QuoteFeed::with_lane_cap`]) refuses every subscribe after one failed: a retried
/// order must not spend an IG login each.
const LANE_FAILURE_TTL: Duration = Duration::from_secs(30);

/// What a [`QuoteFeed`] holds, shared with every [`CexWatcher`] it hands out.
///
/// ⚠ **No lock is held across a datahub round trip.** A first ctrader handshake takes ~12 s and an IG
/// subscribe is a login, so [`PriceLane::subscribe`] LENDS the dial out of `client` ([`Lent`]), runs the round
/// trip with neither lock held and gives it back; [`PriceLane::close`] and [`QuoteFeed::shutdown`] meanwhile
/// never wait for it (a close is deferred to the giving back, a shutdown is finished by it). Other
/// subscribers still queue for the dial on `returned`, as they queued on the old mutex: the feed has ONE
/// `&mut` dial.
struct QuoteShared {
    client: Mutex<Dial>,
    /// Signalled when the dial is given back or shut down.
    returned: Condvar,
    book: Mutex<QuoteBook>,
}

/// The dial of a [`QuoteShared`], and what happened to it while a subscriber had it out.
struct Dial {
    /// `None` while a subscriber has it out ([`Lent`]).
    feed: Option<Tracked>,
    /// Lanes closed while the dial was out: unsubscribed when it comes back.
    deferred: Vec<SubscriptionId>,
    /// [`QuoteFeed::shutdown`] ran: the dial is closed as soon as it is in hand, and nothing takes it again.
    shut: bool,
}

struct QuoteBook {
    /// The quote lanes a watcher opened for symbols that were not mounted, by symbol: unsubscribed
    /// only by the idle reaper ([`PriceLane::close`]) or the teardown.
    watched: HashMap<String, SubscriptionId>,
    /// Symbols a watcher is subscribing right now (the round trip runs with no `book` lock held).
    opening: std::collections::HashSet<String>,
    /// Whether [`QuoteFeed::shutdown`] ran.
    closed: bool,
    /// The most quote lanes a watcher may hold open at once (`None` = unbounded, every venue but ig): a
    /// subscription beyond it is refused, naming the cap.
    cap: Option<usize>,
    /// How long a failed subscribe is remembered ([`QuoteFeed::with_lane_cap`]; `None` = not remembered).
    failure_ttl: Option<Duration>,
    /// The last failed subscribe and its words: while it is younger than `failure_ttl` every subscribe is
    /// refused with it, so a retried order cannot spend a login each (a failing login is not a lane, so
    /// the cap never counted it). A success clears it.
    failed: Option<(Instant, String)>,
}

impl QuoteShared {
    fn client(&self) -> MutexGuard<'_, Dial> {
        self.client.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn book(&self) -> MutexGuard<'_, QuoteBook> {
        self.book.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Take the dial out for a round trip, waiting (on no lock) while another subscriber has it.
    fn lend(&self) -> Result<Lent<'_>, LiveDataError> {
        let mut dial = self.client();
        loop {
            if dial.shut {
                return Err(LiveDataError::Subscribe("the price feed is shut down".to_string()));
            }
            if let Some(feed) = dial.feed.take() {
                return Ok(Lent { shared: self, feed: Some(feed), release: None });
            }
            dial = self.returned.wait(dial).unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// The words of a subscribe that failed less than the failure memory ago, if any.
    fn remembered_failure(&self) -> Option<String> {
        let book = self.book();
        let (at, why) = book.failed.as_ref()?;
        let ttl = book.failure_ttl?;
        (at.elapsed() < ttl)
            .then(|| format!("the last subscribe failed ({why}); not retried for {ttl:?}"))
    }

    /// Unsubscribe `id` now, or when the dial comes back if a subscriber has it out. Never waits.
    fn release(&self, id: SubscriptionId) {
        let mut dial = self.client();
        match dial.feed.as_mut() {
            Some(feed) => feed.unsubscribe(id),
            None => dial.deferred.push(id),
        }
    }
}

/// The dial of a [`QuoteShared`] while a subscriber has it out. Giving it back (also by unwinding) applies
/// the closes that came meanwhile, `release` (a lane the subscriber itself must undo) and a shutdown that
/// came meanwhile, then wakes the next subscriber.
struct Lent<'a> {
    shared: &'a QuoteShared,
    feed: Option<Tracked>,
    release: Option<SubscriptionId>,
}

impl Lent<'_> {
    fn feed(&mut self) -> &mut Tracked {
        self.feed.as_mut().expect("the dial is in hand until it is given back")
    }
}

impl Drop for Lent<'_> {
    fn drop(&mut self) {
        let Some(mut feed) = self.feed.take() else { return };
        let mut dial = self.shared.client();
        for id in dial.deferred.drain(..).chain(self.release.take()) {
            feed.unsubscribe(id);
        }
        if dial.shut {
            feed.shutdown();
        }
        dial.feed = Some(feed);
        self.shared.returned.notify_all();
    }
}

impl QuoteFeed {
    /// The feed over `feed`, holding no extra symbol yet; `tap` is where its sink records prices.
    pub fn new(feed: Tracked, tap: Arc<PriceTap>) -> Self {
        Self {
            inner: Arc::new(QuoteShared {
                client: Mutex::new(Dial { feed: Some(feed), deferred: Vec::new(), shut: false }),
                returned: Condvar::new(),
                book: Mutex::new(QuoteBook {
                    watched: HashMap::new(),
                    opening: std::collections::HashSet::new(),
                    closed: false,
                    cap: None,
                    failure_ttl: None,
                    failed: None,
                }),
            }),
            tap,
        }
    }

    /// This feed with at most `cap` quote lanes held open for symbols that were not mounted (ig: each lane
    /// is a logged-in IG session and a Lightstreamer socket, and the venue's caps are unmeasured), and a
    /// failed subscribe remembered for `LANE_FAILURE_TTL` (30 s; each attempt is a login, and a failing one is
    /// no lane, so the cap alone never bounded retries). The idle reaper closes only the lane of a symbol no
    /// account admitted (`LeaseFacts::close_idle_pumps`) and frees its place; an ADMITTED symbol's lane
    /// stays open until its account's engine stops or the node restarts.
    #[must_use]
    pub fn with_lane_cap(self, cap: usize) -> Self {
        self.with_lane_limits(cap, LANE_FAILURE_TTL)
    }

    /// [`Self::with_lane_cap`] with the failure memory's length chosen (tests).
    #[must_use]
    pub(super) fn with_lane_limits(self, cap: usize, failure_ttl: Duration) -> Self {
        {
            let mut book = self.inner.book();
            book.cap = Some(cap);
            book.failure_ttl = Some(failure_ttl);
        }
        self
    }

    /// A handle that subscribes more quote lanes on this dial and reads the prices they deliver.
    pub fn watcher(&self) -> CexWatcher {
        let inner = Arc::clone(&self.inner);
        let feed: Arc<dyn PriceLane> = inner;
        CexWatcher { feed, tap: Arc::clone(&self.tap) }
    }

    /// Release every subscription (the mounted ones and the watcher's: [`Tracked`] holds every id it
    /// handed out), then close the dial: the [`Tracked`] order. Never waits for a subscribe round trip in
    /// flight: that one closes the dial itself when it gives it back.
    pub fn shutdown(&mut self) {
        {
            let mut book = self.inner.book();
            book.closed = true;
            book.watched.clear();
        }
        let mut dial = self.inner.client();
        dial.shut = true;
        if let Some(feed) = dial.feed.as_mut() {
            feed.shutdown();
        }
        self.inner.returned.notify_all();
    }
}

impl Drop for QuoteFeed {
    /// A feed dropped without its shutdown is released in order too: a [`CexWatcher`] outliving it keeps
    /// the dial alive, so [`Tracked`]'s own `Drop` would not run.
    fn drop(&mut self) {
        let closed = self.inner.book().closed;
        if !closed {
            self.shutdown();
        }
    }
}

impl PriceLane for QuoteShared {
    /// Hyperliquid's quote lane closes like a CEX tick pump: the reaper's rules (an idle symbol no
    /// account admitted, no venue gap open) are the caller's, and the mount's own quotes are not in
    /// `watched`.
    fn close(&self, symbol: &str) -> bool {
        let id = self.book().watched.remove(symbol);
        if let Some(id) = id {
            self.release(id);
        }
        id.is_some()
    }

    fn subscribe(&self, symbol: &str, _tick_size: f64) -> Result<(), LiveDataError> {
        {
            let mut book = self.book();
            if book.closed {
                return Err(LiveDataError::Subscribe("the price feed is shut down".to_string()));
            }
            if book.watched.contains_key(symbol) || book.opening.contains(symbol) {
                return Ok(());
            }
            if let Some(cap) = book.cap
                && book.watched.len() + book.opening.len() >= cap
            {
                return Err(LiveDataError::Subscribe(format!(
                    "this feed holds at most {cap} extra quote lanes and all are open (the idle \
                     reaper frees only the lane of a symbol no account admitted; the lane of an \
                     admitted symbol stays open until its account's engine stops or the node \
                     restarts)"
                )));
            }
            book.opening.insert(symbol.to_string());
        }
        // The round trip: the dial is lent out, so neither lock is held (a close or a shutdown meanwhile
        // is applied when it comes back). A subscriber that waited for the dial re-reads the failure
        // memory: the one it queued behind may have failed.
        let lent = self.lend().and_then(|dial| match self.remembered_failure() {
            Some(why) => Err(LiveDataError::Subscribe(why)),
            None => Ok(dial),
        });
        let mut dial = match lent {
            Ok(dial) => dial,
            Err(e) => {
                self.book().opening.remove(symbol);
                return Err(e);
            }
        };
        let subscribed = dial.feed().subscribe_quotes(symbol);
        let mut book = self.book();
        book.opening.remove(symbol);
        match &subscribed {
            Ok(id) if book.closed => dial.release = Some(*id),
            Ok(id) => {
                book.failed = None;
                book.watched.insert(symbol.to_string(), *id);
            }
            Err(e) => {
                if book.failure_ttl.is_some() {
                    book.failed = Some((Instant::now(), e.to_string()));
                }
            }
        }
        subscribed.map(|_| ())
    }
}

/// ONE live venue's market feed — one variant per live-wired venue, so the daemon's teardown stays
/// venue-agnostic. Every variant is a dial to the datahub's strategy lane, and each releases its
/// subscriptions before it closes ([`Tracked`], [`CexFeed`]).
///
/// The daemon holds a COLLECTION of these — one entry per DISTINCT mounted venue, in mount order
/// (`live_runtime`'s `feeds` vec; a single-mount profile holds one) — and each venue's feed arm is wired
/// exactly once however many mounts share the venue.
pub enum LiveFeeds {
    /// Hyperliquid: bars + quotes, on the network the venue's armed accounts' exec dials; the dial is shared with
    /// the price handle of a symbol the control door admits ([`QuoteFeed`]).
    Hyperliquid(QuoteFeed),
    /// One feed (its own dial + `MakerSink` bar synth) per DISTINCT mounted token: a `MakerSink` folds
    /// EVERY price it sees into ITS token's synth bars, so two tokens through one sink would corrupt both
    /// mounts' paper-fill bars — the per-token split is correctness, not tidiness. (Two mounts on ONE token
    /// share one entry; a same-token interval mismatch is refused before the core spawns — see
    /// `live_runtime`'s planning pass.)
    Polymarket(Vec<Tracked>),
    /// binance/bybit/okx/aster: the tick pump AND the kline lane, both required (see [`CexFeed`] for why
    /// neither alone is a working mount).
    Cex(CexFeed),
    /// alpaca (split-plane I9): the ONE multiplexed, OAuth-authenticated market-data client — real 1m bar
    /// closes + quotes + trades through `CoreLaneSink`. A credentialed stream at the gatewayhub. Held as a
    /// [`QuoteFeed`] (as Hyperliquid's and deribit's): a quote subscription delivers the price of a symbol
    /// the control door admits (decision 0174).
    Alpaca(QuoteFeed),
    /// ctrader (split-plane I9): quotes over the gatewayhub's dedicated data socket. Closed bars are
    /// SYNTHESIZED from quote mids by the [`vike_tradehub_core::MakerSink`] the feed delivers into — the
    /// socket itself never emits a bar close. Held as a [`QuoteFeed`] (as alpaca's): a quote subscription
    /// delivers the price of a symbol the control door admits (decision 0174), through a sink that keeps
    /// the quotes of any symbol but the mount's out of the mount's bar synthesizer.
    Ctrader(QuoteFeed),
    /// oanda (split-plane I9): quotes off the chunked-HTTP `/pricing/stream` line stream, bars POLLED off
    /// the candles REST endpoint, both Bearer-authed against one account. Held behind the [`DataClient`]
    /// seam since the arm was routed through [`FeedCtors`]: production stores the strategy-lane dial, the
    /// deterministic splice test stores its scripted double.
    Oanda(Tracked),
    /// deribit (split-plane I9): the widest feed this daemon mounts — FOUR live verbs (bars, quotes,
    /// trades, book) off the venue's KEYLESS public MAINNET host. The book verb here is the LOSSLESS
    /// `LiveDataSink::book` lane (which `vike_tradehub_core::CoreLaneSink` really forwards) rather than the
    /// conflating `l2_snapshot` one the CEX arm has to route around. Held behind the [`DataClient`] seam
    /// like oanda's, and shared with the price handle of a perpetual the control door admits
    /// ([`QuoteFeed`], as Hyperliquid's: a quote subscription delivers the price).
    Deribit(QuoteFeed),
    /// ibkr (any-symbol price lane, decision 0174): NEVER a mount (no profile names ibkr, it has no
    /// `FEED_ARMS` row) - only the PRICE-ONLY feed of the stocks the control door admits: the dial to the
    /// datahub's strategy lane with nothing subscribed, whose quote lane the door opens per symbol through the
    /// feed's watcher, over a [`PriceTap::real_time_only`] (only a real-time two-sided quote is a price).
    /// Quotes only; the gatewayhub's end is a credentialed stream that dials the IB Gateway on that first symbol.
    Ibkr(QuoteFeed),
    /// ig (split-plane I9): the NARROWEST live feed this daemon mounts — TWO verbs (`subscribe_quotes` +
    /// `subscribe_bars`) and no more: trades and book are caps refusals, because a DEALER venue publishes
    /// its own two-sided price and no public tape or ladder exists behind them. Held as a [`QuoteFeed`] with
    /// a lane cap (`feeds::ig::IG_MAX_QUOTE_LANES`): a quote subscription delivers the price of an epic the
    /// control door admits (decision 0174), and each lane is a logged-in IG session plus a socket.
    Ig(QuoteFeed),
}

impl LiveFeeds {
    /// The handle the control door subscribes a newly admitted symbol's price through: the CEX
    /// feed's, Hyperliquid's, deribit's, alpaca's, ctrader's, ig's or ibkr's (`None` for every other venue, whose
    /// adapters the door does not admit for).
    pub fn price_watcher(&self) -> Option<CexWatcher> {
        match self {
            LiveFeeds::Cex(feed) => Some(feed.watcher()),
            LiveFeeds::Hyperliquid(feed)
            | LiveFeeds::Deribit(feed)
            | LiveFeeds::Alpaca(feed)
            | LiveFeeds::Ctrader(feed)
            | LiveFeeds::Ig(feed)
            | LiveFeeds::Ibkr(feed) => Some(feed.watcher()),
            _ => None,
        }
    }

    /// The per-venue feed-status handles for the reconcile HEALTH GATE — EMPTY for every venue, since
    /// wave 2.
    ///
    /// The CEX row was the one row this map ever carried: it keyed on the bridge's `Feeds::status` string
    /// (`vike_bybit::market_feed`'s `LIVE_STATUS` and its siblings), a handle this process no longer
    /// has — the venue sockets are the gatewayhub's. The health gate's evidence for a CEX venue is the
    /// gatewayhub's own (`ReconNotReady{Degraded}`, the recon task), and an absent row reads `Healthy`
    /// here, the fail-soft direction the gate always argued for: it can only ever SUPPRESS a pass.
    ///
    /// ⚠ Until that lands, a CEX venue's reconcile pass is no longer suppressed by a degraded feed
    /// string. The function stays, returning the map's type, only so `live_runtime`'s one caller and
    /// `reconcile_config::build_recon_config`'s parameter keep compiling; they leave with the recon task.
    pub fn recon_feed_statuses(&self) -> HashMap<String, Arc<std::sync::Mutex<String>>> {
        HashMap::new()
    }

    pub fn shutdown(&mut self) {
        match self {
            LiveFeeds::Oanda(f) => f.shutdown(),
            LiveFeeds::Hyperliquid(f)
            | LiveFeeds::Deribit(f)
            | LiveFeeds::Alpaca(f)
            | LiveFeeds::Ctrader(f)
            | LiveFeeds::Ig(f)
            | LiveFeeds::Ibkr(f) => {
                f.shutdown();
            }
            LiveFeeds::Polymarket(list) => {
                for f in list {
                    f.shutdown();
                }
            }
            LiveFeeds::Cex(f) => f.shutdown(),
        }
    }
}

/// The MERGED per-venue feed-status map across every mounted venue's feed (split-plane I10) — what the
/// reconcile health gate takes now that `live_runtime` wires N venues. EMPTY since wave 2: see
/// [`LiveFeeds::recon_feed_statuses`].
pub fn recon_feed_statuses_of(
    feeds: &[LiveFeeds],
) -> HashMap<String, Arc<std::sync::Mutex<String>>> {
    let mut merged = HashMap::new();
    for f in feeds {
        merged.extend(f.recon_feed_statuses());
    }
    merged
}

/// The LIVE arm's extra teardown handles (there are none in the paper arm). Held from mount to the
/// bounded shutdown so the venue feed threads are stopped+joined and the live-event forwarder is
/// quiesced in the load-bearing order (forwarder-stop BEFORE the core shutdown+join — see
/// `crates/vike-tradehub-core/src/node.rs`'s forwarder teardown-safety note).
/// Extra teardown run AFTER `feeds.shutdown()` and BEFORE the core join — the recorder's bounded flush
/// when `record-feeds` + the `flags.tradehub_record` row, else `None` (byte-identical to no recording).
/// ⚠ Always `None` since #2093 deleted `record-feeds` (2026-09-22): the slot stays so the teardown
/// tail keeps its shape (`crates/vike-tradehub/src/tradehub_cli.rs`'s `post_feeds`). Boxed
/// (and aliased) so the struct never names the feature-gated `RecorderHandle` type, and to keep the
/// recorder-open tuple under clippy's `type_complexity` bar.
pub type PostFeeds = Option<Box<dyn FnOnce() + Send>>;

pub struct LiveTeardown {
    /// One entry per mounted venue's feed (split-plane I10) — a single-mount daemon holds one.
    /// The teardown fans these out as one bounded task EACH, so every venue's feed quiesces
    /// inside the ONE shutdown deadline and a wedged venue cannot serialize its siblings.
    pub feeds: Vec<LiveFeeds>,
    pub forwarder_stop: Arc<AtomicBool>,
    pub post_feeds: PostFeeds,
    /// The reconciliation driver, `Some` when the S2 reconcile gate said yes AND ≥1 live venue
    /// produced a ReconClient (`None` on a paper mount and on any run the operator refused). Shut
    /// down in the SEQUENTIAL tail BEFORE the core join (the teardown order vike-app's local core
    /// used): it holds only a WEAK core-ingest sender so it can never wedge the core, but joining it
    /// there bounds its `vt-core-recon` thread to the daemon's lifetime rather than leaking it.
    pub recon_driver: Option<vike_tradehub_core::ReconDriver>,
    /// Where the control door looks up the facts of a symbol an engine was not started on: the open
    /// trade leases of this run (`crate::remote_backend::facts::LeaseFacts`). `None` on a mount with
    /// no gatewayhub lease to ask. Not torn down: it only reads sessions the engines own.
    pub symbol_facts: Option<Arc<dyn crate::server::symbol_admission::SymbolFacts>>,
    /// The account watcher's parts (decision 0169), taken out by `run` once the observe server exists and
    /// handed to [`crate::account_follow::spawn`]; `None` when nothing follows the account rows.
    pub follow: Option<crate::account_follow::FollowParts>,
}
