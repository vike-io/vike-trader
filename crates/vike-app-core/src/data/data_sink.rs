//! The GUI's live-data seam: [`DataClientSinkGuiFeed`], the venue-agnostic `vike_data::DataClientSink` the
//! market-data session's feeds write into, and the three GUI-readable stores it lands in —
//! [`L2BookStore`] (L2 books for the Trade windows' ladders and the Polymarket cockpit),
//! [`TickStore`] (the trade tape the tick/volume and orderflow folds drain) and
//! [`StoredBarStore`] (kline bars for the charts the snapshot does not carry). Feed threads write;
//! the GUI thread reads, lossy-latest and with no back-pressure, like the arc-swap `CoreSnapshot`.
//!
//! ⚠ There is no core behind this sink. It used to have a sibling, `CoreSinkAdapter`, that also
//! forwarded every lane into a LOCAL core's ingest channels; the desktop has no local core (it
//! observes a remote daemon), nothing constructed that adapter any more, and it was deleted. The
//! lanes only a core consumed (`mark_tick`, `bar_close_tick`, `quote`) are no-ops here.
//!
//! Every store takes its lock through `vike_utility::mutex::lock`: a feed thread that panics holding
//! one must not panic the GUI thread's next read.

use vike_utility::mutex::lock;

/// The GUI-readable live order-book store: the L2 sink lane the core has no home for. Depth
/// snapshots fold into one [`vike_marketdata::L2Book`] per `(venue, symbol)` here (feed-thread writes),
/// and each Trade window reads a cheap clone each frame (GUI-thread reads) — the same lossy-latest,
/// no-back-pressure discipline as the arc-swap `CoreSnapshot`, just for the book the core doesn't
/// carry. Keyed by `(venue, symbol)` so Trade windows on different venues hold their books side by
/// side. Each entry also carries the RECEIPT epoch-ms of its last update, so a window can flag a
/// stale (frozen / not-yet-arrived) book.
#[derive(Default)]
pub struct L2BookStore {
    books: std::sync::Mutex<
        std::collections::HashMap<(String, String), (vike_marketdata::L2Book, i64)>,
    >,
}

/// Infer the instrument tick from a depth snapshot: the smallest positive gap between adjacent
/// price levels (Binance levels sit on the true tick grid, so some adjacent pair is one tick
/// apart). Falls back to 0.01 when it can't be inferred (< 2 levels).
fn infer_tick(bids: &[vike_marketdata::L2BookLevel], asks: &[vike_marketdata::L2BookLevel]) -> f64 {
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

impl L2BookStore {
    /// Replace `symbol`'s book from a full depth snapshot. Uses the feed's authoritative
    /// `tick_size` so the GUI grid is identical to the feed's (and stable across frames); only
    /// falls back to inference if the feed didn't supply one (`<= 0`) — never for a real venue feed.
    pub fn update(
        &self,
        venue: &str,
        symbol: &str,
        tick_size: f64,
        bids: Vec<vike_marketdata::L2BookLevel>,
        asks: Vec<vike_marketdata::L2BookLevel>,
        timestamp: i64,
    ) {
        let tick = if tick_size > 0.0 { tick_size } else { infer_tick(&bids, &asks) };
        let mut book = vike_marketdata::L2Book::new(tick);
        book.apply_snapshot(0, &bids, &asks);
        lock(&self.books).insert((venue.to_string(), symbol.to_string()), (book, timestamp));
    }

    /// Store a full, already-folded [`vike_marketdata::L2Book`] directly, keyed by `(venue, symbol)` and
    /// stamped with `ts` for staleness. The `book()`-verb twin of [`Self::update`] (which rebuilds a book
    /// from raw levels): a feed that emits a WHOLE book instead of raw depth snapshots — the
    /// Polymarket market feed, whose `subscribe_book` pushes `DataClientSink::book` — lands here, so a
    /// GUI reader (the Polymarket cockpit) sees a live book exactly as a Trade window sees a crypto venue's
    /// `l2_snapshot`. The book already carries its own `tick_size`, so nothing is inferred.
    pub(crate) fn insert_book(
        &self,
        venue: &str,
        symbol: &str,
        book: vike_marketdata::L2Book,
        timestamp: i64,
    ) {
        lock(&self.books).insert((venue.to_string(), symbol.to_string()), (book, timestamp));
    }

    /// A clone of `(venue, symbol)`'s current book + its last-update epoch-ms (≈hundreds of levels
    /// — cheap), or `None` before the first snapshot arrives.
    pub fn get(&self, venue: &str, symbol: &str) -> Option<(vike_marketdata::L2Book, i64)> {
        lock(&self.books).get(&(venue.to_string(), symbol.to_string())).cloned()
    }

    /// Drop ONE key's book — the §7.3 rule-1 disclosure the datahub market-data wire needs (there
    /// is no drop-every-book verb, and this is why there must not be one).
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
        lock(&self.books).remove(&(venue.to_string(), symbol.to_string()));
    }
}

