//! `DataClient` — the live market-data seam, the live mirror of [`crate::hist::HistStore`].
//!
//! This carries over the proven contract from the retired vike-exec market-data prototype
//! (production-dead — the GUI never used it): **fire-and-forget**. `subscribe_*` starts a stream
//! and returns immediately; it never carries data itself. ALL data returns via the
//! [`LiveDataSink`] handed to the client at CONSTRUCTION — the trait methods below never carry a
//! sink parameter, and there is no "subscribe with a callback" shape. An `Err` from a `subscribe_*`
//! call means the subscription could not be *started* (e.g. the OS refused a feed thread, or a bad
//! symbol was rejected up front); ongoing per-stream runtime faults (disconnects, reconnects,
//! resubscribe-after-drop) surface on the feed's own status/reconnect path, never through this
//! trait. Reconnect is the feed's concern, not the seam's.
//!
//! Capabilities-not-obligations (spec D7): a client that cannot serve a verb at all (e.g. a
//! bars-only feed asked for live trade prints) returns [`LiveDataError::Unsupported`] rather than
//! pretending to subscribe. Quote/trade subscribe verbs exist on [`DataClient`] today per spec,
//! but every current implementation returns `Unsupported` for them — there is no live tick
//! producer yet (the R8 core lanes exist for this; [`LiveDataSink`] grows `quote`/`trade` methods
//! the day a venue first implements one of these verbs).
//!
//! Sink methods must be cheap and effectively non-blocking: implementations are called from feed
//! worker threads on every closed bar / intrabar update / tick, so a slow sink stalls the feed.
//! Downstream is a lossless bar lane (`seed_bars`/`close_bar` — a missed close is a series hole)
//! plus a latest-wins conflating lane (`forming_bar`/`bar_close_tick`/`mark_tick`), the same
//! shape the Binance kline feed already assumes. `bar_close_tick` carries a kline feed's
//! candle-close snapshots; `mark_tick` is reserved for a venue's REAL mark/index price stream
//! (mark-slot semantics — the two land in different `PriceBoard` slots downstream).
//!
//! # Which price verb a feed must use (the producer side of the valuation law)
//!
//! Downstream, each price verb fills a DIFFERENT rung of `vike_exec::price_board`'s resolver
//! chain, and that chain is strictly ordered — `mark` > side-aware `bid`/`ask` > `last_trade` >
//! `bar_close`. A feed choosing the wrong verb therefore does not merely mislabel a number, it
//! re-ranks it against every other price for the same symbol. The mapping is fixed:
//!
//! - [`LiveDataSink::mark_tick`] — ONLY a venue-published mark/index stream (the price that venue's
//!   own liquidation and funding engines key off). Today: binance-family `@markPrice@1s`,
//!   bybit `tickers.markPrice`, okx `mark-price`, hyperliquid `activeAssetCtx.markPx` — default ON
//!   for perps; aster `@markPrice@1s` is default OFF (unverified grammar — opt in with
//!   `VIKE_MARK_STREAMS_ASTER=1`). All disabled together by the master kill `VIKE_MARK_STREAMS=0`.
//! - [`LiveDataSink::quote`] — a BBO/quote or two-sided book top. Fills `bid`/`ask`.
//! - [`LiveDataSink::trade`] — a real print. Fills `last_trade`.
//! - [`LiveDataSink::bar_close_tick`] — a kline feed's candle close, from EVERY venue that serves
//!   klines (binance/aster/bybit/okx/hyperliquid/alpaca/ibkr). It is the LOWEST rung: an aggregate
//!   of a window that has already ended, up to one full interval stale.
//!
//! Note the classes overlap freely. A venue with no mark stream still fills the quote and trade
//! rungs whenever those subscriptions are open — alpaca's `dispatch` emits `quote`/`trade`
//! alongside `bar_close_tick`, and vike-app's `ensure_trade_feed_on` subscribes trades for ANY
//! venue as soon as an orderflow or tick chart is opened. Such a symbol is valued at its live
//! quote or last trade, not at its candle close; that is the intended law, not an accident.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use vike_model::{Bar, BookUpdate, L2Book, Level, QuoteTick, TradeTick};

/// Opaque handle to one `subscribe_*` call. Returned by [`DataClient::subscribe_bars`] /
/// `subscribe_quotes` / `subscribe_trades`; pass it back to [`DataClient::unsubscribe`] to stop
/// just that one stream (other subscriptions on the same client keep running).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubscriptionId(pub u64);

/// Why a subscription could not be *started*. Deliberately small, mirroring the retired
/// vike-exec market-data prototype's start-failure error: ongoing per-stream faults (reconnect,
/// seed errors) ride the feed's own status/reconnect path, not here. `#[non_exhaustive]` —
/// clients may need new start-failure shapes without a breaking change.
#[derive(Debug)]
#[non_exhaustive]
pub enum LiveDataError {
    /// A feed subscription could not be started (e.g. the OS refused a feed thread, or a bad
    /// symbol was rejected up front).
    Subscribe(String),
    /// The client does not serve this verb at all (capabilities-not-obligations, spec D7) — e.g.
    /// a bars-only feed asked for `subscribe_trades`. The `&'static str` names the verb/reason.
    Unsupported(&'static str),
}

impl std::fmt::Display for LiveDataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LiveDataError::Subscribe(msg) => write!(f, "live-data subscribe failed: {msg}"),
            LiveDataError::Unsupported(what) => write!(f, "live-data verb unsupported: {what}"),
        }
    }
}

