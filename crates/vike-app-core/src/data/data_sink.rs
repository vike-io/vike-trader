//! CoreSinkAdapter — adapts the venue-agnostic vike_data::LiveDataSink onto the core's
//! concrete ingest lanes (lossless BarSender; conflating MarketSender). Lives HERE (the
//! composition root) so vike-data never depends on vike-exec (spec D1; Phase 2 may move it).
//!
//! Send-error policy: `BarSender::seed`/`close` and `TickSender::quote`/`trade`/`book` are all
//! blocking-lossless and only fail with `CoreGone` (the ingest channel's receiver — the vt-core
//! thread — has exited). `MarketSender::publish`/`publish_forming` are wait-free and infallible
//! (a full/closed ingest queue just means the conflation marker is retried on the next tick —
//! see `runtime.rs`'s `arm_marker`). A feed thread has no channel back to its `Feeds` owner to
//! request a stop, so on a `CoreGone` send failure this adapter cannot stop the feed itself;
//! it logs ONE `tracing::warn!` for the adapter's whole lifetime (guarded by `Once`) rather
//! than one per dropped message, mirroring how quiet the old `feed_main` was on this same
//! path (it just `return`ed without logging at all — see `git show 1509994` for the pre-seam
//! shape). In practice this path is near-unreachable: `App::on_exit` stops every feed BEFORE
//! calling `core.shutdown_and_join()`, so a live feed thread should never observe `CoreGone`.
/// The GUI-readable live order-book store: the L2 sink lane the core has no home for. Depth
/// snapshots fold into one [`vike_model::L2Book`] per `(venue, symbol)` here (feed-thread writes),
/// and each Trade window reads a cheap clone each frame (GUI-thread reads) — the same lossy-latest,
/// no-back-pressure discipline as the arc-swap `CoreSnapshot`, just for the book the core doesn't
/// carry. Keyed by `(venue, symbol)` so Trade windows on different venues hold their books side by
/// side. Each entry also carries the RECEIPT epoch-ms of its last update, so a window can flag a
/// stale (frozen / not-yet-arrived) book.
#[derive(Default)]
pub struct BookStore {
    books: std::sync::Mutex<std::collections::HashMap<(String, String), (vike_model::L2Book, i64)>>,
}

/// Infer the instrument tick from a depth snapshot: the smallest positive gap between adjacent
/// price levels (Binance levels sit on the true tick grid, so some adjacent pair is one tick
/// apart). Falls back to 0.01 when it can't be inferred (< 2 levels).
fn infer_tick(bids: &[vike_model::BookLevel], asks: &[vike_model::BookLevel]) -> f64 {
    let mut prices: Vec<f64> = bids.iter().chain(asks).map(|l| l.price).collect();
    prices.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut min_diff = f64::INFINITY;
    for w in prices.windows(2) {
        let d = w[1] - w[0];
        if d > 1e-9 && d < min_diff {
            min_diff = d;
        }
    }
    if min_diff.is_finite() { min_diff } else { 0.01 }
}

impl BookStore {
    /// Replace `symbol`'s book from a full depth snapshot. Uses the feed's authoritative
    /// `tick_size` so the GUI grid is identical to the feed's (and stable across frames); only
    /// falls back to inference if the feed didn't supply one (`<= 0`) — never for a real venue feed.
    pub fn update(
        &self,
        venue: &str,
        symbol: &str,
        tick_size: f64,
        bids: Vec<vike_model::BookLevel>,
        asks: Vec<vike_model::BookLevel>,
        ts: i64,
    ) {
        let tick = if tick_size > 0.0 { tick_size } else { infer_tick(&bids, &asks) };
        let mut book = vike_model::L2Book::new(tick);
        book.apply_snapshot(0, &bids, &asks);
        self.books.lock().unwrap().insert((venue.to_string(), symbol.to_string()), (book, ts));
    }

    /// Store a full, already-folded [`vike_model::L2Book`] directly, keyed by `(venue, symbol)` and
    /// stamped with `ts` for staleness. The `book()`-verb twin of [`Self::update`] (which rebuilds a book
    /// from raw levels): a feed that emits a WHOLE book instead of raw depth snapshots — the
    /// Polymarket market feed, whose `subscribe_book` pushes `LiveDataSink::book` — lands here, so a
    /// GUI reader (the Polymarket cockpit) sees a live book exactly as a Trade window sees a crypto venue's
    /// `l2_snapshot`. The book already carries its own `tick_size`, so nothing is inferred.
    pub fn insert_book(&self, venue: &str, symbol: &str, book: vike_model::L2Book, ts: i64) {
        self.books.lock().unwrap().insert((venue.to_string(), symbol.to_string()), (book, ts));
    }