/// Cap on the number of buffered trades held per `(venue, symbol)` key in [`TickStore`]. Chosen
/// generously above any plausible one-frame drain size — this bounds worst-case memory on a
/// symbol nobody is draining (e.g. a stale/abandoned chart window), not steady-state throughput.
const TICK_STORE_CAP: usize = 50_000;

/// The GUI-readable live trade-tape buffer: like [`L2BookStore`], a lane the core has no home for.
/// Trades fold into a per-`(venue, symbol)` FIFO [`std::collections::VecDeque`] here (feed-thread
/// writes via `push`, one call per trade), capped at `TICK_STORE_CAP` with oldest-first
/// eviction so a busy feed can't grow this store unbounded when nobody is reading it. A GUI
/// consumer (tick/volume bar aggregation — see the chart-phase2 plan's Task B5) reads by calling
/// `drain`, which takes everything buffered since its last poll (FIFO order) and leaves the key
/// empty — the same lossy-latest, no-back-pressure discipline as `L2BookStore`/the arc-swap
/// `CoreSnapshot`, just take-all instead of latest-only since every trade (not just the newest)
/// matters for aggregation.
#[derive(Default)]
pub struct TickStore {
    trades: std::sync::Mutex<
        std::collections::HashMap<
            (String, String),
            std::collections::VecDeque<vike_marketdata::TickTrade>,
        >,
    >,
    /// The last [`RECENT_MS`] of every key's prints, kept for READERS THAT DO NOT DRAIN: a Trade
    /// window's tick chart paints the tape beside whatever chart window drains the same key, and a
    /// drain is take-all, so the two cannot share the FIFO above. Never drained; trimmed by age and
    /// by [`RECENT_CAP`] as prints arrive.
    recent: std::sync::Mutex<
        std::collections::HashMap<
            (String, String),
            std::collections::VecDeque<
                vike_order_entry::trade_window::tick_chart::TradeWindowTickChartPrint,
            >,
        >,
    >,
    /// Every quote-lane key's latest quote. It lives beside the prints because this is the store a
    /// Trade window already reads the stream of its instrument from (`WindowToolContextMarket::trades`), so no field
    /// has to cross the CI-excluded shell for it.
    quotes: std::sync::Mutex<std::collections::HashMap<(String, String), TickQuoteView>>,
}

/// One key's latest top of book as this desktop holds it, off the market-data wire's quote lane.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TickQuoteView {
    pub bid: f64,
    pub ask: f64,
    /// When the datahub received it, on THIS desktop's clock (§7.4: never a wire timestamp), so an age
    /// is `now - at_ms` wherever the two boxes' clocks stand.
    pub at_ms: i64,
    /// The venue's delay label on the stream, when one was given.
    pub delay: Option<vike_model::StreamDelay>,
    /// The price step the key's prices have been spelled to so far: a quote carries no grid, and a
    /// catalog row from the datahub carries none either.
    pub tick: f64,
    decimals: u32,
}