impl std::error::Error for LiveDataError {}

/// The declared-capability refusal, DRIVEN off the venue's `vike_model::VenueCaps.live_data` row
/// (w2-task-5) instead of each feed hand-writing its own `Unsupported` decision: `Ok(())` when
/// the venue's row declares `verb` served, the canonical [`LiveDataError::Unsupported`] otherwise.
/// A feed's `subscribe_*` verb calls this FIRST (`require_live_verb(VENUE, LiveVerb::Quotes)?;`)
/// so its refusal set can never drift from the declared matrix — the same
/// one-declaration-many-consumers discipline the order-path preflight applies to `VenueCaps`.
/// (Bridge feeds still hand-roll their refusals today; routing them through here is the noted
/// follow-up — their files are owned by sibling tasks.)
///
/// An UNKNOWN venue string refuses every verb (the registry's fail-closed `UNSUPPORTED` row) —
/// correct here because a feed that exists but is not in the roster has no declared row to serve
/// from; a non-venue test double should not call this.
pub fn require_live_verb(venue: &str, verb: vike_model::LiveVerb) -> Result<(), LiveDataError> {
    use vike_model::LiveVerb;
    if vike_model::caps_for(venue).live_data.supports(verb) {
        return Ok(());
    }
    Err(LiveDataError::Unsupported(match verb {
        LiveVerb::Bars => "venue's declared VenueCaps.live_data serves no live bars",
        LiveVerb::Quotes => "venue's declared VenueCaps.live_data serves no live quotes",
        LiveVerb::Trades => "venue's declared VenueCaps.live_data serves no live trade prints",
        LiveVerb::Book => "venue's declared VenueCaps.live_data serves no lossless L2 book lane",
        LiveVerb::Depth => "venue's declared VenueCaps.live_data serves no L2 depth-snapshot lane",
    }))
}

/// Stream-health disclosure for one live stream (net-hardening §B). `stream` names the lane the way
/// the data methods do: an interval (`"1m"`) for bar streams, `"quotes"`/`"trades"`/`"book"` for
/// tick lanes. Reconnect *handling* (backoff, re-open, re-seed) stays entirely the feed's concern;
/// what the seam gains is gap *disclosure*, because for tick lanes the data lost in a gap is
/// unrecoverable and only the consumer can decide what staleness means (flatten, re-arm, widen).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamStatus {
    /// The stream can no longer be trusted from `at_ts_ms`: transport error, server close, or
    /// idle-watchdog trip. Data between this and the next `Live` is MISSING (tick lanes) or STALE
    /// (bar forming lane); the lossless bar lane is repaired by the re-seed before `Live` fires.
    GapStart { at_ts_ms: i64 },
    /// The stream is live again. For bar streams this fires AFTER the re-seed, so the lossless lane
    /// is already repaired when consumers see it; for tick lanes it marks where the unrecoverable
    /// hole ends. `gap_started_ts_ms` echoes the matching `GapStart` when one was emitted.
    ///
    /// `Live` ALSO closes a stream-health freshness staleness episode (a preceding
    /// `Stale`) — in that case NO re-seed occurred at all: the socket never dropped, so there was
    /// nothing to repair; fresh data simply resumed flowing on the same still-live connection.
    /// `gap_started_ts_ms` then echoes the `Stale` episode's trip time rather than a real gap start.
    Live { gap_started_ts_ms: Option<i64> },
    /// The transport is still alive (recent frames — data OR keepalives) but no fresh DATA has
    /// arrived: `now_ms − newest_data_ts_ms` exceeded the subscription's freshness threshold. This
    /// is the silently-failed re-subscribe a socket-liveness watchdog can't see. Distinct from
    /// `GapStart` (transport loss) precisely because both can be true at once — a consumer may need
    /// "transport OK but data stale". Recovery is the shared `Live`.
    Stale { newest_data_ts_ms: i64, now_ms: i64 },
}

