//! The live Bybit V5 [`vike_data::DataClient`] feed — klines, mark, trades and DOM depth over the
//! venue's three public sockets (spot, linear, inverse) — the Bybit twin of binance's `market_feed`
//! module, as distinct from [`crate::market_data`], the L2/tick HFT feed.
//!
//! Same shape as the Binance feed: one STOPPABLE thread per (symbol, interval) — REST warmup seed,
//! then the public `kline.<code>.<SYMBOL>` WS. Data flows through the [`vike_data::LiveDataSink`]
//! handed to [`Feeds::new`] at construction: closed bars via `close_bar` (the lossless lane),
//! intrabar forming updates + last-price ticks via `forming_bar`/`bar_close_tick` (the wait-free
//! conflating lane; a `.P` perp bars subscription ALSO opens the linear `tickers` stream feeding
//! the REAL-mark verb `mark_tick`, default ON, the venue's `mark_streams` row via
//! [`Feeds::with_mark_streams`]). Threads poll a
//! PER-SUBSCRIPTION stop flag on a socket read timeout, so
//! [`Feeds::unsubscribe`]/[`Feeds::shutdown`] (via `impl DataClient for Feeds`) stop+join
//! deterministically (the teardown gate).
//!
//! **Dedup A6 (wave 3):** the WS session LIFECYCLE (connect + subscribe replay, subscribe-ack
//! watchdog, read-timeout stop poll, stop-aware 30×100 ms reconnect backoff) now rides the shared
//! [`vike_bridge_core::market_pump`] driver, and the per-subscription stop/join bookkeeping rides
//! [`vike_data::FeedRegistry`] — behavior-identical to the pre-driver copy (bybit is the wave-3
//! proof venue; binance/okx/hyperliquid/polymarket follow in later PRs). Only the PROTOCOL stays
//! here: the pure frame classifiers ([`route_frame`]/`route_trades_frame`), the `.P` perp split +
//! the three-way host pick, the REST warmup seed, and the sink emission.
//!
//! ## ⚠ THE HOST IS A ROUTING DECISION, AND IT IS THE SAME ONE THE HISTORY PATH MAKES
//!
//! `docs/decisions/0061-an-instrument-names-its-kind.md` phase 1 PINNED a contradiction inside this
//! one crate: `crates/bridges/bybit/src/data.rs`'s `route_target` REFUSED a bare symbol the venue
//! lists under both spot and inverse, while every lane in THIS file read the same string through a
//! `bool` (`.P` or not) and reached a host by construction. Phase 4 — this change — ends it.
//! [`feed_route`] IS `data.rs`'s `range_target`, so a symbol the store refuses to write is a symbol
//! this refuses to stream, and [`ws_host`] turns the resolved `Category` into one of the venue's
//! THREE public sockets.
//!
//! Two consequences worth carrying before editing a lane here:
//!
//!   * **The socket is STRICT where the REST endpoint is lenient.** MEASURED 2026-09-16 — see
//!     [`PUBLIC_WS_INVERSE`]'s table for the probes. Every wrong-host lane is a `handler not found`
//!     reconnect loop, except DEPTH, where it is silent.
//!   * **[`depth_main`]'s route CHANGED user-visibly** (a bare symbol's DOM moves from the perp book
//!     to the spot book). Its own ⚠ carries what that was and why.
//!
//! **Closed-bar detection: the `confirm` flag.** A Bybit kline datum carries `confirm` — `true` once
//! the candle is FINAL, `false` while it is still forming. This is the exact analogue of Binance's
//! kline `x`: `confirm=true` → emit a CLOSED bar via `close_bar`; `confirm=false` → a forming bar
//! via `forming_bar`.
//!
//! Endpoint: `wss://stream.bybit.com/v5/public/spot`. Subscribe:
//! `{"op":"subscribe","args":["kline.<code>.<SYMBOL>"]}` where `<code>` is Bybit's interval code
//! ("1","60","D",…) from [`super::data::interval_code`] — the SAME map the REST kline fetcher uses.
//! A kline datum is a JSON OBJECT `{start,open,high,low,close,volume,confirm,…}` (named fields, so
//! JSON order is irrelevant); `start` is the bar-open ms.
//!
//! [`route_frame`]: klines::route_frame

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use vike_data::{
    DataClient, FeedRegistry, LiveDataError, LiveDataSink, StreamLabel, SubscriptionId,
    require_live_verb,
};
use vike_venues::LiveVerb;

use super::data::Category;