/// How many decimals `px` is spelled with, on `f64`'s shortest spelling.
fn spelled_decimals(price: f64) -> u32 {
    price.to_string().split_once('.').map_or(0, |(_, d)| d.len() as u32)
}

/// How long [`TickStore::recent`] remembers a print: the tick chart's window.
const RECENT_MS: i64 = vike_order_entry::trade_window::tick_chart::WINDOW_MS;
/// Cap on the prints [`TickStore::recent`] keeps per key — bounds a busy symbol the way
/// [`TICK_STORE_CAP`] bounds the FIFO.
const RECENT_CAP: usize = 20_000;

impl TickStore {
    /// Append one trade for `(venue, tick.symbol)`, dropping the oldest buffered trade(s) first
    /// once the per-key buffer exceeds `TICK_STORE_CAP`.
    pub fn push(&self, venue: &str, tick: &vike_marketdata::TickTrade) {
        {
            let mut recent = lock(&self.recent);
            let dq = recent.entry((venue.to_string(), tick.symbol.to_string())).or_default();
            dq.push_back(vike_order_entry::trade_window::tick_chart::TradeWindowTickChartPrint {
                ms: tick.local_timestamp,
                price: tick.price,
                size: tick.size,
                buy: !tick.is_buyer_maker,
            });
            while dq.front().is_some_and(|p| tick.local_timestamp - p.ms > RECENT_MS)
                || dq.len() > RECENT_CAP
            {
                dq.pop_front();
            }
        }
        let mut trades = lock(&self.trades);
        let dq = trades.entry((venue.to_string(), tick.symbol.to_string())).or_default();
        dq.push_back(tick.clone());
        while dq.len() > TICK_STORE_CAP {
            dq.pop_front();
        }
    }

    /// The prints of the last `RECENT_MS` on `(venue, symbol)`, oldest first, at most `newest` of
    /// them, WITHOUT taking them: the same prints stay for the next reader. Empty for a key nothing
    /// has printed on (a venue serving no trade stream among them).
    pub fn recent(
        &self,
        venue: &str,
        symbol: &str,
        newest: usize,
    ) -> Vec<vike_order_entry::trade_window::tick_chart::TradeWindowTickChartPrint> {
        let recent = lock(&self.recent);
        recent
            .get(&(venue.to_string(), symbol.to_string()))
            .map(|dq| dq.iter().skip(dq.len().saturating_sub(newest)).copied().collect())
            .unwrap_or_default()
    }

    /// Take every trade buffered for `(venue, symbol)`, oldest first, leaving the key empty for
    /// the next poll.
    pub fn drain(&self, venue: &str, symbol: &str) -> Vec<vike_marketdata::TickTrade> {
        let mut trades = lock(&self.trades);
        trades
            .get_mut(&(venue.to_string(), symbol.to_string()))
            .map(|dq| dq.drain(..).collect())
            .unwrap_or_default()
    }

    /// Replace `(venue, symbol)`'s latest quote. The step keeps the finest spelling seen, because one
    /// price of `1.0712` says nothing about a grid that also prints `1.07123`.
    pub(crate) fn set_quote(
        &self,
        venue: &str,
        symbol: &str,
        (bid, ask): (f64, f64),
        at_ms: i64,
        delay: Option<vike_model::StreamDelay>,
    ) {
        let mut quotes = lock(&self.quotes);
        let q = quotes.entry((venue.to_string(), symbol.to_string())).or_insert(TickQuoteView {
            bid,
            ask,
            at_ms,
            delay,
            tick: 1.0,
            decimals: 0,
        });
        let decimals = q.decimals.max(spelled_decimals(bid)).max(spelled_decimals(ask)).min(18);
        let tick = 1.0 / 10u64.pow(decimals) as f64;
        *q = TickQuoteView { bid, ask, at_ms, delay, tick, decimals };
    }