/// Where a [`DataClient`]'s streamed data lands. Given to the client at CONSTRUCTION (never
/// threaded through `subscribe_*`/`unsubscribe`/`shutdown` — those stay fire-and-forget). A
/// single sink instance is typically shared (e.g. via `Arc<dyn LiveDataSink>`) across every
/// subscription a client owns. `Send + Sync` — feeds call it from their own worker threads.
pub trait LiveDataSink: Send + Sync {
    /// Seed a freshly (re)subscribed series with its recent history, ts-ascending, before live
    /// closes start arriving.
    fn seed_bars(&self, venue: &str, symbol: &str, interval: &str, bars: Vec<Bar>);
    /// One bar has CLOSED — the lossless lane (a missed close is a series hole downstream).
    fn close_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar);
    /// An intrabar snapshot of the bar still forming — the conflating lane (latest-wins; drops
    /// are fine, only the newest snapshot matters).
    fn forming_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar);
    /// The venue's REAL mark/index valuation price for `symbol` — same conflating lane as
    /// `forming_bar`. This verb is reserved for a venue-published mark stream (binance-family
    /// `@markPrice@1s`, bybit `tickers` markPrice, okx `mark-price`, HL `activeAssetCtx` markPx):
    /// downstream it fills the `PriceBoard` MARK slot, the head of the valuation resolver chain,
    /// so it must never carry a candle close (mark-slot semantics, law-map A1). Kline-driven
    /// feeds emit their last-price snapshots through [`LiveDataSink::bar_close_tick`] instead.
    fn mark_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64);
    /// The latest CANDLE-CLOSE last-price snapshot from a kline-driven feed (closed OR forming
    /// bar) — the conflating lane, same cheap/latest-wins contract as `mark_tick`. Split from
    /// `mark_tick` so the core can file it in the `PriceBoard` BAR-CLOSE slot (the resolver's
    /// fallback) rather than the MARK slot (the venue's real valuation price). DEFAULT no-op —
    /// existing sinks compile and behave unchanged until they opt in (the same additive-growth
    /// rule as `l2_snapshot`/`book_update`/`stream_status`).
    fn bar_close_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        let _ = (venue, symbol, px, ts);
    }
    /// A full L2 depth snapshot for `symbol` — `bids`/`asks` as `(price, qty)` levels (a top-N
    /// partial book, or a folded full book). The conflating lane: latest-wins, drops are fine.
    /// `tick_size` is the instrument's authoritative price tick, carried so a book-consuming sink
    /// quantizes on the SAME grid the feed used (rather than re-inferring it from a truncated level
    /// set — which can diverge or jitter frame-to-frame); pass `0.0` if unknown. `ts` is the
    /// venue/receipt epoch-ms stamp. Default no-op — only sinks that consume a book (e.g. a DOM
    /// window's book store) override it, and only depth-capable feeds call it (spec D7).
    fn l2_snapshot(
        &self,
        venue: &str,
        symbol: &str,
        tick_size: f64,
        bids: Vec<Level>,
        asks: Vec<Level>,
        ts: i64,
    ) {
        let _ = (venue, symbol, tick_size, bids, asks, ts);
    }
    /// One L1 quote update (R8 tick lane) — same fire-and-forget/cheap contract as the bar
    /// verbs above; every distinct quote is a separate call (lossless downstream, unlike
    /// `forming_bar`/`mark_tick`'s conflation).
    fn quote(&self, venue: &str, symbol: &str, quote: QuoteTick);
    /// One executed-trade print (R8 tick lane) — same fire-and-forget/cheap contract.
    fn trade(&self, venue: &str, symbol: &str, trade: TradeTick);
    /// One L2 book update (R8 tick lane) — same fire-and-forget/cheap contract. This is the
    /// FOLDED-STATE verb (whole book, no delta information): a sink that persists ticks must
    /// derive rows from it (e.g. top-of-book) or ignore it entirely and take the delta-preserving
    /// [`LiveDataSink::book_update`] instead.
    ///
    /// The book arrives behind an [`Arc`] (perf audit finding #1): a pump keeping ONE standing
    /// book hands out `Arc::clone`s and mutates its own copy through `Arc::make_mut`, so an
    /// applied depth delta no longer deep-clones two whole `BTreeMap`s per emission — and the
    /// consumer that ends up dropping the last handle is whichever thread finishes last, not
    /// unconditionally the single-writer core. A sink needing an OWNED, mutable book clones
    /// explicitly (`(*book).clone()`); a sink that only reads gets it for free through `Deref`.
    /// Fan-out (see [`TeeSink`]) is a refcount bump per inner sink instead of a full copy.
    fn book(&self, venue: &str, symbol: &str, book: Arc<L2Book>);
    /// One RAW L2 book update (delta / snapshot-anchor / §B status marker) — the RECORDABLE
    /// twin of [`LiveDataSink::book`] (which carries folded state and cannot reconstruct
    /// deltas). Lossless lane, same fire-and-forget/cheap contract as `quote`/`trade`.
    /// DEFAULT no-op: only recording sinks opt in, and only feeds that maintain a
    /// seq-tracked book emit it (today: the polymarket pump in Book mode).
    fn book_update(&self, venue: &str, symbol: &str, update: BookUpdate) {
        let _ = (venue, symbol, update);
    }
    /// Stream-health disclosure (net-hardening §B). DEFAULT no-op: existing sinks compile and
    /// behave unchanged until they opt in. The feed's idle watchdog + reconnect path fire this —
    /// `GapStart` when a stream goes silent/errors, `Live` once it's re-seeded/recovered. `stream`
    /// is the interval for bar lanes, `"quotes"`/`"trades"`/`"book"` for tick lanes.
    fn stream_status(&self, venue: &str, symbol: &str, stream: &str, status: StreamStatus) {
        let _ = (venue, symbol, stream, status);
    }
}