    /// A clone of `(venue, symbol)`'s current book + its last-update epoch-ms (≈hundreds of levels
    /// — cheap), or `None` before the first snapshot arrives.
    pub fn get(&self, venue: &str, symbol: &str) -> Option<(vike_model::L2Book, i64)> {
        self.books.lock().unwrap().get(&(venue.to_string(), symbol.to_string())).cloned()
    }

    /// Drop ONE key's book — the §7.3 rule-1 disclosure the datahub market-data wire needs, and
    /// the reason [`BookStore::clear`] could not serve it.
    ///
    /// A `Status::GapStart`/`Status::Stale` on a book lane says one `(venue, symbol)` can no longer
    /// be trusted. ⚠ **A stale ladder that is known to be wrong is worse than an empty one** — the
    /// Trade window's own reader already renders `None` as "no book yet" and a present-but-old book
    /// as a STALE ladder a human may still read prices off — **and clearing EVERY book because one
    /// venue gapped is worse still**: this store is ONE map shared by every Trade window and every open
    /// Polymarket cockpit token, so `clear()` on a bybit gap would blank the binance ladder and
    /// every cockpit beside it. Hence a per-key remove.
    ///
    /// Removing an absent key is a no-op, so a gap on a key that never got its first snapshot costs
    /// nothing and needs no guard at the call site.
    pub fn remove(&self, venue: &str, symbol: &str) {
        self.books.lock().unwrap().remove(&(venue.to_string(), symbol.to_string()));
    }

    /// Drop every cached book — the feed-plane teardown
    /// (`crate::backend::backend_conn::teardown_feed_plane`). The keys carry `(venue, symbol)` and no
    /// backend dimension because the content HAS none: a book is venue truth, which is why a
    /// backend SWITCH keeps it (split-plane B2) and only a feed-plane teardown drops it.
    pub fn clear(&self) {
        self.books.lock().unwrap().clear();
    }
}

/// Cap on the number of buffered trades held per `(venue, symbol)` key in [`TradeStore`]. Chosen
/// generously above any plausible one-frame drain size — this bounds worst-case memory on a
/// symbol nobody is draining (e.g. a stale/abandoned chart window), not steady-state throughput.
const TRADE_STORE_CAP: usize = 50_000;

/// The GUI-readable live trade-tape buffer: like [`BookStore`], a lane the core has no home for.
/// Trades fold into a per-`(venue, symbol)` FIFO [`std::collections::VecDeque`] here (feed-thread
/// writes via `push`, one call per trade), capped at [`TRADE_STORE_CAP`] with oldest-first
/// eviction so a busy feed can't grow this store unbounded when nobody is reading it. A GUI
/// consumer (tick/volume bar aggregation — see the chart-phase2 plan's Task B5) reads by calling
/// `drain`, which takes everything buffered since its last poll (FIFO order) and leaves the key
/// empty — the same lossy-latest, no-back-pressure discipline as `BookStore`/the arc-swap
/// `CoreSnapshot`, just take-all instead of latest-only since every trade (not just the newest)
/// matters for aggregation.
#[derive(Default)]
pub struct TradeStore {
    trades: std::sync::Mutex<
        std::collections::HashMap<
            (String, String),
            std::collections::VecDeque<vike_model::TradeTick>,
        >,
    >,
}

impl TradeStore {
    /// Append one trade for `(venue, tick.symbol)`, dropping the oldest buffered trade(s) first
    /// once the per-key buffer exceeds [`TRADE_STORE_CAP`].
    pub fn push(&self, venue: &str, tick: &vike_model::TradeTick) {
        let mut trades = self.trades.lock().unwrap();
        let dq = trades.entry((venue.to_string(), tick.symbol.clone())).or_default();
        dq.push_back(tick.clone());
        while dq.len() > TRADE_STORE_CAP {
            dq.pop_front();
        }
    }

    /// Drop every buffered trade — the feed-plane teardown
    /// (`crate::backend::backend_conn::teardown_feed_plane`), same argument as `BookStore::clear`.
    pub fn clear(&self) {
        self.trades.lock().unwrap().clear();
    }

