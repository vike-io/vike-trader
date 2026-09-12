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
/// and the DOM window reads a cheap clone each frame (GUI-thread reads) — the same lossy-latest,
/// no-back-pressure discipline as the arc-swap `CoreSnapshot`, just for the book the core doesn't
/// carry. Keyed by `(venue, symbol)` so the DOM venue switcher can hold Binance/Bybit/OKX books
/// side by side. Each entry also carries the RECEIPT epoch-ms of its last update, so the DOM can
/// flag a stale (frozen / not-yet-arrived) book.
#[derive(Default)]
pub struct BookStore {
    books: std::sync::Mutex<std::collections::HashMap<(String, String), (vike_model::L2Book, i64)>>,
}

/// Infer the instrument tick from a depth snapshot: the smallest positive gap between adjacent
/// price levels (Binance levels sit on the true tick grid, so some adjacent pair is one tick
/// apart). Falls back to 0.01 when it can't be inferred (< 2 levels).
fn infer_tick(bids: &[vike_model::Level], asks: &[vike_model::Level]) -> f64 {
    let mut prices: Vec<f64> = bids.iter().chain(asks).map(|(p, _)| *p).collect();
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
        bids: Vec<vike_model::Level>,
        asks: Vec<vike_model::Level>,
        ts: i64,
    ) {
        let tick = if tick_size > 0.0 { tick_size } else { infer_tick(&bids, &asks) };
        let mut book = vike_model::L2Book::new(tick);
        book.apply_snapshot(0, &bids, &asks);
        self.books.lock().unwrap().insert((venue.to_string(), symbol.to_string()), (book, ts));
    }

    /// Store a full, already-folded [`vike_model::L2Book`] directly, keyed by `(venue, symbol)` and
    /// stamped with `ts` for staleness. The `book()`-verb twin of [`update`] (which rebuilds a book
    /// from raw levels): a feed that emits a WHOLE book instead of raw depth snapshots — the
    /// Polymarket market feed, whose `subscribe_book` pushes `LiveDataSink::book` — lands here, so a
    /// GUI reader (the Polymarket cockpit) sees a live book exactly as the DOM sees a crypto venue's
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
    /// DOM's own reader already renders `None` as "no book yet" and a present-but-old book as a
    /// STALE ladder a human may still read prices off — **and clearing EVERY book because one venue
    /// gapped is worse still**: this store is ONE map shared by every DOM venue and every open
    /// Polymarket cockpit token, so `clear()` on a bybit gap would blank the binance ladder and
    /// every cockpit beside it. Hence a per-key remove.
    ///
    /// Removing an absent key is a no-op, so a gap on a key that never got its first snapshot costs
    /// nothing and needs no guard at the call site.
    pub fn remove(&self, venue: &str, symbol: &str) {
        self.books.lock().unwrap().remove(&(venue.to_string(), symbol.to_string()));
    }

    /// Drop every cached book — the feed-plane teardown
    /// (`crate::backend_conn::teardown_feed_plane`). The keys carry `(venue, symbol)` and no
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
    /// (`crate::backend_conn::teardown_feed_plane`), same argument as `BookStore::clear`.
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
/// [`core_sync::sync_from_core`](crate::core_sync::sync_from_core)'s direct-bar fold (GUI-thread
/// reads) into the charts whose render source is
/// [`SeriesSource::DirectBars`](crate::split_plane::SeriesSource::DirectBars).
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
    /// (`crate::backend_conn::teardown_feed_plane`), same argument as [`BookStore::clear`]: the
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
    /// live order books for the DOM window (the L2 lane the core doesn't carry)
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
        bids: Vec<vike_model::Level>,
        asks: Vec<vike_model::Level>,
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
        // crypto DOM feeds' `l2_snapshot()` — sees a live book. Deep-cloned OUT of the `Arc`
        // before the core forward, because `BookStore` owns plain `L2Book`s the GUI reads by
        // value; this clone is unchanged from the pre-`Arc` shape and stays on the feed thread,
        // never on the single-writer core (making the store itself `Arc`-backed is a separate,
        // GUI-facing change — vike-app is compile-checked but never tested in CI). The crypto DOM
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
/// ([`split_plane::series_render_source`](crate::split_plane::series_render_source) is the
/// decision it implements). What lands, and where, mirrors [`CoreSinkAdapter`]'s GUI-side half —
/// plus the direct-bar path (B2's kline follow-up), which gives the bar lanes the core-free home
/// they used to lack:
///
/// - `seed_bars` / `close_bar` / `forming_bar` → the [`DirectBarStore`] (third-mode kline
///   charts on venues with a local bar feed render from it — venue-direct, no backend tail);
/// - `trade` → the [`TradeStore`] tape (tick/volume + orderflow aggregation — client-direct);
/// - `l2_snapshot` / `book` → the [`BookStore`] (the DOM ladder + Polymarket cockpit —
///   client-direct);
/// - `stream_status` → the same machine-readable gap/stale/live log lines, minus the core
///   forward (no mounted strategy exists to react);
/// - `mark_tick` / `bar_close_tick` / `quote` remain consumer-less: their only consumer is the
///   core's `PriceBoard`, which does not exist here — explicit no-ops, not accidents.
pub struct GuiFeedSink {
    /// Live order books for the DOM window — same store, same keying as [`CoreSinkAdapter`].
    pub books: std::sync::Arc<BookStore>,
    /// Live trade tape for GUI-side aggregation — same store as [`CoreSinkAdapter`].
    pub trades: std::sync::Arc<TradeStore>,
    /// Live kline bars for the third mode's direct-rendered charts — the store
    /// [`core_sync::sync_from_core`](crate::core_sync::sync_from_core)'s direct-bar fold reads.
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
        bids: Vec<vike_model::Level>,
        asks: Vec<vike_model::Level>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trade_store_fifo_drain_and_cap() {
        let s = TradeStore::default();
        let t = |ts| vike_model::TradeTick {
            ts,
            local_ts: 0,
            price: 1.0,
            size: 1.0,
            is_buyer_maker: false,
            symbol: "BTCUSDT".into(),
        };
        s.push("binance", &t(1));
        s.push("binance", &t(2));
        let got = s.drain("binance", "BTCUSDT");
        assert_eq!(got.iter().map(|x| x.ts).collect::<Vec<_>>(), vec![1, 2]);
        assert!(s.drain("binance", "BTCUSDT").is_empty()); // drained
        for i in 0..50_100 {
            s.push("binance", &t(i));
        }
        let got = s.drain("binance", "BTCUSDT");
        assert_eq!(got.len(), 50_000);
        assert_eq!(got[0].ts, 100); // oldest 100 dropped
    }