    /// `(venue, symbol)`'s latest quote, `None` before the first and after a gap forgets it.
    pub fn quote(&self, venue: &str, symbol: &str) -> Option<TickQuoteView> {
        lock(&self.quotes).get(&(venue.to_string(), symbol.to_string())).copied()
    }

    /// Drop `(venue, symbol)`'s quote: a price known to be cut off is worse on screen than none, the
    /// rule [`L2BookStore::remove`] states for a book.
    pub(crate) fn forget_quote(&self, venue: &str, symbol: &str) {
        lock(&self.quotes).remove(&(venue.to_string(), symbol.to_string()));
    }
}

/// Cap on the closed bars held per series in [`StoredBarStore`] — bounds worst-case memory on a
/// long-lived GUI process the way [`TICK_STORE_CAP`] does for the tape. Comfortably above every
/// venue's kline REST seed depth (the deepest, `fetch_klines_latest`-family warmups, serve well
/// under 1_500 bars), so a seed is never trimmed in practice; live closes evict oldest-first once
/// the cap is reached. Front-eviction is safe for the render path: `ChartState::sync` detects the
/// changed first-ts and takes its full-rebuild arm.
pub(crate) const STORED_BAR_CLOSED_CAP: usize = 5_000;

// ⚠ **The cap must stay ABOVE the deepest chart read the settings allow.** A chart's store read of a
// datahub that does not resample asks for `preferences.chart_history_bars` bars, up to
// `vike_config::preferences::MAX_CHART_HISTORY_BARS` (a view read is held under it in
// `crate::data::stored`' `history`);
// a read deeper than this cap would have its oldest rows trimmed on the way in, and the chart would
// show less history than the store returned. The row's ceiling is what keeps the two apart, and this
// assertion is what keeps the ceiling honest if either number is edited.
const _: () = assert!(
    (vike_config::preferences::MAX_CHART_HISTORY_BARS as usize) < STORED_BAR_CLOSED_CAP,
    "preferences.chart_history_bars may be set deeper than the GUI bar store keeps per series"
);

/// One series' bars in the [`StoredBarStore`]: the closed history (`Arc`-wrapped exactly like
/// `vike_execution::BarSeries::closed` / `TickVolAgg::closed`, so the per-frame fold can hand it to
/// `ChartState::sync` without copying) plus the latest forming snapshot.
#[derive(Default, Clone)]
struct StoredBarSeries {
    closed: std::sync::Arc<Vec<vike_marketdata::Bar>>,
    forming: Option<vike_marketdata::Bar>,
    live_tail: StoredBarLiveTail,
}

/// Whether a series is OLDER history read under the live tail the node publishes for it
/// ([`StoredBarStore::seed_under_live_tail`]), and whether that tail has been laid over it yet.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum StoredBarLiveTail {
    /// A series the node does not publish: the store's own fold renders it.
    #[default]
    None,
    /// Read up to the bar before the tail's first: the next fold joins the tail to it, whatever
    /// time lies between.
    Seeded,
    /// The tail's closes are appended to it on every fold.
    Joined,
}