    /// Take every trade buffered for `(venue, symbol)`, oldest first, leaving the key empty for
    /// the next poll.
    pub fn drain(&self, venue: &str, symbol: &str) -> Vec<vike_model::TradeTick> {
        let mut trades = self.trades.lock().unwrap();
        trades
            .get_mut(&(venue.to_string(), symbol.to_string()))
            .map(|dq| dq.drain(..).collect())
            .unwrap_or_default()
    }
}

/// Cap on the closed bars held per series in [`DirectBarStore`] — bounds worst-case memory on a
/// long-lived GUI process the way [`TRADE_STORE_CAP`] does for the tape. Comfortably above every
/// venue's kline REST seed depth (the deepest, `fetch_klines_latest`-family warmups, serve well
/// under 1_500 bars), so a seed is never trimmed in practice; live closes evict oldest-first once
/// the cap is reached. Front-eviction is safe for the render path: `ChartState::sync` detects the
/// changed first-ts and takes its full-rebuild arm.
pub const DIRECT_BAR_CLOSED_CAP: usize = 5_000;

/// One series' bars in the [`DirectBarStore`]: the closed history (`Arc`-wrapped exactly like
/// `vike_exec::BarSeries::closed` / `TickVolAgg::closed`, so the per-frame fold can hand it to
/// `ChartState::sync` without copying) plus the latest forming snapshot.
#[derive(Default, Clone)]
struct DirectSeries {
    closed: std::sync::Arc<Vec<vike_model::Bar>>,
    forming: Option<vike_model::Bar>,
}

/// The GUI-readable live BAR store (split-plane B2's direct-bar follow-up): the kline lane the
/// third mode's core-free sink previously dropped, now landed — [`BookStore`]/[`TradeStore`]'s
/// sibling, fed by [`GuiFeedSink`]'s bar lanes (feed-thread writes) and drained by
/// [`core_sync::sync_from_core`](crate::ui::core_sync::sync_from_core)'s direct-bar fold (GUI-thread
/// reads) into the charts whose render source is
/// [`SeriesSource::DirectBars`](crate::backend::split_plane::SeriesSource::DirectBars).
///
/// **The fold rules deliberately MIRROR the core's own bar-cache arms** (`crates/vike-core/src/
/// runtime/mod.rs`'s `Ingest::BarSeed` / `Ingest::BarClose` arms and its conflated forming
/// drain), so a third-mode kline series holds the SAME content the fat arm's core cache would
/// hold from the identical feed lane — the venue's REST warmup seed, then live closes:
///
/// - [`seed`](Self::seed) REPLACES the series (closed = the seed, forming cleared) — a re-seed
///   after a WS reconnect repairs the lossless lane exactly as `Ingest::BarSeed` does;
/// - [`close`](Self::close) appends ts-ascending, with THE BOUNDARY-TS DEDUP RULE: a close at
///   the last held ts REPLACES that bar idempotently (the reconnect-overlap / seed-boundary
///   case — the venue re-closing the window the seed already carried must never duplicate it),
///   a strictly older close is DROPPED (stale replay), and any close clears a forming snapshot
///   at or below its ts;
/// - [`forming`](Self::forming) is latest-wins, kept only STRICTLY ABOVE the last closed ts (a
///   late conflated snapshot of an already-closed window must not paint the same bar twice).
///
/// The one divergence from the core: closed history is bounded ([`DIRECT_BAR_CLOSED_CAP`],
/// oldest-first eviction) because this store lives in a GUI process with no snapshot cadence to
/// bound it. And one addition the core never needed:
/// [`seed_backend_tail`](Self::seed_backend_tail), the third mode's HISTORY seam — a venue whose
/// feed serves no REST warmup (hyperliquid's candle pump is live-only) would otherwise start
/// every chart empty, so the fold offers the backend's streamed snapshot tail as a seed exactly
/// ONCE per series (only while it holds NO closed bars); live closes then append onto it through
/// the same boundary-ts rule above, and a venue REST seed arriving later replaces it wholesale.
///
/// Accepted residual (same class as a reaped tape key): a series whose window closed keeps its
/// bars until the feed-plane teardown ([`Self::clear`]) — bounded by the cap, replaced by the
/// fresh seed on any re-subscribe.
#[derive(Default)]
pub struct DirectBarStore {
    series: std::sync::Mutex<std::collections::HashMap<(String, String, String), DirectSeries>>,
    /// Bumped on every state-changing write — the fold's cheap dirty probe
    /// ([`Self::generation`]), the direct-bar sibling of `CoreSnapshot.seq`.
    generation: std::sync::atomic::AtomicU64,
}