    /// The third mode's sink (split-plane B2): trades and books land in the SAME GUI stores
    /// `CoreSinkAdapter` fills — the tick/DOM plane is client-direct with no core — and the bar
    /// lanes land in the [`DirectBarStore`] (the direct-bar follow-up: they used to be explicit
    /// no-ops). The remaining core-bound lanes (mark/close ticks, quotes) stay consumer-less.
    #[test]
    fn gui_feed_sink_lands_trades_books_and_bars_and_drops_the_price_board_lanes() {
        use vike_data::LiveDataSink;
        let books = std::sync::Arc::new(BookStore::default());
        let trades = std::sync::Arc::new(TradeStore::default());
        let bars = std::sync::Arc::new(DirectBarStore::default());
        let sink = GuiFeedSink { books: books.clone(), trades: trades.clone(), bars: bars.clone() };

        sink.trade(
            "binance",
            "BTCUSDT",
            vike_model::TradeTick {
                ts: 7,
                local_ts: 0,
                price: 100.0,
                size: 1.0,
                is_buyer_maker: false,
                symbol: "BTCUSDT".into(),
            },
        );
        sink.l2_snapshot("okx", "BTC-USDT", 0.1, vec![(99.9, 1.0)], vec![(100.1, 2.0)], 7);
        sink.book("polymarket", "1071", std::sync::Arc::new(vike_model::L2Book::new(0.01)));

        assert_eq!(trades.drain("binance", "BTCUSDT").len(), 1, "trade tape is client-direct");
        assert!(books.get("okx", "BTC-USDT").is_some(), "DOM books are client-direct");
        assert!(books.get("polymarket", "1071").is_some(), "cockpit books are client-direct");

        // The bar lanes land in the direct store — seed, then a live close, then a forming.
        sink.seed_bars("binance", "BTCUSDT", "1m", vec![dbar(60_000, 10.0)]);
        sink.close_bar("binance", "BTCUSDT", "1m", dbar(120_000, 11.0));
        sink.forming_bar("binance", "BTCUSDT", "1m", dbar(180_000, 12.0));
        let (closed, forming) = bars.series("binance", "BTCUSDT", "1m").expect("bars landed");
        assert_eq!(closed.len(), 2, "seed + live close are the closed history");
        assert_eq!(forming.map(|b| b.ts), Some(180_000), "the forming snapshot rides beside it");

        // The still-dropped lanes: callable (feeds emit them uniformly), landing nowhere.
        sink.mark_tick("binance", "BTCUSDT", 100.0, 7);
        sink.bar_close_tick("binance", "BTCUSDT", 100.0, 7);
        sink.quote(
            "binance",
            "BTCUSDT",
            vike_model::QuoteTick {
                ts: 7,
                local_ts: 0,
                bid: 99.0,
                ask: 101.0,
                bid_size: 1.0,
                ask_size: 1.0,
                symbol: "BTCUSDT".into(),
            },
        );
    }