mod depth;
pub mod klines;
pub mod mark;
pub mod trades;

use depth::depth_main;
#[cfg(test)]
use depth::{fold_depth_frame, frame_ts_ms};
use klines::feed_main;
use mark::mark_main;
use trades::trades_main;

const VENUE: &str = "bybit";
const SEED_LIMIT: usize = 1000; // newest N klines for the REST warmup (Bybit's per-request cap)
/// Bybit V5 public **spot** stream (matches `data::CATEGORY = "spot"`, the kline category served).
pub const PUBLIC_WS_SPOT: &str = "wss://stream.bybit.com/v5/public/spot";
/// Bybit V5 public **linear-perp** stream (`BTCUSDT` linear — what `BybitPerpRest` trades). Same
/// `orderbook.200.<SYM>`/`kline.<code>.<SYM>` topic shapes as spot; only the stream URL differs. A
/// symbol the venue lists on `category=linear` routes its kline and depth WS here, never by a `.P`
/// suffix alone — see [`feed_route`] and [`ws_host`].
pub const PUBLIC_WS_LINEAR: &str = "wss://stream.bybit.com/v5/public/linear";
/// Bybit V5 public **inverse-perp** (coin-settled) stream.
///
/// ⚠ **This host is the reason the live feed had to change at all, and it is the half no prose in
/// this crate had measured.** `/v5/market/kline` resolves `category` LENIENTLY — MEASURED
/// 2026-09-16, `category=linear&symbol=BTCUSD` returns the inverse tape byte-identically — so the
/// REST side was quietly correct. **The socket is STRICT, in both directions**, MEASURED the same
/// day against the real hosts with the real topics this file builds:
///
/// | host | topic | answer |
/// |---|---|---|
/// | `…/public/linear` | `orderbook.200.BTCUSD`, `orderbook.50.BTCUSD`, `kline.1.BTCUSD`, `publicTrade.BTCUSD`, `tickers.BTCUSD` | `success=false`, `error:handler not found` |
/// | `…/public/inverse` | all five of the above | `type=snapshot` DATA |
/// | `…/public/inverse` | `kline.1.BTCUSDT` | `success=false`, `error:handler not found` |
///
/// So an inverse symbol on [`PUBLIC_WS_LINEAR`] is a `FrameOutcome::Fatal` and a reconnect loop on
/// the kline/trades/mark lanes — and on the DEPTH lane it is SILENT: a `success=false` ack folds to
/// `BookOp::Ignored`, the book never seeds, and the driver's idle watchdog reconnects forever with
/// no error text anywhere. That is why widening the catalog without this const would have been worse
/// than leaving the instruments unreachable.
pub const PUBLIC_WS_INVERSE: &str = "wss://stream.bybit.com/v5/public/inverse";

/// **The ONE host decision**, and the twin of `crates/bridges/bybit/src/data.rs`'s `rest_category`:
/// the same [`Category`] value picks the REST category and this socket, so a lane's warmup seed and
/// its stream can never reach different books.
///
/// Spelled as a `match` with no catch-all on purpose — a fourth category (bybit serves `option`
/// too) must fail to COMPILE here rather than default onto one of these three.
fn ws_host(book: Category) -> &'static str {
    match book {
        Category::Spot => PUBLIC_WS_SPOT,
        Category::Linear => PUBLIC_WS_LINEAR,
        Category::Inverse => PUBLIC_WS_INVERSE,
    }
}

/// Test-only reach into [`ws_host`] from a SIBLING module — see
/// `crates/bridges/bybit/src/data.rs`'s `route_for_test` for why the catalog's routing gate calls
/// the real chooser rather than restating its table.
#[cfg(test)]
pub(crate) fn ws_host_for_test(book: Category) -> &'static str {
    ws_host(book)
}

/// Split an incoming feed symbol into `(api_symbol, is_perp)`: a trailing `.P` marks a perpetual
/// (the catalog's distinct-symbol tag, matching `BYBIT:BTCUSDT.P`) and is STRIPPED to the exchange
/// symbol that drives the WS subscribe topic + REST seed, while the ORIGINAL `.P`-suffixed symbol
/// stays the sink/core series label (so a perp's series key never collides with its spot twin). An
/// owning wrapper over [`vike_catalog::split_perp_at`], which is where that split is defined for
/// every venue.
///
/// ⚠ **It answers "is this a perpetual", NOT "which book".** That was the whole defect: every lane
/// in this file read this one `bool` as a host, so `.P` MEANT linear. [`feed_route`] is the function
/// that answers the host question now, and this one survives only where a plain perp/not-perp
/// predicate is genuinely what is wanted ([`should_pair_mark`] — spot has no mark price, and that is
/// true of both derivative books).
fn perp_split(symbol: &str) -> (String, bool) {
    let (api, perp) = vike_catalog::split_perp_at(VENUE, symbol);
    (api.to_string(), perp)
}