impl DirectBarStore {
    fn bump(&self) {
        self.generation.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Monotonic write counter: unchanged since the last read ⇒ every series is unchanged, so
    /// the per-frame fold can skip its whole pass (the `snap.seq` idiom, store-side).
    pub fn generation(&self) -> u64 {
        self.generation.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The venue REST-warmup seed (`LiveDataSink::seed_bars`): REPLACE the series outright —
    /// closed becomes `bars` (cap-trimmed oldest-first), forming cleared for the live stream to
    /// refresh — mirroring `Ingest::BarSeed`. Also how a reconnect re-seed repairs the series.
    pub fn seed(&self, venue: &str, symbol: &str, interval: &str, mut bars: Vec<vike_model::Bar>) {
        let over = bars.len().saturating_sub(DIRECT_BAR_CLOSED_CAP);
        if over > 0 {
            bars.drain(..over);
        }
        let mut map = self.series.lock().unwrap();
        let entry =
            map.entry((venue.to_string(), symbol.to_string(), interval.to_string())).or_default();
        entry.closed = std::sync::Arc::new(bars);
        entry.forming = None;
        drop(map);
        self.bump();
    }

    /// One venue-closed bar (`LiveDataSink::close_bar`) — the lossless lane, folded with the
    /// boundary-ts dedup rule (see the type doc): append if newer, replace idempotently at the
    /// same ts, drop if older; then clear any forming snapshot at or below the closed ts.
    pub fn close(&self, venue: &str, symbol: &str, interval: &str, bar: vike_model::Bar) {
        let mut map = self.series.lock().unwrap();
        let entry =
            map.entry((venue.to_string(), symbol.to_string(), interval.to_string())).or_default();
        let changed = match entry.closed.last().map(|b| b.ts) {
            // reconnect/seed-boundary overlap: the same window re-closes — replace idempotently
            Some(t) if bar.ts == t => {
                if let Some(last) = std::sync::Arc::make_mut(&mut entry.closed).last_mut() {
                    *last = bar.clone();
                }
                true
            }
            Some(t) if bar.ts < t => false, // stale replay — drop
            _ => {
                let closed = std::sync::Arc::make_mut(&mut entry.closed);
                closed.push(bar.clone());
                let over = closed.len().saturating_sub(DIRECT_BAR_CLOSED_CAP);
                if over > 0 {
                    closed.drain(..over);
                }
                true
            }
        };
        // a close supersedes any forming state of that (or an older) window
        if entry.forming.as_ref().is_some_and(|f| f.ts <= bar.ts) {
            entry.forming = None;
        }
        drop(map);
        if changed {
            self.bump();
        }
    }

    /// The conflating forming-bar snapshot (`LiveDataSink::forming_bar`): latest-wins, kept only
    /// strictly above the last closed ts (the core's own reconnect-race guard).
    pub fn forming(&self, venue: &str, symbol: &str, interval: &str, bar: vike_model::Bar) {
        let mut map = self.series.lock().unwrap();
        let entry =
            map.entry((venue.to_string(), symbol.to_string(), interval.to_string())).or_default();
        if entry.closed.last().is_none_or(|last| bar.ts > last.ts) {
            entry.forming = Some(bar);
            drop(map);
            self.bump();
        }
    }

    /// THE HISTORY SEAM (the third mode's seed decision, made with the code): offer the
    /// BACKEND's streamed bar tail as this series' initial history. Applies — and returns
    /// `true` — ONLY while the series holds no closed bars, so it happens at most once per
    /// series life: after it, live closes append through the boundary-ts rule, a venue REST
    /// seed replaces it wholesale, and neither a later snapshot nor a backend SWITCH can
    /// re-import another backend's bars under the accumulated venue truth. Any forming
    /// snapshot the venue already delivered survives (only a real venue seed clears forming).
    pub fn seed_backend_tail(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        tail: &[vike_model::Bar],
    ) -> bool {
        if tail.is_empty() {
            return false;
        }
        let mut map = self.series.lock().unwrap();
        let entry =
            map.entry((venue.to_string(), symbol.to_string(), interval.to_string())).or_default();
        if !entry.closed.is_empty() {
            return false;
        }
        let start = tail.len().saturating_sub(DIRECT_BAR_CLOSED_CAP);
        entry.closed = std::sync::Arc::new(tail[start..].to_vec());
        // A forming snapshot at or below the tail's last ts is stale against the seeded history
        // — same supersede rule as `close`.
        let last_ts = entry.closed.last().map(|b| b.ts).unwrap_or(i64::MIN);
        if entry.forming.as_ref().is_some_and(|f| f.ts <= last_ts) {
            entry.forming = None;
        }
        drop(map);
        self.bump();
        true
    }

    /// A cheap read of one series: the closed history (`Arc` clone) + the forming snapshot —
    /// the exact pair `ChartState::sync` takes. `None` before any write for the key.
    pub fn series(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
    ) -> Option<(std::sync::Arc<Vec<vike_model::Bar>>, Option<vike_model::Bar>)> {
        self.series
            .lock()
            .unwrap()
            .get(&(venue.to_string(), symbol.to_string(), interval.to_string()))
            .map(|s| (s.closed.clone(), s.forming.clone()))
    }

    /// Every `(venue, symbol, interval)` the store holds — the fold's iteration set.
    pub fn keys(&self) -> Vec<(String, String, String)> {
        self.series.lock().unwrap().keys().cloned().collect()
    }

    /// Drop every series — the feed-plane teardown
    /// (`crate::backend::backend_conn::teardown_feed_plane`), same argument as [`BookStore::clear`]: the
    /// content is venue truth, so a backend SWITCH keeps it and only a feed-plane teardown
    /// drops it.
    pub fn clear(&self) {
        self.series.lock().unwrap().clear();
        self.bump();
    }
}

pub struct CoreSinkAdapter {
    /// the shared feed→core-lane forwarder (bars/marks/ticks/stream-status + the warn-once policy)
    core: vike_core::CoreLaneSink,
    /// live order books for the Trade windows' ladders (the L2 lane the core doesn't carry)
    pub books: std::sync::Arc<BookStore>,
    /// live trade tape for GUI-side aggregation (the tick lane the core doesn't carry)
    pub trades: std::sync::Arc<TradeStore>,
}

impl CoreSinkAdapter {
    pub fn new(
        bars: vike_exec::BarSender,
        market: vike_exec::MarketSender,
        books: std::sync::Arc<BookStore>,
        trades: std::sync::Arc<TradeStore>,
        ticks: vike_exec::TickSender,
    ) -> Self {
        CoreSinkAdapter { core: vike_core::CoreLaneSink::new(bars, market, ticks), books, trades }
    }
}

impl vike_data::LiveDataSink for CoreSinkAdapter {
    fn seed_bars(&self, venue: &str, symbol: &str, interval: &str, bars: Vec<vike_model::Bar>) {
        self.core.seed_bars(venue, symbol, interval, bars);
    }

    fn close_bar(&self, venue: &str, symbol: &str, interval: &str, bar: vike_model::Bar) {
        self.core.close_bar(venue, symbol, interval, bar);
    }

    fn forming_bar(&self, venue: &str, symbol: &str, interval: &str, bar: vike_model::Bar) {
        self.core.forming_bar(venue, symbol, interval, bar);
    }

    fn mark_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        self.core.mark_tick(venue, symbol, px, ts);
    }

    fn bar_close_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        // Must forward, NOT inherit the trait default (a no-op) — else every kline feed's
        // candle-close tick dies here and the core's bar-close price slot never fills.
        self.core.bar_close_tick(venue, symbol, px, ts);
    }