    /// One flat closed/forming bar at `ts` for the direct-bar store tests.
    fn dbar(ts: i64, px: f64) -> vike_model::Bar {
        vike_model::Bar {
            ts,
            open: px,
            high: px,
            low: px,
            close: px,
            volume: 1.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }
    }

    /// The store mirrors `Ingest::BarSeed`: a seed REPLACES the series — closed becomes the
    /// seed, forming cleared — so a reconnect re-seed repairs the lossless lane instead of
    /// merging into stale history.
    #[test]
    fn direct_store_seed_replaces_the_series_and_clears_forming() {
        let s = DirectBarStore::default();
        s.seed("binance", "BTCUSDT", "1m", vec![dbar(60_000, 10.0), dbar(120_000, 11.0)]);
        s.forming("binance", "BTCUSDT", "1m", dbar(180_000, 12.0));
        s.seed("binance", "BTCUSDT", "1m", vec![dbar(120_000, 11.5)]);
        let (closed, forming) = s.series("binance", "BTCUSDT", "1m").unwrap();
        assert_eq!(closed.len(), 1, "the re-seed replaced the whole closed history");
        assert_eq!(closed[0].close, 11.5);
        assert!(forming.is_none(), "a seed clears forming — the live stream refreshes it");
    }

    /// THE BOUNDARY-TS DEDUP RULE (`Ingest::BarClose`'s): a close at the last held ts replaces
    /// that bar idempotently — the seed's last bar re-closed live must never appear twice — a
    /// strictly older close is dropped, and a newer one appends.
    #[test]
    fn direct_store_close_dedups_at_the_boundary_ts() {
        let s = DirectBarStore::default();
        s.seed("binance", "BTCUSDT", "1m", vec![dbar(60_000, 10.0), dbar(120_000, 11.0)]);

        // The boundary case: the venue re-closes the window the seed already carried.
        s.close("binance", "BTCUSDT", "1m", dbar(120_000, 11.9));
        let (closed, _) = s.series("binance", "BTCUSDT", "1m").unwrap();
        assert_eq!(closed.len(), 2, "same-ts close must REPLACE, never duplicate");
        assert_eq!(closed[1].close, 11.9, "…and the replacement is the venue's newer bar");

        // Stale replay: dropped.
        s.close("binance", "BTCUSDT", "1m", dbar(60_000, 9.0));
        let (closed, _) = s.series("binance", "BTCUSDT", "1m").unwrap();
        assert_eq!(closed.len(), 2);
        assert_eq!(closed[0].close, 10.0, "an older close is a stale replay — dropped");

        // The live append.
        s.close("binance", "BTCUSDT", "1m", dbar(180_000, 12.0));
        let (closed, _) = s.series("binance", "BTCUSDT", "1m").unwrap();
        assert_eq!(closed.iter().map(|b| b.ts).collect::<Vec<_>>(), vec![60_000, 120_000, 180_000]);
    }

