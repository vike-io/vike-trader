//! Live Binance Spot market data → the vike-data live seam (R5c): the venue face of the shared
//! [`crate::family::market_feed`].
//!
//! One STOPPABLE thread per (symbol, interval): REST kline backfill (warmup seed), then the
//! public kline WS. Data flows through the [`vike_data::LiveDataSink`] handed to [`Feeds::new`]
//! at construction — closed bars via `close_bar` (the lossless lane), intrabar forming updates +
//! last-price ticks via `forming_bar`/`bar_close_tick` (the wait-free conflating lane; a perp
//! bars subscription ALSO opens the `@markPrice@1s` stream feeding the REAL-mark verb
//! `mark_tick`, default ON, the venue's `mark_streams` row via [`Feeds::with_mark_streams`])
//! — into the core-owned bar cache; the GUI renders from `CoreSnapshot.bars`. Threads poll a
//! PER-SUBSCRIPTION stop flag on a socket read timeout, so [`Feeds::unsubscribe`]/
//! [`Feeds::shutdown`] (via `impl DataClient for Feeds`) stop+join deterministically (the
//! teardown gate) — `unsubscribe` stops exactly one `(symbol, interval)` stream, leaving every
//! other subscription on the same `Feeds` running.
//!
//! Aster's kline/depth grammar is a verbatim fork, so the frame decode, the WS session loop, the
//! DOM depth lane and the seed-then-stream body live once in the family module and both venues call
//! in (F17, dedup rung 2). What stays HERE is the `Feeds` SHELL — the struct, its constructors, its
//! per-key `spawn_with` bookkeeping and its `DataClient` impl — because Aster's own shell diverges
//! (an `Environment` field + a `with_env` constructor Binance has no concept of) and the orphan rule
//! forbids Aster attaching constructors to a vike-binance-owned type. [`BINANCE_URLS`] below is the
//! whole per-venue delta: a `const` table, deliberately NOT env-resolved.
//!
//! **Dedup A6 (wave 3):** the per-subscription stop/join bookkeeping rides
//! [`vike_data::FeedRegistry`] and the family kline/trades WS session lifecycles ride the shared
//! [`vike_bridge_core::market_pump`] driver — behavior-identical to the pre-driver copies (bybit
//! was the wave-3 proof venue; see [`crate::family::market_feed`]'s module doc).
//!
//! Trades (`subscribe_trades`, Task B2): a separate live `@aggTrade` feed with its own
//! WS-buffer→REST-warmup→splice startup dance — implemented in [`crate::family::trades`] (its
//! module doc has the full contract); this file only adapts `FeedCtx` into that module's plain args
//! and spawns it via the same per-key `spawn_with` bookkeeping as `subscribe_bars`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use vike_data::{
    DataClient, FeedRegistry, LiveDataError, LiveDataSink, SubscriptionId, require_live_verb,
};
use vike_model::LiveVerb;

use super::data::fetch_klines_latest;
use crate::VENUE;
use crate::family::UrlTable;
use crate::family::market_feed::{SEED_LIMIT, depth_main, feed_main, mark_main};

// Re-exported so this module's existing paths (and its tests) are unchanged.
pub use crate::family::market_feed::FeedCtx;

/// Binance's mainnet public endpoints — the whole per-venue delta the shared market-data stack
/// takes as a parameter. A `const`: Binance resolves to exactly ONE set of hosts and is deliberately
/// NOT env-resolved (demo→mainnet threading is a separate, separately-gated concern). Every URL the
/// family builds for this venue is derived from these, byte-identical to the inline `format!`s they
/// replaced.
pub const BINANCE_URLS: UrlTable = UrlTable {
    spot_ws: "wss://stream.binance.com:9443",
    perp_ws: "wss://fstream.binance.com",
    spot_rest: "https://api.binance.com",
    perp_rest: "https://fapi.binance.com",
    spot_agg_trades_path: "/api/v3/aggTrades",
    // Binance serves perp aggTrades at v1 (Aster at v3) — a real venue divergence, carried as data.
    perp_agg_trades_path: "/fapi/v1/aggTrades",
    // Binance's futures `@aggTrade` WS stream is DEAD — it accepts the socket and pushes nothing
    // (measured 0 frames/60s on BTCUSDT AND ETHUSDT, while `@trade` pushed 770). Aster's works, so
    // this is per-venue. See `PerpTradesLane`.
    perp_trades_lane: crate::family::PerpTradesLane::Raw,
    perp_raw_trades_path: "/fapi/v1/trades",
};