    fn l2_snapshot(
        &self,
        venue: &str,
        symbol: &str,
        tick_size: f64,
        bids: Vec<vike_model::BookLevel>,
        asks: Vec<vike_model::BookLevel>,
        ts: i64,
    ) {
        // The book lane has no core home — land it directly in the GUI-readable store, keyed by
        // (venue, symbol) on the feed's own tick grid, stamped with the receipt time for staleness.
        self.books.update(venue, symbol, tick_size, bids, asks, ts);
    }

    fn quote(&self, venue: &str, symbol: &str, quote: vike_model::QuoteTick) {
        self.core.quote(venue, symbol, quote);
    }

    fn trade(&self, venue: &str, symbol: &str, trade: vike_model::TradeTick) {
        // GUI-readable trade tape (no core home, like the book lane above) — pushed here in
        // addition to, not instead of, the core-bound forward below; the recorder path (tee'd in
        // ahead of this adapter, see the module doc) is untouched.
        self.trades.push(venue, &trade);
        self.core.trade(venue, symbol, trade);
    }

    fn book(&self, venue: &str, symbol: &str, book: std::sync::Arc<vike_model::L2Book>) {
        // Also land the full book in the GUI-readable store (keyed by (venue, symbol)) so a reader
        // like the Polymarket cockpit — whose feed emits `book()` (a whole L2Book) rather than the
        // crypto depth feeds' `l2_snapshot()` — sees a live book. Deep-cloned OUT of the `Arc`
        // before the core forward, because `BookStore` owns plain `L2Book`s the GUI reads by
        // value; this clone is unchanged from the pre-`Arc` shape and stays on the feed thread,
        // never on the single-writer core (making the store itself `Arc`-backed is a separate,
        // GUI-facing change — the shell, `vike-app` when this was written, ran no test over it).
        // The crypto depth
        // path is unaffected (those feeds populate the store via `l2_snapshot`, never `book`); the
        // only other `book()` caller, the ibkr feed, `l2_snapshot`s the SAME book first, so this
        // write is a harmless same-value overwrite.
        self.books.insert_book(venue, symbol, (*book).clone(), vike_model::now_ms());
        self.core.book(venue, symbol, book);
    }