/// The live market-data seam — the live mirror of [`crate::hist::HistStore`]. A venue adapter
/// implements this directly on its feed type to start/stop live streams; all data flows out
/// through the [`LiveDataSink`] given to the client at construction, never through this trait's
/// return values. Fire-and-forget: `subscribe_*` starts a stream and returns immediately; see the
/// module doc for the `Err` / runtime-fault split and the capabilities-not-obligations rule.
pub trait DataClient {
    /// Start streaming closed + forming bars for `(symbol, interval)`. Returns a
    /// [`SubscriptionId`] to later pass to [`DataClient::unsubscribe`].
    fn subscribe_bars(
        &mut self,
        symbol: &str,
        interval: &str,
    ) -> Result<SubscriptionId, LiveDataError>;
    /// Start streaming L1 quotes for `symbol`. `Err(Unsupported)` if the client serves no quote
    /// feed (capabilities-not-obligations, spec D7).
    fn subscribe_quotes(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError>;
    /// Start streaming executed trades for `symbol`. `Err(Unsupported)` if the client serves no
    /// trade feed.
    fn subscribe_trades(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError>;
    /// Start streaming L2 book updates for `symbol`. `Err(Unsupported)` if the client serves no
    /// book feed (capabilities-not-obligations, spec D7). A book subscription MAY also imply
    /// derived `LiveDataSink::quote` emissions (top-of-book changes) — a concrete client
    /// documents its exact behavior (see the polymarket feed's pump, tick-producer Task 3).
    fn subscribe_book(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError>;
    /// Start streaming L2 depth SNAPSHOTS for `symbol` (delivered via [`LiveDataSink::l2_snapshot`];
    /// the DOM's conflating book lane, distinct from the lossless tick-lane `subscribe_book`/`book`).
    /// Default `Unsupported` — only venues with a live depth feed override it; every other client
    /// inherits this and stays a no-op (capabilities-not-obligations).
    fn subscribe_depth(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        let _ = symbol;
        Err(LiveDataError::Unsupported("no live depth feed"))
    }
    /// Stop exactly the stream `id` names. Unknown ids (already stopped, or never issued by this
    /// client) are a no-op — never panics.
    fn unsubscribe(&mut self, id: SubscriptionId);
    /// Raise this client's stop flags and RETURN AT ONCE — join nothing, block on nothing. Phase one
    /// of a teardown across SEVERAL clients: run it on every client first, and each one's threads are
    /// already winding down by the time the first [`DataClient::shutdown`]/[`DataClient::unsubscribe`]
    /// join is entered.
    ///
    /// Why it exists: a feed thread learns it should stop on its next socket read timeout — 2 s on
    /// every on-driver venue (`crates/vike-bridge-core/src/pump_spec.rs`'s `market_pump_spec`) — so a
    /// caller that stops one stream at a time pays that timeout PER SOCKET. `vike-recorder`'s
    /// teardown did exactly that, outside its own bounded region, which put its total stop time on a
    /// collision course with the unit's `TimeoutStopSec=` as a profile grew (a binance subscription
    /// is two threads per symbol). With this, the whole profile costs about ONE timeout.
    ///
    /// DEFAULT no-op — the additive-growth rule this trait already uses for `l2_snapshot` /
    /// `book_update` / `stream_status`. A client that does not override it is still torn down
    /// correctly by `shutdown`; it simply does not get the head start, so its cost stays serial. The
    /// registry-backed feeds implement it as [`FeedRegistry::raise_stops`].
    fn begin_shutdown(&mut self) {}
    /// Stop every stream this client owns and release any resources (join feed threads, etc.) —
    /// deterministic teardown, mirroring the retired vike-exec market-data prototype's handle
    /// shutdown.
    fn shutdown(&mut self);
}

/// The per-subscription feed-thread registry every venue `Feeds` shares (dedup A6, wave 3):
/// [`SubscriptionId`] allocation, one stop flag + one `JoinHandle` per subscription, stop+join of
/// exactly one stream ([`FeedRegistry::stop_join`] — the `unsubscribe` body) or of them all
/// ([`FeedRegistry::shutdown`]). Nothing is ever detached — the deterministic-teardown rule.
///
/// This was pasted per venue (binance/bybit/okx/hyperliquid/polymarket, `next_id` + `subs` +
/// identical unsubscribe/shutdown drains); the venue keeps only its own context (sink/status/wake)
/// and hands this registry a thread body taking the fresh stop flag. The optional `spawn_hook`
/// runs ON each spawned thread before the body — the seam a venue threads
/// `vike_exec::affinity::pin_current_thread` through WITHOUT this crate depending on `vike-exec`
/// (layering: vike-data stays model-only).
///
/// Converted: bybit, binance, okx, hyperliquid, polymarket (the whole text-WS market-feed set).
pub struct FeedRegistry {
    next_id: u64,
    subs: HashMap<SubscriptionId, (Arc<AtomicBool>, std::thread::JoinHandle<()>)>,
    spawn_hook: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl FeedRegistry {
    /// An empty registry with no spawn hook.
    pub fn new() -> Self {
        FeedRegistry { next_id: 0, subs: HashMap::new(), spawn_hook: None }
    }

    /// An empty registry whose `hook` runs ON every spawned feed thread, before its body — e.g.
    /// the venue's affinity pin (`vike_exec::affinity::pin_current_thread(MarketData, venue)`).
    pub fn with_spawn_hook(hook: impl Fn() + Send + Sync + 'static) -> Self {
        FeedRegistry { next_id: 0, subs: HashMap::new(), spawn_hook: Some(Arc::new(hook)) }
    }

    /// Spawn `body` on its own named thread with a fresh [`SubscriptionId`] + dedicated stop flag
    /// (handed to `body` — the venue builds its own feed context around it). An OS thread-spawn
    /// failure is returned (not panicked) so `DataClient::subscribe_*` can map it to a
    /// [`LiveDataError`].
    pub fn spawn(
        &mut self,
        thread_name: String,
        body: impl FnOnce(Arc<AtomicBool>) + Send + 'static,
    ) -> std::io::Result<SubscriptionId> {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let hook = self.spawn_hook.clone();
        let h = std::thread::Builder::new().name(thread_name).spawn(move || {
            if let Some(hook) = hook {
                hook();
            }
            body(stop_thread)
        })?;
        let id = SubscriptionId(self.next_id);
        self.next_id += 1;
        self.subs.insert(id, (stop, h));
        Ok(id)
    }

    /// Stop + JOIN exactly the stream `id` names; every other subscription keeps running. Unknown
    /// ids (already stopped, or never issued) are a no-op — never panics.
    pub fn stop_join(&mut self, id: SubscriptionId) {
        if let Some((stop, h)) = self.subs.remove(&id) {
            stop.store(true, Ordering::Relaxed);
            let _ = h.join();
        }
    }

    /// Raise every stop flag and JOIN NOTHING — phase one of the raise-all-then-join idiom, exposed
    /// so a caller holding SEVERAL clients can run that phase across all of them before any join.
    ///
    /// This is the difference between a teardown that costs one socket read-timeout and one that
    /// costs the SUM of them. A feed thread notices its flag on its next read timeout (2 s on every
    /// on-driver venue — `crates/vike-bridge-core/src/pump_spec.rs`'s `market_pump_spec`), so
    /// raising N flags and then joining N threads costs about one timeout, while
    /// raise-join-raise-join costs N of them. [`Self::shutdown`] has always had that property WITHIN
    /// one registry; the recorder's teardown holds one registry PER SUBSCRIBED VENUE and reaches it
    /// across them (`crates/vike-recorder/src/runtime.rs`'s `stop_all`), which is what this is for.
    ///
    /// Idempotent, and safe on a registry that is about to be shut down anyway: raising an
    /// already-raised flag is a store of `true` over `true`.
    pub fn raise_stops(&mut self) {
        for (stop, _) in self.subs.values() {
            stop.store(true, Ordering::Relaxed);
        }
    }

    /// Deterministic teardown: raise EVERY stop flag first (so all threads wind down in parallel),
    /// then JOIN every feed thread.
    pub fn shutdown(&mut self) {
        self.raise_stops();
        for (_, (_, h)) in self.subs.drain() {
            let _ = h.join();
        }
    }

    /// Number of live (registered) subscriptions.
    pub fn len(&self) -> usize {
        self.subs.len()
    }

    /// Whether no subscription is registered.
    pub fn is_empty(&self) -> bool {
        self.subs.is_empty()
    }

    /// Whether `id` is still registered (not yet unsubscribed/shut down).
    pub fn contains(&self, id: SubscriptionId) -> bool {
        self.subs.contains_key(&id)
    }
}

impl Default for FeedRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Fan a single feed's sink calls out to every inner sink, in order — lets a venue feed's ONE
/// sink slot (given at `DataClient` construction) drive multiple destinations at once, e.g. the
/// core ingest adapter plus a tick-recording sink (tick-producer composition, spec D7). Each
/// verb forwards to every entry of the `Vec` in order before returning, so downstream ordering
/// across sinks matches the caller's.
pub struct TeeSink(pub Vec<Arc<dyn LiveDataSink>>);

impl LiveDataSink for TeeSink {
    fn seed_bars(&self, venue: &str, symbol: &str, interval: &str, bars: Vec<Bar>) {
        for sink in &self.0 {
            sink.seed_bars(venue, symbol, interval, bars.clone());
        }
    }
    fn close_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        for sink in &self.0 {
            sink.close_bar(venue, symbol, interval, bar.clone());
        }
    }
    fn forming_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        for sink in &self.0 {
            sink.forming_bar(venue, symbol, interval, bar.clone());
        }
    }
    fn mark_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        for sink in &self.0 {
            sink.mark_tick(venue, symbol, px, ts);
        }
    }
    fn bar_close_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        // Must forward, NOT inherit the trait default (a no-op) — else composing a feed's sink
        // into a TeeSink silently drops every candle-close tick and the core's bar-close price
        // slot goes stale (same failure mode as the l2_snapshot comment below).
        for sink in &self.0 {
            sink.bar_close_tick(venue, symbol, px, ts);
        }
    }
    fn l2_snapshot(
        &self,
        venue: &str,
        symbol: &str,
        tick_size: f64,
        bids: Vec<Level>,
        asks: Vec<Level>,
        ts: i64,
    ) {
        // Must forward, NOT inherit the trait default (a no-op) — else composing a feed's sink into
        // a TeeSink (the recorder path) silently drops every depth snapshot and the DOM book store
        // never updates (the book reads permanently STALE). All but the last entry get a clone.
        for sink in &self.0 {
            sink.l2_snapshot(venue, symbol, tick_size, bids.clone(), asks.clone(), ts);
        }
    }
    fn book_update(&self, venue: &str, symbol: &str, update: BookUpdate) {
        // Must forward, NOT inherit the trait default (a no-op) — else composing the recorder
        // into a TeeSink silently drops every recordable book event (same failure mode as the
        // l2_snapshot comment above).
        for sink in &self.0 {
            sink.book_update(venue, symbol, update.clone());
        }
    }
    fn quote(&self, venue: &str, symbol: &str, quote: QuoteTick) {
        for sink in &self.0 {
            sink.quote(venue, symbol, quote.clone());
        }
    }
    fn trade(&self, venue: &str, symbol: &str, trade: TradeTick) {
        for sink in &self.0 {
            sink.trade(venue, symbol, trade.clone());
        }
    }
    fn book(&self, venue: &str, symbol: &str, book: Arc<L2Book>) {
        // Refcount bump per inner sink — the pre-`Arc` shape deep-cloned the whole book here,
        // once per extra destination (perf audit finding #1).
        for sink in &self.0 {
            sink.book(venue, symbol, Arc::clone(&book));
        }
    }
    fn stream_status(&self, venue: &str, symbol: &str, stream: &str, status: StreamStatus) {
        for sink in &self.0 {
            sink.stream_status(venue, symbol, stream, status);
        }
    }
}