/// The GUI-readable live BAR store (split-plane B2's direct-bar follow-up): the kline lane the
/// third mode's core-free sink previously dropped, now landed — [`L2BookStore`]/[`TickStore`]'s
/// sibling, fed by [`DataClientSinkGuiFeed`]'s bar lanes (feed-thread writes) and drained by
/// [`core_sync::sync_from_core`](crate::ui::core_sync::sync_from_core)'s direct-bar fold (GUI-thread
/// reads) into the charts whose render source is
/// `BackendSeriesRenderSource::StoredBars`.
///
/// **The fold rules deliberately MIRROR the core's own bar-cache arms**
/// (`crates/vike-core/src/runtime/dispatch.rs`'s `Ingest::BarSeed` / `Ingest::BarClose` arms and
/// its conflated forming drain), so a third-mode kline series holds the SAME content the fat arm's
/// core cache would hold from the identical feed lane — the venue's REST warmup seed, then live
/// closes:
///
/// - [`seed`](Self::seed) REPLACES the series (closed = the seed, forming cleared) — a re-seed
///   after a WS reconnect repairs the lossless lane exactly as `Ingest::BarSeed` does;
/// - [`close`](Self::close) appends ts-ascending, with THE BOUNDARY-TIMESTAMP DEDUP RULE: a close at
///   the last held ts REPLACES that bar idempotently (the reconnect-overlap / seed-boundary
///   case — the venue re-closing the window the seed already carried must never duplicate it),
///   a strictly older close is DROPPED (stale replay), and any close clears a forming snapshot
///   at or below its ts;
/// - [`forming`](Self::forming) is latest-wins, kept only STRICTLY ABOVE the last closed ts (a
///   late conflated snapshot of an already-closed window must not paint the same bar twice).
///
/// The one divergence from the core: closed history is bounded (`STORED_BAR_CLOSED_CAP`,
/// oldest-first eviction) because this store lives in a GUI process with no snapshot cadence to
/// bound it. (A third-mode `seed_backend_tail` history seam stood beside these until the
/// venue-fed bar plane was deleted: the snapshot's tail now paints straight from `snapshot.bars`.)
///
/// Accepted residual (same class as a reaped tape key): a series whose window closed keeps its
/// bars until a backend switch clears the store ([`Self::clear`]) — bounded by the cap, replaced
/// by the fresh seed on any re-subscribe.
#[derive(Default)]
pub struct StoredBarStore {
    series: std::sync::Mutex<std::collections::HashMap<(String, String, String), StoredBarSeries>>,
    /// Bumped on every state-changing write — the fold's cheap dirty probe
    /// ([`Self::generation`]), the direct-bar sibling of `CoreSnapshot.seq`.
    generation: std::sync::atomic::AtomicU64,
}