    /// Net-hardening §B consumer: surface stream-health transitions as machine-readable log events
    /// (the human-facing UI string is set by the feed's own `set_status`) AND forward the
    /// transition to the live core (via [`vike_core::CoreLaneSink`]) so a MOUNTED STRATEGY's
    /// `on_feed_status` hook can react (e.g. pull its quotes on disconnect). The log detail is
    /// app-side; the 1:1 `StreamStatus`→`FeedStatus` forward + warn-once policy live in the shared
    /// sink. `StreamStatus` is `Copy`, so the match below does not consume it.
    fn stream_status(
        &self,
        venue: &str,
        symbol: &str,
        stream: &str,
        status: vike_data::StreamStatus,
    ) {
        match status {
            vike_data::StreamStatus::GapStart { at_ts_ms } => {
                tracing::warn!(
                    venue,
                    symbol,
                    stream,
                    at_ts_ms,
                    "market-data gap started — feed idle/dropped; data missing (ticks) or stale (forming bars) until it recovers"
                );
            }
            vike_data::StreamStatus::Stale { newest_data_ts_ms, now_ms } => {
                tracing::warn!(
                    venue,
                    symbol,
                    stream,
                    newest_data_ts_ms,
                    now_ms,
                    "data-stale: transport alive but no fresh data within the freshness window"
                );
            }
            vike_data::StreamStatus::Live { gap_started_ts_ms } => {
                tracing::info!(
                    venue,
                    symbol,
                    stream,
                    ?gap_started_ts_ms,
                    "market-data stream live again (transport re-seed or data fresh)"
                );
            }
        }
        self.core.stream_status(venue, symbol, stream, status);
    }
}

/// The THIRD MODE's live-data seam (split-plane B2): [`CoreSinkAdapter`] minus the core lanes,
/// for the `--observe`-with-feeds arm where NO local core exists to receive them.
///
/// This struct is the double-fold guard made structural
/// ([`split_plane::series_render_source`](crate::backend::split_plane::series_render_source) is the
/// decision it implements). What lands, and where, mirrors [`CoreSinkAdapter`]'s GUI-side half —
/// plus the direct-bar path (B2's kline follow-up), which gives the bar lanes the core-free home
/// they used to lack:
///
/// - `seed_bars` / `close_bar` / `forming_bar` → the [`DirectBarStore`] (third-mode kline
///   charts on venues with a local bar feed render from it — venue-direct, no backend tail);
/// - `trade` → the [`TradeStore`] tape (tick/volume + orderflow aggregation — client-direct);
/// - `l2_snapshot` / `book` → the [`BookStore`] (the Trade window's ladder + Polymarket cockpit —
///   client-direct);
/// - `stream_status` → the same machine-readable gap/stale/live log lines, minus the core
///   forward (no mounted strategy exists to react);
/// - `mark_tick` / `bar_close_tick` / `quote` remain consumer-less: their only consumer is the
///   core's `PriceBoard`, which does not exist here — explicit no-ops, not accidents.
pub struct GuiFeedSink {
    /// Live order books for the Trade windows' ladders — same store, same keying as
    /// [`CoreSinkAdapter`].
    pub books: std::sync::Arc<BookStore>,
    /// Live trade tape for GUI-side aggregation — same store as [`CoreSinkAdapter`].
    pub trades: std::sync::Arc<TradeStore>,
    /// Live kline bars for the third mode's direct-rendered charts — the store
    /// [`core_sync::sync_from_core`](crate::ui::core_sync::sync_from_core)'s direct-bar fold reads.
    pub bars: std::sync::Arc<DirectBarStore>,
}