// The `now_ms` receive-time stamp this module used to own is now
// `crate::family::market_feed::now_ms` — still the same thin delegation to
// `vike_model::now_ms` (the consolidation point for the `SystemTime::now()…` idiom), just
// single-sited alongside its only callers (the depth driver's `&now_ms` fn-pointer, `publish_book`,
// and `family::trades`' receive stamp), all of which moved there in rung 2.

/// REST warmup seed: the newest `SEED_LIMIT` klines (last one still forming) via the shared
/// [`super::data`] fetcher — the same kline REST endpoint + JSON→bar map the backfill uses.
/// `symbol` here is always the EXCHANGE (`.P`-stripped) symbol; `perp` picks the fapi vs spot host.
fn fetch_seed(symbol: &str, interval: &str, perp: bool) -> Result<Vec<vike_model::Bar>, String> {
    fetch_klines_latest(symbol, interval, SEED_LIMIT, perp)
}

/// `subscribe_trades`'s thread body (matches the `spawn_with` shape) — a thin adapter unpacking
/// `FeedCtx` into the plain trait-object/closure args [`crate::family::trades::run_trades_feed`]
/// takes. `earliest_ids` (SP3 T2) is `subscribe_trades`'s clone of `Feeds::earliest_live_ids` —
/// threaded through as its own plain param rather than folded into `FeedCtx`, since the other two
/// `FeedCtx` consumers have no use for it.
///
/// A trailing `.P` marks a USDS-M perp: it's stripped to the exchange `api_symbol` for the fstream
/// WS + fapi REST warmup, while the ORIGINAL `.P`-suffixed `symbol` stays the `series_symbol`
/// (sink/core label) — the SAME [`vike_catalog::split_perp_at`] split `try_spawn` uses for the
/// perp kline feed. Spot (`series_symbol == api_symbol`, `is_perp = false`) is byte-identical to
/// the pre-perp behavior.
fn trades_thread_main(
    symbol: String,
    _interval: String,
    ctx: FeedCtx,
    earliest_ids: Arc<Mutex<HashMap<String, u64>>>,
) {
    let (api_symbol, is_perp) = vike_catalog::split_perp_at(VENUE, &symbol);
    crate::trades::run_trades_feed(
        &crate::trades::SPEC,
        &symbol,
        api_symbol,
        is_perp,
        ctx.sink.as_ref(),
        ctx.wake.as_ref(),
        &|s| ctx.set_status(s),
        &ctx.stop,
        &earliest_ids,
    );
}

/// The DOM depth thread body (matches the `spawn_with` shape) — supplies Binance's own snapshot
/// fetch to the shared lane, which now resolves host AND path from the perp flag
/// [`crate::family::market_feed::depth_main`] derives from the symbol. A `.P` subscription
/// therefore seeds from `fapi` and streams from `fstream`; a spot one is unchanged.
fn depth_thread_main(symbol: String, interval: String, ctx: FeedCtx) {
    depth_main(symbol, interval, ctx, |sym, perp| {
        crate::market_data::fetch_depth_snapshot_for(perp, sym)
    })
}

/// All live feed threads, keyed by [`SubscriptionId`] via the shared [`FeedRegistry`] (dedup A6:
/// one stop flag + one `JoinHandle` per subscription; [`Feeds::unsubscribe`] stops+joins exactly
/// one stream, [`DataClient::shutdown`] stops+joins them all). Nothing is ever detached — the
/// deterministic-teardown rule the legacy feed violated.
pub struct Feeds {
    sink: Arc<dyn LiveDataSink>,
    pub status: Arc<Mutex<String>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    registry: FeedRegistry,
    /// Earliest live aggTrade id ever observed per symbol (SP3 T2) — see
    /// [`Feeds::earliest_live_ids`] for the full contract; every `subscribe_trades` call clones
    /// this same `Arc` into its spawned thread.
    earliest_live_ids: Arc<Mutex<HashMap<String, u64>>>,
    /// Whether a perp `subscribe_bars` also opens the venue's `@markPrice@1s` stream — the venue's
    /// `mark_streams` row (`Feeds::with_mark_streams`), else [`MARK_STREAM_DEFAULT_ON`] (mark-slot
    /// semantics, W2-T4).
    mark_streams: bool,
    /// PER-SYMBOL reference-counted companion mark streams (the mark pump has no id of its own at
    /// the `DataClient` seam), so charting one perp at two intervals opens ONE mark socket and
    /// [`Feeds::unsubscribe`] stops it only when the last bars subscription releases it.
    mark_pairings: vike_bridge_core::MarkPairings<SubscriptionId>,
}

/// This venue's mark wire is documented, so its mark stream ships ON; `venue.binance.mark_streams
/// = 0` turns it off (decision 0095).
const MARK_STREAM_DEFAULT_ON: bool = true;