/// **Resolve a feed symbol to `(api_symbol, category)`** — the live twin of
/// `crates/bridges/bybit/src/data.rs`'s `range_target`, and the end of the contradiction this file
/// used to PIN.
///
/// `docs/decisions/0061-an-instrument-names-its-kind.md` phase 1 recorded that *"bybit's own crate
/// reads one string two ways … Two live, opposite readings, one venue, one crate"* and said to PIN
/// it rather than fix it, because changing a live subscription's route was phase 4. This is phase 4:
/// the history path and the live path now call the same `route_target` through the same venue
/// listings, so a symbol that the store refuses to write is a symbol this refuses to stream.
///
/// ⚠ **It can REFUSE, and a caller must surface the refusal rather than fall back.** A bare symbol
/// the venue lists under both spot and inverse has no route (the collision refusal), and a listing
/// read that FAILED is not permission to guess. Every lane below turns an `Err` into the
/// subscription's status string, which is what an operator sees.
///
/// Network I/O on the first call per process only — the derivation is cached in
/// [`crate::instruments`], and a plain spot symbol never reaches it.
fn feed_route(symbol: &str) -> Result<(String, Category), String> {
    let (wire, book) = crate::data::live_route(symbol)?;
    Ok((wire.to_string(), book))
}

/// The per-subscription wiring every Bybit feed-thread body receives.
///
/// ⚠ **`pub` so a scripted test can build one**, which is the whole point of
/// [`feed_body`]: the latch this type's `status` field carried on the CI box for 42 hours was
/// invisible to every test in this crate because nothing outside the module could construct the
/// context the real closures write into. Same shape and same reason as
/// `crates/bridges/binance/src/family/market_feed.rs`'s `FeedCtx`, which has been `pub` since its
/// own journal test needed one.
///
/// [`feed_body`]: klines::feed_body
pub struct FeedCtx {
    pub sink: Arc<dyn LiveDataSink>,
    pub status: Arc<Mutex<String>>,
    pub wake: Arc<dyn Fn() + Send + Sync>,
    pub stop: Arc<AtomicBool>,
}

/// The prefix every HEALTHY status string this venue publishes starts with. Named rather than
/// spelled at each branch because [`FeedCtx::set_status`] classifies a transition's journal LEVEL
/// on it — a call site inventing a different healthy spelling would log a recovery as a warning,
/// and this is the one place to look for why. Adopted verbatim from
/// `crates/bridges/binance/src/family/market_feed.rs`'s `HEALTHY_STATUS_PREFIX`.
const HEALTHY_STATUS_PREFIX: &str = "LIVE ·";

/// The ONE healthy status text every Bybit lane publishes — kline, mark and trades alike.
///
/// ⚠ **One string per VENUE, not per lane, and that is load-bearing rather than tidy.** All three
/// lanes share ONE `Arc<Mutex<String>>` (`Feeds::spawn_with` clones the one handle into every
/// `FeedCtx`), and [`FeedCtx::set_status`] emits only on a text TRANSITION. Per-lane healthy
/// spellings would therefore turn every session boundary on either lane into a transition against
/// the OTHER lane's text — a journal line per reconnect per lane, in a file layer defaulting to
/// `trace` (the 341 GB shape root `CLAUDE.md` records). Identical text lets the dedup absorb
/// multi-lane alternation entirely. `the_three_lanes_publish_one_healthy_string` is the gate.
pub const LIVE_STATUS: &str = "LIVE · Bybit";