impl vike_data::LiveDataSink for GuiFeedSink {
    // ── the BAR lanes (the direct-bar path — B2's kline follow-up) ──────────────────────────
    // These used to be the double-fold guard's explicit no-ops ("the direct-bar follow-up's
    // input"). They now land in the DirectBarStore, and the guard moved WHERE it belongs: the
    // render-source decision (`split_plane::series_render_source`) assigns each kline series to
    // exactly ONE of `snap.bars` and this store, and `core_sync`'s two folds each consult it —
    // so a series still can never paint from both sources.
    fn seed_bars(&self, venue: &str, symbol: &str, interval: &str, bars: Vec<vike_model::Bar>) {
        self.bars.seed(venue, symbol, interval, bars);
    }

    fn close_bar(&self, venue: &str, symbol: &str, interval: &str, bar: vike_model::Bar) {
        self.bars.close(venue, symbol, interval, bar);
    }

    fn forming_bar(&self, venue: &str, symbol: &str, interval: &str, bar: vike_model::Bar) {
        self.bars.forming(venue, symbol, interval, bar);
    }

    // ── the DROPPED lanes (consumer-less without a core, spelled rather than inherited) ─────
    // Mark/close ticks and quotes feed the core's PriceBoard valuation slots; no core, no slot.
    fn mark_tick(&self, _venue: &str, _symbol: &str, _px: f64, _ts: i64) {}

    fn quote(&self, _venue: &str, _symbol: &str, _quote: vike_model::QuoteTick) {}

    // ── the GUI-store lanes (identical to CoreSinkAdapter's GUI half) ───────────────────────
    fn l2_snapshot(
        &self,
        venue: &str,
        symbol: &str,
        tick_size: f64,
        bids: Vec<vike_model::BookLevel>,
        asks: Vec<vike_model::BookLevel>,
        ts: i64,
    ) {
        self.books.update(venue, symbol, tick_size, bids, asks, ts);
    }

    fn trade(&self, venue: &str, symbol: &str, trade: vike_model::TradeTick) {
        let _ = symbol; // the store keys by the tick's own symbol, as CoreSinkAdapter does
        self.trades.push(venue, &trade);
    }

    fn book(&self, venue: &str, symbol: &str, book: std::sync::Arc<vike_model::L2Book>) {
        // Same deep-clone-out-of-the-Arc as `CoreSinkAdapter::book` (the store owns plain
        // `L2Book`s), on the feed's own thread.
        self.books.insert_book(venue, symbol, (*book).clone(), vike_model::now_ms());
    }

    fn stream_status(
        &self,
        venue: &str,
        symbol: &str,
        stream: &str,
        status: vike_data::StreamStatus,
    ) {
        // The same operator-facing transition lines `CoreSinkAdapter::stream_status` emits — the
        // third mode's feeds must not go silent about gaps just because no core is mounted.
        match status {
            vike_data::StreamStatus::GapStart { at_ts_ms } => {
                tracing::warn!(
                    venue,
                    symbol,
                    stream,
                    at_ts_ms,
                    "market-data gap started — feed idle/dropped; data missing (ticks) or stale (forming bars) until it recovers"
                );
            }
            vike_data::StreamStatus::Stale { newest_data_ts_ms, now_ms } => {
                tracing::warn!(
                    venue,
                    symbol,
                    stream,
                    newest_data_ts_ms,
                    now_ms,
                    "data-stale: transport alive but no fresh data within the freshness window"
                );
            }
            vike_data::StreamStatus::Live { gap_started_ts_ms } => {
                tracing::info!(
                    venue,
                    symbol,
                    stream,
                    ?gap_started_ts_ms,
                    "market-data stream live again (transport re-seed or data fresh)"
                );
            }
        }
    }
}

#[path = "data_sink_tests.rs"]
#[cfg(test)]
mod data_sink_tests;