    /// The forming lane is latest-wins and only ever STRICTLY ABOVE the last closed ts: a close
    /// supersedes the forming it closes, and a late forming snapshot of an already-closed window
    /// is dropped (the double-paint at the boundary this rule exists for).
    #[test]
    fn direct_store_forming_is_superseded_by_its_close_and_never_resurrects() {
        let s = DirectBarStore::default();
        s.forming("binance", "BTCUSDT", "1m", dbar(60_000, 10.0));
        let (_, forming) = s.series("binance", "BTCUSDT", "1m").unwrap();
        assert_eq!(forming.map(|b| b.ts), Some(60_000), "forming lands even before any close");

        s.close("binance", "BTCUSDT", "1m", dbar(60_000, 10.5));
        let (closed, forming) = s.series("binance", "BTCUSDT", "1m").unwrap();
        assert_eq!(closed.len(), 1);
        assert!(forming.is_none(), "the close supersedes the forming snapshot of its window");

        // A late conflated snapshot of the closed window arrives after the close: dropped.
        s.forming("binance", "BTCUSDT", "1m", dbar(60_000, 10.4));
        let (_, forming) = s.series("binance", "BTCUSDT", "1m").unwrap();
        assert!(forming.is_none(), "a forming at or below the last closed ts must not repaint");

        // The next window's forming is kept, latest-wins.
        s.forming("binance", "BTCUSDT", "1m", dbar(120_000, 11.0));
        s.forming("binance", "BTCUSDT", "1m", dbar(120_000, 11.2));
        let (_, forming) = s.series("binance", "BTCUSDT", "1m").unwrap();
        assert_eq!(forming.map(|b| b.close), Some(11.2));
    }

    /// The bounded-history contract: closed bars never exceed [`DIRECT_BAR_CLOSED_CAP`], with
    /// oldest-first eviction — on the live append path and on an oversized seed alike.
    #[test]
    fn direct_store_closed_history_is_bounded_with_oldest_first_eviction() {
        let s = DirectBarStore::default();
        s.seed(
            "binance",
            "BTCUSDT",
            "1m",
            (0..DIRECT_BAR_CLOSED_CAP as i64 + 7).map(|i| dbar(i * 60_000, 10.0)).collect(),
        );
        let (closed, _) = s.series("binance", "BTCUSDT", "1m").unwrap();
        assert_eq!(closed.len(), DIRECT_BAR_CLOSED_CAP, "an oversized seed is trimmed to the cap");
        assert_eq!(closed[0].ts, 7 * 60_000, "…dropping the OLDEST bars");

        let next_ts = (DIRECT_BAR_CLOSED_CAP as i64 + 7) * 60_000;
        s.close("binance", "BTCUSDT", "1m", dbar(next_ts, 11.0));
        let (closed, _) = s.series("binance", "BTCUSDT", "1m").unwrap();
        assert_eq!(closed.len(), DIRECT_BAR_CLOSED_CAP, "a live append evicts one from the front");
        assert_eq!(closed[0].ts, 8 * 60_000);
        assert_eq!(closed.last().unwrap().ts, next_ts);
    }