impl FeedCtx {
    /// Publish this thread's human-readable status AND — on a TRANSITION only — say it in the
    /// journal.
    ///
    /// ⚠ **The journal half is not a nicety here, it is what made the the CI box incident
    /// undiagnosable.** This method used to write the mutex and emit nothing at all, and the
    /// mutex is a GUI channel: the daemon that suppressed bybit's reconcile leg 2,516 times holds
    /// no reader for it, so the latched string could not be read from the box at all — the
    /// diagnosis had to reason about which of two candidate literals it must have been. Adopted
    /// wholesale from `crates/bridges/binance/src/family/market_feed.rs`'s `FeedCtx::set_status`,
    /// including its two rules: emit on a TEXT CHANGE only (these strings are produced on a
    /// reconnect loop, and a line per cycle is the 341 GB shape), and split the LEVEL on
    /// [`HEALTHY_STATUS_PREFIX`] so `journalctl -p warning` shows a feed that stopped working
    /// while a recovery lands at `info!`.
    ///
    /// It became REQUIRED with the success disclosure rather than merely desirable: the first
    /// `SessionStatus::Live` of a session now OVERWRITES a `"{key} seed error: {e}"`, and
    /// `feed_main` seeds once per FEED — nothing re-attempts it — so without this a failed REST
    /// warmup would leave no trace anywhere at all.
    pub fn set_status(&self, s: String) {
        let changed = {
            let mut cur = self.status.lock().unwrap();
            let changed = *cur != s;
            if changed {
                cur.clone_from(&s);
            }
            changed
        };
        if changed {
            if s.starts_with(HEALTHY_STATUS_PREFIX) {
                tracing::info!(target: "vike_bybit::market_feed", venue = VENUE, status = %s, "feed status");
            } else {
                tracing::warn!(target: "vike_bybit::market_feed", venue = VENUE, status = %s, "feed status");
            }
        }
        (self.wake)();
    }
}

/// All live feed threads, keyed by [`SubscriptionId`] via the shared [`FeedRegistry`] (dedup A6:
/// one stop flag + one `JoinHandle` per subscription; [`Feeds::unsubscribe`] stops+joins exactly
/// one stream, [`DataClient::shutdown`] stops+joins them all). Nothing is ever detached — the
/// deterministic-teardown rule.
pub struct Feeds {
    sink: Arc<dyn LiveDataSink>,
    pub status: Arc<Mutex<String>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    registry: FeedRegistry,
    /// Whether a perp `subscribe_bars` also opens the linear `tickers` mark stream — the venue's
    /// `mark_streams` row (`Feeds::with_mark_streams`), else [`MARK_STREAM_DEFAULT_ON`] (mark-slot
    /// semantics, W2-T4).
    mark_streams: bool,
    /// PER-SYMBOL reference-counted companion mark streams (the mark pump has no id of its own at
    /// the `DataClient` seam), so charting one perp at two intervals opens ONE mark socket and
    /// [`Feeds::unsubscribe`] stops it only when the last bars subscription releases it.
    mark_pairings: vike_bridge_core::MarkPairings<SubscriptionId>,
}

/// This venue's mark wire is documented, so its mark stream ships ON;
/// `venue.bybit.mark_streams = 0` turns it off (decision 0095).
const MARK_STREAM_DEFAULT_ON: bool = true;

/// Whether a `subscribe_bars` on `symbol` should ALSO open the linear `tickers` mark stream: the
/// `.P` perp tag AND the venue's `mark_streams` row. Pure — the ONE place the pairing predicate
/// lives, so the spawn site and its tests read the same law. Spot has no mark price at all.
fn should_pair_mark(symbol: &str, mark_streams: bool) -> bool {
    mark_streams && perp_split(symbol).1
}

impl Feeds {
    /// How many feed threads this `Feeds` currently has running — every one of which writes the
    /// ONE `status` string [`Self::status`] hands out.
    ///
    /// ⚠ **It exists so a consumer can decide whether that string is UNAMBIGUOUS evidence**, which
    /// is a question about the mount rather than about this code. `crates/vike-tradehub/src/
    /// feeds.rs`'s `LiveFeeds::recon_feed_statuses` health-gates a venue's reconcile leg on the
    /// string, and the gate can only ever SUPPRESS — so a second lane sharing it is the difference
    /// between a fault report and an ambiguous one. On 2026-09-10 that row suppressed bybit's leg
    /// for 42 hours; the daemon's mount happened to run exactly one lane here, which is the only
    /// reason the latch was diagnosable at all. A `.P` symbol (a paired mark lane) or a second
    /// interval makes it two, and neither is visible from the daemon's source.
    ///
    /// Counts SUBSCRIPTIONS, not lane kinds: a mark lane paired to a bar subscription is its own
    /// registry entry, so it counts — which is the answer the caller wants.
    pub fn status_writer_lanes(&self) -> usize {
        self.registry.len()
    }