/// Whether a `subscribe_bars` on `symbol` should ALSO open the venue's `@markPrice@1s` stream:
/// the `.P` perp tag AND the venue's `mark_streams` row. Pure — the ONE place the pairing predicate
/// lives, so the spawn site and its tests read the same law. Spot has no mark price at all.
///
/// ⚠ The tag was spelled as a bare `ends_with(".P")` literal here — the eight-times-repeated idiom
/// `crates/vike-catalog/src/symbol.rs`'s module doc exists to end, written out a ninth time in the
/// one crate that already imports the helper three lines away. Byte-identical: `split_perp_at` on a
/// `Naming::PerpSuffix` venue is `strip_suffix(PERP_SUFFIX).is_some()`.
fn should_pair_mark(symbol: &str, mark_streams: bool) -> bool {
    mark_streams && vike_catalog::split_perp_at(VENUE, symbol).1
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
    /// status-string / forming-bar changes; pass `|| {}` for a headless caller. Also
    /// self-constructs the empty `earliest_live_ids` map (SP3 T2) — no new param, so every
    /// existing call site stays source-compatible; see [`Feeds::earliest_live_ids`]. The
    /// registry's spawn hook is the venue's HFT affinity pin (opt-in via `VIKE_PIN_CORES`, no-op
    /// otherwise) — threaded in as a closure because `vike-data` deliberately never depends on
    /// `vike-exec`.
    pub fn new(sink: Arc<dyn LiveDataSink>, wake: impl Fn() + Send + Sync + 'static) -> Self {
        Feeds {
            sink,
            status: Arc::new(Mutex::new("connecting to Binance…".into())),
            wake: Arc::new(wake),
            registry: FeedRegistry::with_spawn_hook(|| {
                vike_exec::affinity::pin_current_thread(
                    vike_exec::affinity::Role::MarketData,
                    "binance",
                );
            }),
            earliest_live_ids: Arc::new(Mutex::new(HashMap::new())),
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

    /// The shared earliest-live-aggTrade-id-per-symbol handle (SP3 T2): each `subscribe_trades`
    /// thread records `min(existing, its startup splice's first id)` for its own symbol right
    /// after that splice (see `family::trades::run_trades_feed`'s module doc, "Earliest-live-id
    /// reporting"). The GUI shell's backfill thread (SP3 Task 3) cloned this handle once at
    /// construction and read it as the strictly-older paging boundary: backfill ids must stay
    /// `< min_live_id` (`global-constraints.md`'s no-double-count invariant). A symbol with no
    /// completed `subscribe_trades` splice yet simply has no entry in the map. ⚠ That consumer
    /// (`vike-app`'s, as this said in the present tense until 2026-09-28) went with the shell's local
    /// feed plane on 2026-09-09 (#1727); nothing outside this crate reads the map today.
    pub fn earliest_live_ids(&self) -> Arc<Mutex<HashMap<String, u64>>> {
        self.earliest_live_ids.clone()
    }

    /// Fallible spawn of the feed thread for `(symbol, interval)`: allocates a fresh
    /// [`SubscriptionId`] and a dedicated stop flag, then runs the shared `feed_main` on its own
    /// thread — an OS thread-spawn failure is returned (not panicked) so
    /// [`DataClient::subscribe_bars`] can map it to a [`LiveDataError`].
    ///
    /// A trailing `.P` on `symbol` marks a USDS-M perp (the catalog's distinct-symbol tag, matching
    /// `BINANCE:BTCUSDT.P`): it is stripped to the exchange symbol for the fapi REST seed + fstream
    /// WS URL, while the ORIGINAL `.P`-suffixed `symbol` stays the sink/core series label (so a
    /// perp's series key never collides with its spot twin). `symbol`/`interval` are still passed
    /// into `spawn_with` unchanged (its `FnOnce(String, String, FeedCtx)` bound stays as-is); the
    /// series/api/is_perp split is captured by the closure instead.
    pub fn try_spawn(&mut self, symbol: &str, interval: &str) -> std::io::Result<SubscriptionId> {
        let (api_symbol, is_perp) = vike_catalog::split_perp_at(VENUE, symbol);
        let api_symbol = api_symbol.to_string();
        let series_symbol = symbol.to_string();
        let api_for_mark = api_symbol.clone();
        let id = self.spawn_with(symbol, interval, move |_symbol, interval, ctx| {
            feed_main(series_symbol, api_symbol, interval, is_perp, ctx, |sym, iv, perp| {
                fetch_seed(sym, iv, perp)
            })
        })?;
        let series = symbol.to_string();
        self.pair_mark_stream(id, symbol, move |_s, _i, ctx| mark_main(series, api_for_mark, ctx));
        Ok(id)
    }

    /// Attach the venue's REAL mark stream (`@markPrice@1s`, mark-slot semantics — default ON,
    /// a `mark_streams = 0` row disables) to the bars subscription `bars_id` just created for
    /// `symbol`. Spawns at most ONE mark socket per symbol ([`vike_bridge_core::MarkPairings`]);
    /// a second bars subscription on the same perp only reference-counts the running one.
    /// Fail-soft: if the extra thread can't spawn, the bars feed stays live and valuation falls
    /// back to the resolver's bar-close rung, exactly the no-mark-stream behavior. Production and
    /// the pairing tests share this method — only `body` differs (tests pass a network-free stand-in).
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
    /// `feed_main` in production; tests substitute a network-free stand-in to exercise the
    /// per-key lifecycle deterministically without a real venue connection.
    fn spawn_with(
        &mut self,
        symbol: &str,
        interval: &str,
        body: impl FnOnce(String, String, FeedCtx) + Send + 'static,
    ) -> std::io::Result<SubscriptionId> {
        let (sink, status, wake) =
            (Arc::clone(&self.sink), Arc::clone(&self.status), Arc::clone(&self.wake));
        let (symbol, interval) = (symbol.to_string(), interval.to_string());
        self.registry.spawn(format!("feed-{symbol}@{interval}"), move |stop| {
            let ctx = FeedCtx { sink, status, wake, stop, spec: crate::trades::SPEC };
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
            .map_err(|e| LiveDataError::Subscribe(format!("binance {symbol}@{interval}: {e}")))
    }

    fn subscribe_quotes(&mut self, _symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        // Caps-driven refusal (binance live_data.quotes = false): stays in lockstep with the table.
        require_live_verb("binance", LiveVerb::Quotes)?;
        unreachable!("binance declares no live quotes")
    }

    /// Start a live `@aggTrade` stream for `symbol`: WS-first buffered startup, REST
    /// `aggTrades` warmup, a `handoff` splice (dedup by id), then the ordinary live loop
    /// — see [`crate::family::trades`]'s module doc for the full startup/reconnect contract. Data
    /// flows out through [`LiveDataSink::trade`]; the returned id stops+joins just this stream (same
    /// per-key bookkeeping as `subscribe_bars`/`subscribe_depth`). Also clones
    /// [`Feeds::earliest_live_ids`] into the spawned thread (SP3 T2), which records that splice's
    /// oldest id for `symbol` before emitting it.
    ///
    /// A trailing `.P` routes to the USDS-M perp tape (fstream WS + fapi REST), stripped to the
    /// exchange symbol for those endpoints while trades still emit under the original `.P` label —
    /// the same `series`/`api`/`is_perp` split `subscribe_bars` uses for the perp kline feed (see
    /// [`trades_thread_main`]). Spot (no `.P`) is byte-identical to the pre-perp behavior.
    fn subscribe_trades(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        let earliest_ids = Arc::clone(&self.earliest_live_ids);
        self.spawn_with(symbol, "trades", move |s, iv, ctx| {
            trades_thread_main(s, iv, ctx, earliest_ids)
        })
        .map_err(|e| LiveDataError::Subscribe(format!("binance {symbol} trades: {e}")))
    }

    fn subscribe_book(&mut self, _symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        // Caps-driven refusal (binance live_data.book = false); use subscribe_depth for L2 depth.
        require_live_verb("binance", LiveVerb::Book)?;
        unreachable!("binance declares no lossless book lane")
    }

    /// Start a live L2 partial-book-depth stream for `symbol` (`@depth@100ms`). Data flows out
    /// through [`LiveDataSink::l2_snapshot`]; the returned id stops+joins just this stream.
    fn subscribe_depth(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.spawn_with(symbol, "depth", depth_thread_main)
            .map_err(|e| LiveDataError::Subscribe(format!("binance {symbol} depth: {e}")))
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

    /// Raise every feed thread's stop flag and JOIN NOTHING — phase one of a teardown that spans
    /// several clients (`vike_data::DataClient::begin_shutdown`). This venue matters most for
    /// it: a recorder subscribing a family opens TWO threads per symbol (the stream plus its
    /// companion mark stream), so a stop that paid the read timeout per socket scaled with the
    /// SUBSCRIPTION, not with the profile.
    fn begin_shutdown(&mut self) {
        self.registry.raise_stops();
    }

    /// Deterministic teardown: raise every stop flag, then JOIN every feed thread (each notices
    /// within one read-timeout tick).
    fn shutdown(&mut self) {
        self.mark_pairings.clear();
        self.registry.shutdown();
    }
}

#[path = "market_feed_tests.rs"]
#[cfg(test)]
mod market_feed_tests;