    /// THE HISTORY SEAM: the backend tail seeds a series exactly ONCE — only while it holds no
    /// closed bars — live closes then append through the boundary-ts rule, and a series that
    /// already holds venue bars refuses the tail outright (a backend switch can never re-import
    /// another backend's tail under accumulated venue truth).
    #[test]
    fn direct_store_backend_tail_seeds_once_and_never_over_venue_bars() {
        let s = DirectBarStore::default();
        // A forming snapshot from the venue does NOT block the tail seed (hyperliquid's pump
        // emits forming frames from the first second, long before any close exists)…
        s.forming("hyperliquid", "BTC", "1m", dbar(180_000, 12.0));
        let tail = vec![dbar(60_000, 10.0), dbar(120_000, 11.0)];
        assert!(s.seed_backend_tail("hyperliquid", "BTC", "1m", &tail), "empty series: seeded");
        let (closed, forming) = s.series("hyperliquid", "BTC", "1m").unwrap();
        assert_eq!(closed.len(), 2);
        assert_eq!(forming.map(|b| b.ts), Some(180_000), "…and the newer forming survives it");

        // ONCE: a second offer (a later snapshot, or backend B's tail after a switch) refuses.
        assert!(!s.seed_backend_tail("hyperliquid", "BTC", "1m", &[dbar(60_000, 99.0)]));
        let (closed, _) = s.series("hyperliquid", "BTC", "1m").unwrap();
        assert_eq!(closed[0].close, 10.0, "the held history is untouched by the refused offer");

        // The boundary: the venue re-closes the tail's last window — replaced, never duplicated.
        s.close("hyperliquid", "BTC", "1m", dbar(120_000, 11.5));
        let (closed, _) = s.series("hyperliquid", "BTC", "1m").unwrap();
        assert_eq!(closed.len(), 2);
        assert_eq!(closed[1].close, 11.5);

        // A series whose venue seed already landed refuses the tail too.
        s.seed("binance", "BTCUSDT", "1m", vec![dbar(60_000, 20.0)]);
        assert!(!s.seed_backend_tail("binance", "BTCUSDT", "1m", &tail));
        // …and an empty tail seeds nothing (no phantom entry, no generation churn).
        assert!(!s.seed_backend_tail("okx", "BTC-USDT", "1m", &[]));
        assert!(s.series("okx", "BTC-USDT", "1m").is_none());
    }

    /// `generation` is the fold's dirty probe: every state-changing write bumps it, a refused
    /// write does not, and `clear` (the feed-plane teardown) both empties the store and bumps —
    /// so the fold that trusts an unchanged generation can never miss a change.
    #[test]
    fn direct_store_generation_tracks_every_state_change_and_only_those() {
        let s = DirectBarStore::default();
        let g0 = s.generation();
        s.seed("binance", "BTCUSDT", "1m", vec![dbar(60_000, 10.0)]);
        let g1 = s.generation();
        assert!(g1 > g0, "seed bumps");
        s.close("binance", "BTCUSDT", "1m", dbar(30_000, 9.0)); // stale — dropped
        assert_eq!(s.generation(), g1, "a dropped stale close changes nothing and must not bump");
        s.forming("binance", "BTCUSDT", "1m", dbar(60_000, 10.0)); // at the closed ts — dropped
        assert_eq!(s.generation(), g1, "a dropped stale forming must not bump");
        s.close("binance", "BTCUSDT", "1m", dbar(120_000, 11.0));
        let g2 = s.generation();
        assert!(g2 > g1, "a live close bumps");
        s.clear();
        assert!(s.generation() > g2, "clear bumps — the fold refolds the now-empty store");
        assert!(s.keys().is_empty());
    }
}