impl StoredBarStore {
    fn bump(&self) {
        self.generation.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Monotonic write counter: unchanged since the last read ⇒ every series is unchanged, so
    /// the per-frame fold can skip its whole pass (the `snapshot.seq` idiom, store-side).
    pub fn generation(&self) -> u64 {
        self.generation.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The venue REST-warmup seed (`DataClientSink::seed_bars`): REPLACE the series outright —
    /// closed becomes `bars` (cap-trimmed oldest-first), forming cleared for the live stream to
    /// refresh — mirroring `Ingest::BarSeed`. Also how a reconnect re-seed repairs the series.
    pub fn seed(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        mut bars: Vec<vike_marketdata::Bar>,
    ) {
        let over = bars.len().saturating_sub(STORED_BAR_CLOSED_CAP);
        if over > 0 {
            bars.drain(..over);
        }
        let mut map = lock(&self.series);
        let entry =
            map.entry((venue.to_string(), symbol.to_string(), interval.to_string())).or_default();
        entry.closed = std::sync::Arc::new(bars);
        entry.forming = None;
        entry.live_tail = StoredBarLiveTail::None;
        drop(map);
        self.bump();
    }

    /// OLDER history of a series the node PUBLISHES (`crate::data::stored`'s read when a chart is
    /// panned left of the node's tail): the bars of `older` that open before the ones held go in
    /// front of them, and the fold lays the node's tail over the result
    /// ([`Self::under_live_tail`]). At the cap the oldest go.
    pub fn seed_under_live_tail(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        older: Vec<vike_marketdata::Bar>,
    ) {
        let mut map = lock(&self.series);
        let entry =
            map.entry((venue.to_string(), symbol.to_string(), interval.to_string())).or_default();
        let held: &[vike_marketdata::Bar] =
            if entry.live_tail == StoredBarLiveTail::None { &[] } else { &entry.closed };
        let first = held.first().map(|b| b.timestamp);
        let mut bars: Vec<vike_marketdata::Bar> =
            older.into_iter().filter(|b| first.is_none_or(|f| b.timestamp < f)).collect();
        bars.extend_from_slice(held);
        // shortcut: a published series scrolls back at most STORED_BAR_CLOSED_CAP bars of its own
        // interval; raise the cap when an operator needs to scroll further back on it.
        let over = bars.len().saturating_sub(STORED_BAR_CLOSED_CAP);
        bars.drain(..over);
        if entry.live_tail != StoredBarLiveTail::Joined {
            entry.live_tail = StoredBarLiveTail::Seeded;
        }
        entry.closed = std::sync::Arc::new(bars);
        entry.forming = None;
        drop(map);
        self.bump();
    }

    /// The closed bars a series the node publishes renders from once older history was read under
    /// it ([`Self::seed_under_live_tail`]): that history with the closes of the node's `tail` newer
    /// than it appended, the oldest dropped at the cap. `None` for any other series, which renders
    /// `tail` alone.
    ///
    /// ⚠ A joined history the tail no longer reaches is DROPPED, not drawn: the tail slid past it
    /// while nothing folded (a minimized window keeps no frames), and drawing both would hide that
    /// hole in time. The chart then shows the tail alone, and panning left reads under it again.
    pub fn under_live_tail(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        tail: &[vike_marketdata::Bar],
    ) -> Option<std::sync::Arc<Vec<vike_marketdata::Bar>>> {
        let mut map = lock(&self.series);
        let entry = map.get_mut(&(venue.to_string(), symbol.to_string(), interval.to_string()))?;
        let last = entry.closed.last().map(|b| b.timestamp);
        let first = tail.first().map(|b| b.timestamp);
        match entry.live_tail {
            StoredBarLiveTail::None => return None,
            StoredBarLiveTail::Joined if first.is_some_and(|f| last.is_none_or(|l| f > l)) => {
                entry.live_tail = StoredBarLiveTail::None;
                return None;
            }
            _ if first.is_some() => entry.live_tail = StoredBarLiveTail::Joined,
            _ => {}
        }
        let from = last.map_or(0, |l| tail.partition_point(|b| b.timestamp <= l));
        if from < tail.len() {
            let closed = std::sync::Arc::make_mut(&mut entry.closed);
            closed.extend_from_slice(&tail[from..]);
            let over = closed.len().saturating_sub(STORED_BAR_CLOSED_CAP);
            closed.drain(..over);
        }
        Some(entry.closed.clone())
    }

    /// One venue-closed bar (`DataClientSink::close_bar`) — the lossless lane, folded with the
    /// boundary-ts dedup rule (see the type doc): append if newer, replace idempotently at the
    /// same ts, drop if older; then clear any forming snapshot at or below the closed ts.
    pub fn close(&self, venue: &str, symbol: &str, interval: &str, bar: vike_marketdata::Bar) {
        let mut map = lock(&self.series);
        let entry =
            map.entry((venue.to_string(), symbol.to_string(), interval.to_string())).or_default();
        let changed = match entry.closed.last().map(|b| b.timestamp) {
            // reconnect/seed-boundary overlap: the same window re-closes — replace idempotently
            Some(t) if bar.timestamp == t => {
                if let Some(last) = std::sync::Arc::make_mut(&mut entry.closed).last_mut() {
                    *last = bar.clone();
                }
                true
            }
            Some(t) if bar.timestamp < t => false, // stale replay — drop
            _ => {
                let closed = std::sync::Arc::make_mut(&mut entry.closed);
                closed.push(bar.clone());
                let over = closed.len().saturating_sub(STORED_BAR_CLOSED_CAP);
                if over > 0 {
                    closed.drain(..over);
                }
                true
            }
        };
        // a close supersedes any forming state of that (or an older) window
        if entry.forming.as_ref().is_some_and(|f| f.timestamp <= bar.timestamp) {
            entry.forming = None;
        }
        drop(map);
        if changed {
            self.bump();
        }
    }

    /// The conflating forming-bar snapshot (`DataClientSink::forming_bar`): latest-wins, kept only
    /// strictly above the last closed ts (the core's own reconnect-race guard).
    pub fn forming(&self, venue: &str, symbol: &str, interval: &str, bar: vike_marketdata::Bar) {
        let mut map = lock(&self.series);
        let entry =
            map.entry((venue.to_string(), symbol.to_string(), interval.to_string())).or_default();
        if entry.closed.last().is_none_or(|last| bar.timestamp > last.timestamp) {
            entry.forming = Some(bar);
            drop(map);
            self.bump();
        }
    }

    /// A cheap read of one series: the closed history (`Arc` clone) + the forming snapshot —
    /// the exact pair `ChartState::sync` takes. `None` before any write for the key.
    pub fn series(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
    ) -> Option<(std::sync::Arc<Vec<vike_marketdata::Bar>>, Option<vike_marketdata::Bar>)> {
        lock(&self.series)
            .get(&(venue.to_string(), symbol.to_string(), interval.to_string()))
            .map(|s| (s.closed.clone(), s.forming.clone()))
    }

    /// Every `(venue, symbol, interval)` the store holds — the fold's iteration set.
    pub fn keys(&self) -> Vec<(String, String, String)> {
        lock(&self.series).keys().cloned().collect()
    }

    /// Drop every series — [`crate::backend::connection::clear_session_state`], under the
    /// backend-store plane only (there the store is a read of THIS backend's hist store, so a
    /// backend SWITCH must not keep it).
    pub fn clear(&self) {
        lock(&self.series).clear();
        self.bump();
    }
}

/// The GUI's live-data seam (split-plane B2's core-free sink): every lane lands in a GUI-readable
/// store, and none reaches a core — there is none in this process. Constructed by
/// `crate::data::market_data_session`'s `build`, over the backend datahub's market-data feeds.
///
/// This struct is the double-fold guard made structural
/// (`split_plane::series_render_source` is the
/// decision it implements). What lands, and where — the book and tape lanes, plus the direct-bar
/// path (B2's kline follow-up), which gives the bar lanes a core-free home:
///
/// - `seed_bars` / `close_bar` / `forming_bar` → the [`StoredBarStore`] (a bar lane no market-data
///   wire feeds today: `market_data_session` keeps its copy private, and the desktop's bar store is filled by
///   [`stored`](crate::data::stored) through `seed` instead);
/// - `trade` → the [`TickStore`] tape (tick/volume + orderflow aggregation — client-direct);
/// - `l2_snapshot` / `book` → the [`L2BookStore`] (the Trade window's ladder + Polymarket cockpit —
///   client-direct);
/// - `stream_status` → the same machine-readable gap/stale/live log lines, minus the core
///   forward (no mounted strategy exists to react);
/// - `mark_tick` / `bar_close_tick` / `quote` remain consumer-less: their only consumer is the
///   core's `PriceBoard`, which does not exist here — explicit no-ops, not accidents.
pub struct DataClientSinkGuiFeed {
    /// Live order books for the Trade windows' ladders and the Polymarket cockpit, keyed by
    /// `(venue, symbol)`.
    pub books: std::sync::Arc<L2BookStore>,
    /// Live trade tape for GUI-side aggregation (tick/volume bars, orderflow).
    pub trades: std::sync::Arc<TickStore>,
    /// Live kline bars — the store
    /// [`core_sync::sync_from_core`](crate::ui::core_sync::sync_from_core)'s direct-bar fold reads.
    pub bars: std::sync::Arc<StoredBarStore>,
}

impl vike_data::DataClientSink for DataClientSinkGuiFeed {
    // ── the BAR lanes (the direct-bar path — B2's kline follow-up) ──────────────────────────
    // These used to be the double-fold guard's explicit no-ops ("the direct-bar follow-up's
    // input"). They now land in the StoredBarStore, and the guard moved WHERE it belongs: the
    // render-source decision (`split_plane::series_render_source`) assigns each kline series to
    // exactly ONE of `snapshot.bars` and this store, and `core_sync`'s two folds each consult it —
    // so a series still can never paint from both sources.
    fn seed_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        bars: Vec<vike_marketdata::Bar>,
    ) {
        self.bars.seed(venue, symbol, interval, bars);
    }

    fn close_bar(&self, venue: &str, symbol: &str, interval: &str, bar: vike_marketdata::Bar) {
        self.bars.close(venue, symbol, interval, bar);
    }

    fn forming_bar(&self, venue: &str, symbol: &str, interval: &str, bar: vike_marketdata::Bar) {
        self.bars.forming(venue, symbol, interval, bar);
    }

    // ── the DROPPED lanes (consumer-less without a core, spelled rather than inherited) ─────
    // Mark/close ticks and quotes feed the core's PriceBoard valuation slots; no core, no slot. A quote
    // the Trade window shows lands in `TickStore::set_quote` from `MarketDataSession::apply_frame`: it needs the
    // frame's age and delay label, which this verb has no room for.
    fn mark_tick(&self, _venue: &str, _symbol: &str, _price: f64, _timestamp: i64) {}

    fn quote(&self, _venue: &str, _symbol: &str, _quote: vike_marketdata::TickQuote) {}

    // ── the GUI-store lanes ─────────────────────────────────────────────────────────────────
    // The book lane lands in the store keyed by (venue, symbol) on the feed's own tick grid,
    // stamped with the receipt time for staleness.
    fn l2_snapshot(
        &self,
        venue: &str,
        symbol: &str,
        tick_size: f64,
        bids: Vec<vike_marketdata::L2BookLevel>,
        asks: Vec<vike_marketdata::L2BookLevel>,
        timestamp: i64,
    ) {
        self.books.update(venue, symbol, tick_size, bids, asks, timestamp);
    }

    fn trade(&self, venue: &str, symbol: &str, trade: vike_marketdata::TickTrade) {
        let _ = symbol; // the store keys by the tick's own symbol
        self.trades.push(venue, &trade);
    }

    fn book(&self, venue: &str, symbol: &str, book: std::sync::Arc<vike_marketdata::L2Book>) {
        // A feed that emits a WHOLE book (the Polymarket cockpit's) rather than `l2_snapshot`
        // levels. Deep-cloned out of the `Arc` because `L2BookStore` owns plain `L2Book`s the GUI
        // reads by value; the clone stays on the feed's own thread.
        self.books.insert_book(venue, symbol, (*book).clone(), vike_model::now_ms());
    }

    fn stream_status(
        &self,
        venue: &str,
        symbol: &str,
        stream: &str,
        status: vike_model::StreamStatus,
    ) {
        // Net-hardening §B: stream-health transitions as machine-readable log events (the
        // human-facing UI string is set by the feed's own `set_status`) — the feeds must not go
        // silent about gaps just because no core is mounted. `StreamStatus` is `Copy`.
        match status {
            vike_model::StreamStatus::GapStart { at_timestamp_ms: at_ts_ms } => {
                tracing::warn!(
                    venue,
                    symbol,
                    stream,
                    at_ts_ms,
                    "market-data gap started — feed idle/dropped; data missing (ticks) or stale (forming bars) until it recovers"
                );
            }
            vike_model::StreamStatus::Stale {
                newest_data_timestamp_ms: newest_data_ts_ms,
                now_ms,
            } => {
                tracing::warn!(
                    venue,
                    symbol,
                    stream,
                    newest_data_ts_ms,
                    now_ms,
                    "data-stale: transport alive but no fresh data within the freshness window"
                );
            }
            vike_model::StreamStatus::Live { gap_started_timestamp_ms: gap_started_ts_ms } => {
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