    /// `sink` receives every seed/close/forming/mark call from every subscription this `Feeds`
    /// spawns (shared — construct once, subscribe many). `wake` is the GUI repaint nudge fired on
    /// status / forming-bar changes; pass `|| {}` headless. The registry's spawn hook is the
    /// venue's HFT affinity pin (opt-in via `VIKE_PIN_CORES`, no-op otherwise) — threaded in as a
    /// closure because `vike-data` deliberately never depends on `vike-exec`.
    pub fn new(sink: Arc<dyn LiveDataSink>, wake: impl Fn() + Send + Sync + 'static) -> Self {
        Feeds {
            sink,
            status: Arc::new(Mutex::new("connecting to Bybit…".into())),
            wake: Arc::new(wake),
            registry: FeedRegistry::with_spawn_hook(|| {
                vike_exec::affinity::pin_current_thread(
                    vike_exec::affinity::Role::MarketData,
                    "bybit",
                );
            }),
            mark_streams: MARK_STREAM_DEFAULT_ON,
            mark_pairings: Default::default(),
        }
    }

    /// Apply this venue's `mark_streams` row (decision 0095) — `stored` is the row's value as the
    /// composition root read it, `None` when there is none. See
    /// `vike_bridge_core::mark_streams_from`.
    #[must_use]
    pub fn with_mark_streams(mut self, stored: Option<&str>) -> Self {
        self.mark_streams = vike_bridge_core::mark_streams_from(stored, MARK_STREAM_DEFAULT_ON);
        self
    }

    /// Fallible spawn of the feed thread for `(symbol, interval)`: allocates a fresh
    /// [`SubscriptionId`] and a dedicated stop flag, then runs [`feed_main`] on its own thread —
    /// an OS thread-spawn failure is returned (not panicked) so [`DataClient::subscribe_bars`] can
    /// map it to a [`LiveDataError`].
    ///
    /// A trailing `.P` on `symbol` marks a PERPETUAL (the catalog's distinct-symbol tag, matching
    /// `BYBIT:BTCUSDT.P`): it is stripped to the exchange symbol for the REST seed and the kline
    /// subscribe, while the ORIGINAL `.P`-suffixed `symbol` stays the sink/core series label (so a
    /// perp's series key never collides with its spot twin). Mirrors binance's
    /// `market_feed::Feeds::try_spawn` exactly (same technique, same rationale).
    ///
    /// ⚠ **WHICH perpetual book is resolved inside the thread, not here** ([`feed_route`], called by
    /// [`feed_main`]): the first resolution per process reads the venue's instrument listings, and
    /// this method runs on the caller's — often the picker's — thread. `symbol`/`interval` are
    /// passed into `spawn_with` unchanged (its `FnOnce(String, String, FeedCtx)` bound stays as-is).
    pub fn try_spawn(&mut self, symbol: &str, interval: &str) -> std::io::Result<SubscriptionId> {
        let series_symbol = symbol.to_string();
        let id = self.spawn_with(symbol, interval, move |_symbol, interval, ctx| {
            feed_main(series_symbol, interval, ctx)
        })?;
        let series = symbol.to_string();
        self.pair_mark_stream(id, symbol, move |_s, _i, ctx| mark_main(series, ctx));
        Ok(id)
    }

    /// Attach the venue's REAL mark stream (linear `tickers`, mark-slot semantics — default ON,
    /// a `mark_streams = 0` row disables) to the bars subscription `bars_id` just created for
    /// `symbol`. Spawns at most ONE mark socket per symbol ([`vike_bridge_core::MarkPairings`]);
    /// a second bars subscription on the same perp only reference-counts the running one.
    /// Fail-soft: if the extra thread can't spawn, the bars feed stays live and valuation falls
    /// back to the resolver's bar-close rung, exactly the no-mark-stream behavior. Production and
    /// the pairing tests share this method — only `body` differs (tests pass a network-free
    /// stand-in).
    fn pair_mark_stream(
        &mut self,
        bars_id: SubscriptionId,
        symbol: &str,
        body: impl FnOnce(String, String, FeedCtx) + Send + 'static,
    ) {
        if self.mark_pairings.attach(bars_id, symbol, should_pair_mark(symbol, self.mark_streams)) {
            return; // disabled/spot, or an existing stream was reference-counted
        }
        if let Ok(mid) = self.spawn_with(symbol, "mark", body) {
            self.mark_pairings.record(bars_id, symbol, mid);
        }
    }