#[cfg(test)]
mod feed_registry_tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::Duration;

    /// A network-free feed body: polls its stop flag on the real feeds' cadence.
    fn polling_body(stop: Arc<AtomicBool>) {
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn spawn_returns_distinct_ids() {
        let mut reg = FeedRegistry::new();
        let id1 = reg.spawn("feed-a".into(), polling_body).expect("spawn ok");
        let id2 = reg.spawn("feed-b".into(), polling_body).expect("spawn ok");
        assert_ne!(id1, id2, "distinct ids per spawn");
        assert_eq!(reg.len(), 2);
        reg.shutdown();
    }

    /// `raise_stops` must RAISE and JOIN NOTHING — it is phase one of a cross-client teardown, and a
    /// version that joined would reintroduce exactly the per-socket serialization it exists to
    /// remove. Proven by shape rather than by timing: the registry still holds every subscription
    /// afterwards (nothing was drained), while each thread has observed its flag.
    #[test]
    fn raise_stops_raises_every_flag_without_joining_anything() {
        let mut reg = FeedRegistry::new();
        let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for i in 0..4 {
            let seen = Arc::clone(&seen);
            reg.spawn(format!("feed-{i}"), move |stop: Arc<AtomicBool>| {
                while !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                seen.fetch_add(1, Ordering::Relaxed);
            })
            .expect("spawn ok");
        }

        reg.raise_stops();
        assert_eq!(reg.len(), 4, "raise_stops must not drain the registry — it joins nothing");

        // Every thread now winds down CONCURRENTLY; the join below is what collects them.
        reg.shutdown();
        assert!(reg.is_empty());
        assert_eq!(seen.load(Ordering::Relaxed), 4, "every thread must have seen its raised flag");
    }

    #[test]
    fn stop_join_stops_only_that_one_feed() {
        let mut reg = FeedRegistry::new();
        let id1 = reg.spawn("feed-a".into(), polling_body).expect("spawn ok");
        let id2 = reg.spawn("feed-b".into(), polling_body).expect("spawn ok");
        reg.stop_join(id1);
        assert_eq!(reg.len(), 1, "only the unsubscribed stream is removed");
        assert!(reg.contains(id2), "the other subscription keeps running");
        assert!(!reg.contains(id1));
        // unknown/already-stopped id is a no-op
        reg.stop_join(id1);
        reg.shutdown();
        assert!(reg.is_empty());
    }

    #[test]
    fn shutdown_joins_every_feed() {
        let mut reg = FeedRegistry::new();
        reg.spawn("feed-a".into(), polling_body).expect("spawn ok");
        reg.spawn("feed-b".into(), polling_body).expect("spawn ok");
        reg.shutdown();
        assert!(reg.is_empty());
    }

    /// The spawn hook runs ON the spawned thread, BEFORE the body — the affinity-pin seam. Proven
    /// by ordering (hook entry precedes body entry in the shared log) and by thread identity (the
    /// hook records the spawned thread's name, not the caller's).
    #[test]
    fn the_spawn_hook_runs_on_the_feed_thread_before_the_body() {
        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let hook_log = Arc::clone(&log);
        let mut reg = FeedRegistry::with_spawn_hook(move || {
            let name = std::thread::current().name().unwrap_or("?").to_string();
            hook_log.lock().unwrap().push(format!("hook on {name}"));
        });
        let body_log = Arc::clone(&log);
        reg.spawn("feed-pinned".into(), move |stop| {
            body_log.lock().unwrap().push("body".into());
            polling_body(stop);
        })
        .expect("spawn ok");
        reg.shutdown();
        let log = log.lock().unwrap();
        assert_eq!(
            log.as_slice(),
            ["hook on feed-pinned", "body"],
            "hook first, on the spawned (named) thread, then the body"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use vike_model::BookUpdateKind;

    // The shared capturing sink (testing-arch Phase 4d) — the canonical copy of the formatted-
    // string RecordingSink that used to live inline here; `calls()` renders the same one-line
    // forms (tick verbs now carry the full union field set — sizes, maker flag, book_update's
    // local_ts/tick fields).
    use crate::test_support::RecordingSink;

    fn fake_bar(close: f64) -> Bar {
        Bar {
            ts: 1,
            open: close,
            high: close,
            low: close,
            close,
            volume: 1.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }
    }

    /// A fake venue client: `subscribe_bars` pushes one `seed_bars` + one `close_bar` through the
    /// sink given at construction and returns a fresh id; quotes/trades are unsupported;
    /// unsubscribe/shutdown just record that they were called. Proves the seam's wiring end to
    /// end without any real venue or network.
    struct FakeClient {
        sink: Arc<dyn LiveDataSink>,
        next_id: AtomicU64,
        unsubscribed: Mutex<Vec<SubscriptionId>>,
        shutdown_called: Mutex<bool>,
    }

    impl FakeClient {
        fn new(sink: Arc<dyn LiveDataSink>) -> Self {
            Self {
                sink,
                next_id: AtomicU64::new(0),
                unsubscribed: Mutex::new(Vec::new()),
                shutdown_called: Mutex::new(false),
            }
        }
    }

    impl DataClient for FakeClient {
        fn subscribe_bars(
            &mut self,
            symbol: &str,
            interval: &str,
        ) -> Result<SubscriptionId, LiveDataError> {
            self.sink.seed_bars("fake", symbol, interval, vec![fake_bar(10.0)]);
            self.sink.close_bar("fake", symbol, interval, fake_bar(11.0));
            let id = self.next_id.fetch_add(1, Ordering::SeqCst);
            Ok(SubscriptionId(id))
        }
        fn subscribe_quotes(&mut self, _symbol: &str) -> Result<SubscriptionId, LiveDataError> {
            Err(LiveDataError::Unsupported("fake client serves bars only"))
        }
        fn subscribe_trades(&mut self, _symbol: &str) -> Result<SubscriptionId, LiveDataError> {
            Err(LiveDataError::Unsupported("fake client serves bars only"))
        }
        fn subscribe_book(&mut self, _symbol: &str) -> Result<SubscriptionId, LiveDataError> {
            Err(LiveDataError::Unsupported("fake client serves bars only"))
        }
        fn unsubscribe(&mut self, id: SubscriptionId) {
            self.unsubscribed.lock().unwrap().push(id);
        }
        fn shutdown(&mut self) {
            *self.shutdown_called.lock().unwrap() = true;
        }
    }

    #[test]
    fn subscribe_bars_streams_seed_and_close_through_the_sink_and_returns_fresh_ids() {
        let sink = Arc::new(RecordingSink::default());
        let mut client = FakeClient::new(sink.clone());

        let id1 = client.subscribe_bars("BTCUSDT", "1m").expect("subscribe ok");
        let id2 = client.subscribe_bars("ETHUSDT", "1m").expect("subscribe ok");
        assert_ne!(id1, id2, "distinct ids per subscribe");

        assert_eq!(
            sink.calls(),
            vec![
                "seed_bars(fake,BTCUSDT,1m,1)".to_string(),
                "close_bar(fake,BTCUSDT,1m,11)".to_string(),
                "seed_bars(fake,ETHUSDT,1m,1)".to_string(),
                "close_bar(fake,ETHUSDT,1m,11)".to_string(),
            ]
        );
    }

    #[test]
    fn quotes_and_trades_are_unsupported_on_the_fake_bars_only_client() {
        let sink = Arc::new(RecordingSink::default());
        let mut client = FakeClient::new(sink);

        match client.subscribe_quotes("BTCUSDT") {
            Err(LiveDataError::Unsupported(_)) => {}
            other => panic!("expected Unsupported, got {other:?}"),
        }
        match client.subscribe_trades("BTCUSDT") {
            Err(LiveDataError::Unsupported(_)) => {}
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn unsubscribe_and_shutdown_are_recorded() {
        let sink = Arc::new(RecordingSink::default());
        let mut client = FakeClient::new(sink);

        let id = client.subscribe_bars("BTCUSDT", "1m").expect("subscribe ok");
        client.unsubscribe(id);
        client.shutdown();

        assert_eq!(*client.unsubscribed.lock().unwrap(), vec![id]);
        assert!(*client.shutdown_called.lock().unwrap());
    }

    #[test]
    fn fake_client_book_unsupported() {
        let sink = Arc::new(RecordingSink::default());
        let mut client = FakeClient::new(sink);

        match client.subscribe_book("BTCUSDT") {
            Err(LiveDataError::Unsupported(_)) => {}
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn tee_fans_out_in_order() {
        let sink1 = Arc::new(RecordingSink::default());
        let sink2 = Arc::new(RecordingSink::default());
        let tee = TeeSink(vec![sink1.clone(), sink2.clone()]);

        tee.seed_bars("fake", "BTCUSDT", "1m", vec![fake_bar(10.0)]);
        tee.quote(
            "fake",
            "BTCUSDT",
            QuoteTick {
                ts: 1,
                local_ts: 0,
                bid: 100.0,
                ask: 101.0,
                bid_size: 1.0,
                ask_size: 2.0,
                symbol: String::new(),
            },
        );
        tee.trade(
            "fake",
            "BTCUSDT",
            TradeTick {
                ts: 2,
                local_ts: 0,
                price: 100.5,
                size: 0.5,
                is_buyer_maker: true,
                symbol: String::new(),
            },
        );
        let mut book = L2Book::new(0.5);
        book.apply_snapshot(1, &[(100.0, 1.0)], &[(101.0, 1.0)]);
        tee.book("fake", "BTCUSDT", Arc::new(book.clone()));
        // l2_snapshot MUST be fanned out too — it has a default no-op in the trait, so a missing
        // TeeSink override silently drops every depth snapshot (the DOM book store goes STALE). This
        // assertion is the regression guard for exactly that bug.
        tee.l2_snapshot("fake", "BTCUSDT", 0.5, vec![(100.0, 2.0)], vec![(101.0, 3.0)], 7);
        // bar_close_tick likewise defaults to a no-op in the trait — a missing TeeSink override
        // would silently drop every candle-close tick (the core's bar-close slot goes stale).
        tee.bar_close_tick("fake", "BTCUSDT", 100.25, 6);
        tee.book_update(
            "fake",
            "BTCUSDT",
            BookUpdate {
                ts: 8,
                local_ts: 9,
                seq: 3,
                kind: BookUpdateKind::Delta,
                tick_size: 0.5,
                bids: vec![(100.0, 2.0)],
                asks: vec![],
                symbol: String::new(),
            },
        );

        let expected = vec![
            "seed_bars(fake,BTCUSDT,1m,1)".to_string(),
            "quote:fake:BTCUSDT:100/101:1x2".to_string(),
            "trade:fake:BTCUSDT:100.5/0.5:maker=true".to_string(),
            format!("book:fake:BTCUSDT:{:?}", book.mid()),
            "l2_snapshot(fake,BTCUSDT,0.5,1b/1a,7)".to_string(),
            "bar_close_tick(fake,BTCUSDT,100.25,6)".to_string(),
            "book_update:fake:BTCUSDT:Delta:seq=3:bids=1:asks=0:local_ts_pos=true:tick=0.5"
                .to_string(),
        ];
        assert_eq!(sink1.calls(), expected, "sink1 records every verb in order");
        assert_eq!(sink2.calls(), expected, "sink2 records every verb in order");
    }

    #[test]
    fn stream_status_disclosed_in_gap_order() {
        // The §B disclosure sequence a consumer sees across one outage: seed → close → GapStart →
        // (reconnect + re-seed) → Live. Driven directly here; the feed watchdog produces it live.
        let sink = RecordingSink::default();
        sink.seed_bars("binance", "BTCUSDT", "1m", vec![fake_bar(10.0)]);
        sink.close_bar("binance", "BTCUSDT", "1m", fake_bar(11.0));
        sink.stream_status("binance", "BTCUSDT", "1m", StreamStatus::GapStart { at_ts_ms: 500 });
        sink.seed_bars("binance", "BTCUSDT", "1m", vec![fake_bar(12.0)]);
        sink.stream_status(
            "binance",
            "BTCUSDT",
            "1m",
            StreamStatus::Live { gap_started_ts_ms: Some(500) },
        );
        assert_eq!(
            sink.calls(),
            vec![
                "seed_bars(binance,BTCUSDT,1m,1)".to_string(),
                "close_bar(binance,BTCUSDT,1m,11)".to_string(),
                "stream_status:binance:BTCUSDT:1m:GapStart { at_ts_ms: 500 }".to_string(),
                "seed_bars(binance,BTCUSDT,1m,1)".to_string(),
                "stream_status:binance:BTCUSDT:1m:Live { gap_started_ts_ms: Some(500) }"
                    .to_string(),
            ]
        );
    }

    /// Seam-growth guard: a sink that implements only the REQUIRED verbs (this file's oldest
    /// shape) still compiles and inherits `bar_close_tick` as a no-op — the additive-trait-method
    /// contract that keeps every existing sink source-compatible.
    #[test]
    fn bar_close_tick_defaults_to_a_noop_on_a_minimal_sink() {
        struct MinimalSink;
        impl LiveDataSink for MinimalSink {
            fn seed_bars(&self, _v: &str, _s: &str, _i: &str, _b: Vec<Bar>) {}
            fn close_bar(&self, _v: &str, _s: &str, _i: &str, _b: Bar) {}
            fn forming_bar(&self, _v: &str, _s: &str, _i: &str, _b: Bar) {}
            fn mark_tick(&self, _v: &str, _s: &str, _p: f64, _t: i64) {}
            fn quote(&self, _v: &str, _s: &str, _q: QuoteTick) {}
            fn trade(&self, _v: &str, _s: &str, _t: TradeTick) {}
            fn book(&self, _v: &str, _s: &str, _b: Arc<L2Book>) {}
        }
        // Inherited default: callable, side-effect free, never panics.
        MinimalSink.bar_close_tick("binance", "BTCUSDT.P", 100.0, 1);
    }

    #[test]
    fn tee_fans_stream_status() {
        let s1 = Arc::new(RecordingSink::default());
        let s2 = Arc::new(RecordingSink::default());
        let tee = TeeSink(vec![s1.clone(), s2.clone()]);
        tee.stream_status("okx", "BTC-USDT-SWAP", "quotes", StreamStatus::GapStart { at_ts_ms: 7 });
        let want =
            vec!["stream_status:okx:BTC-USDT-SWAP:quotes:GapStart { at_ts_ms: 7 }".to_string()];
        assert_eq!(s1.calls(), want);
        assert_eq!(s2.calls(), want, "TeeSink fans status to every inner sink");
    }

    /// `require_live_verb` mirrors the declared `VenueCaps.live_data` matrix EXACTLY, for every
    /// roster venue × verb — the drift-proof the hand-rolled feed refusals lack. Unknown venue
    /// ids refuse every verb (fail-closed `UNSUPPORTED` row).
    #[test]
    fn require_live_verb_is_driven_by_the_declared_matrix() {
        use vike_model::LiveVerb;
        const VERBS: [LiveVerb; 5] =
            [LiveVerb::Bars, LiveVerb::Quotes, LiveVerb::Trades, LiveVerb::Book, LiveVerb::Depth];
        for &v in vike_model::VENUES {
            let declared = vike_model::caps_for(v).live_data;
            for verb in VERBS {
                let got = super::require_live_verb(v, verb);
                assert_eq!(
                    got.is_ok(),
                    declared.supports(verb),
                    "{v}/{verb:?}: helper must mirror the declared row"
                );
                if let Err(e) = got {
                    assert!(matches!(e, LiveDataError::Unsupported(_)), "{v}/{verb:?}: {e}");
                }
            }
        }
        for verb in VERBS {
            assert!(super::require_live_verb("no-such-venue", verb).is_err(), "fail-closed");
        }
        // spot-checks against the known matrix: binance serves depth but not quotes;
        // polymarket the inverse shape (book, no depth)
        assert!(super::require_live_verb("binance", LiveVerb::Depth).is_ok());
        assert!(super::require_live_verb("binance", LiveVerb::Quotes).is_err());
        assert!(super::require_live_verb("polymarket", LiveVerb::Book).is_ok());
        assert!(super::require_live_verb("polymarket", LiveVerb::Depth).is_err());
    }
}