    /// Shared per-key spawn bookkeeping, now one [`FeedRegistry::spawn`] call (dedup A6): the
    /// registry allocates the id + stop flag and owns the join handle; this venue wrapper only
    /// assembles its own [`FeedCtx`] around the registry-issued stop flag. `body` is the shared
    /// `feed_main` in production; tests substitute a network-free stand-in to exercise the per-key
    /// lifecycle deterministically without a real venue connection.
    fn spawn_with(
        &mut self,
        symbol: &str,
        interval: &str,
        body: impl FnOnce(String, String, FeedCtx) + Send + 'static,
    ) -> std::io::Result<SubscriptionId> {
        let (sink, status, wake) =
            (Arc::clone(&self.sink), Arc::clone(&self.status), Arc::clone(&self.wake));
        let (symbol, interval) = (symbol.to_string(), interval.to_string());
        self.registry.spawn(format!("feed-bybit-{symbol}@{interval}"), move |stop| {
            let ctx = FeedCtx { sink, status, wake, stop };
            body(symbol, interval, ctx)
        })
    }
}

impl DataClient for Feeds {
    fn subscribe_bars(
        &mut self,
        symbol: &str,
        interval: &str,
    ) -> Result<SubscriptionId, LiveDataError> {
        self.try_spawn(symbol, interval)
            .map_err(|e| LiveDataError::Subscribe(format!("bybit {symbol}@{interval}: {e}")))
    }

    fn subscribe_quotes(&mut self, _symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        // Caps-driven refusal (bybit live_data.quotes = false): stays in lockstep with the table.
        require_live_verb(VENUE, LiveVerb::Quotes)?;
        unreachable!("bybit declares no live quotes")
    }

    /// Start a live `publicTrade` stream for `symbol` (executed prints, no REST seed). Data flows
    /// out through [`LiveDataSink::trade`]; the returned id stops+joins just this stream (same
    /// per-key bookkeeping as `subscribe_bars`/`subscribe_depth`). Mirrors okx's `subscribe_trades`.
    ///
    /// A trailing `.P` marks a PERPETUAL: it is stripped to the exchange symbol for the
    /// `publicTrade` subscribe topic and for the host that symbol's BOOK names, while the ORIGINAL
    /// `.P`-suffixed `symbol` stays the sink/core series label — the SAME split
    /// [`Feeds::try_spawn`] does for klines (via [`feed_route`], resolved inside the thread). A spot
    /// symbol resolves `series_symbol == api_symbol` and [`Category::Spot`], so the spot path is
    /// byte-identical to the pre-perp behavior.
    fn subscribe_trades(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        let series_symbol = symbol.to_string();
        self.spawn_with(symbol, StreamLabel::Trades.as_str(), move |_symbol, _interval, ctx| {
            trades_main(series_symbol, ctx)
        })
        .map_err(|e| LiveDataError::Subscribe(format!("bybit {symbol} trades: {e}")))
    }

    fn subscribe_book(&mut self, _symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        // Caps-driven refusal (bybit live_data.book = false); use subscribe_depth for L2 depth.
        require_live_verb(VENUE, LiveVerb::Book)?;
        unreachable!("bybit declares no lossless book lane")
    }

    /// Start a live L2 depth stream for `symbol` (`orderbook.200`; the book — spot, linear or
    /// inverse — is whichever [`feed_route`] resolves the symbol to, inside the thread). Data flows
    /// out through [`LiveDataSink::l2_snapshot`]; the returned id stops+joins just this stream.
    fn subscribe_depth(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.spawn_with(symbol, StreamLabel::Depth.as_str(), depth_main)
            .map_err(|e| LiveDataError::Subscribe(format!("bybit {symbol} depth: {e}")))
    }

    /// Stop + JOIN exactly the stream `id` names — plus its companion perp mark stream when this
    /// was the LAST bars subscription holding that symbol's mark stream; every other subscription
    /// on this `Feeds` keeps running. Unknown ids (already stopped, never issued) are a no-op.
    fn unsubscribe(&mut self, id: SubscriptionId) {
        vike_bridge_core::mark_stream::unsubscribe_with_mark(
            &mut self.mark_pairings,
            &mut self.registry,
            id,
        );
    }

    /// Deterministic teardown: raise every stop flag, then JOIN every feed thread.
    fn shutdown(&mut self) {
        self.mark_pairings.clear();
        self.registry.shutdown();
    }
}

#[path = "market_feed_tests.rs"]
#[cfg(test)]
mod market_feed_tests;

#[path = "healthy_string_pin.rs"]
#[cfg(test)]
mod healthy_string_pin;
