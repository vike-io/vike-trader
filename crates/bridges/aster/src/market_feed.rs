//! Live Aster market data (spot + USDⓈ-M perp) → the vike-data live seam: the venue face of the
//! shared [`vike_binance::family::market_feed`].
//!
//! One STOPPABLE thread per (symbol, interval): REST kline backfill (warmup seed), then the
//! public kline WS. Data flows through the [`vike_data::LiveDataSink`] handed to [`Feeds::new`] /
//! [`Feeds::with_env`] at construction — closed bars via `close_bar` (the lossless lane), intrabar
//! forming updates + last-price ticks via `forming_bar`/`bar_close_tick` (the wait-free conflating
//! lane; a perp bars subscription MAY ALSO open the `@markPrice@1s` stream feeding the REAL-mark
//! verb `mark_tick` — but Aster ships this default OFF because its mark grammar, assumed from the
//! family charter, is UNVERIFIED against Aster's own feed: opt in with a
//! `venue.aster.mark_streams = 1` row after its market-data smoke runs) — into the core-owned bar
//! cache; the GUI renders from `CoreSnapshot.bars`. Threads poll a PER-SUBSCRIPTION stop flag on a
//! socket read timeout, so [`Feeds::unsubscribe`]/
//! [`Feeds::shutdown`] (via `impl DataClient for Feeds`) stop+join deterministically (the
//! teardown gate) — `unsubscribe` stops exactly one `(symbol, interval)` stream, leaving every
//! other subscription on the same `Feeds` running.
//!
//! Aster's kline/depth wire grammar is Binance-verbatim (same `@kline_<interval>`/`@depth@100ms`
//! stream names, 12-element klines; ⚠ but the depth SEQUENCE grammar is NOT identical, and saying
//! it was cost this venue a mis-synced depth lane on BOTH planes — aster sends `pu` on spot AND
//! futures where binance sends it on futures only, measured 2026-09-10, and
//! `vike_binance::family::depth`'s `apply_depth_event` reads that off the frame rather than off a
//! venue flag), so the frame decode, the WS session loop,
//! the DOM depth lane and the seed-then-stream body — previously byte-for-byte copies — now live
//! once in the Binance-wire-grammar core and both venues call in (F17, dedup rung 2). What stays
//! HERE is the `Feeds` SHELL plus the ONE real delta: host resolution. Every URL reads `self.env`
//! (an [`Environment`]) through [`crate::urls::urls_for`], mapped into the family's `UrlTable` by
//! [`crate::trades::spec`], instead of a hardcoded `binance.com` const. The shell stays per-venue
//! precisely because of that `env` field + [`Feeds::with_env`], which the binance template has no
//! concept of and which Rust's orphan rule forbids attaching to a vike-binance-owned type.
//!
//! Trades (`subscribe_trades`): a separate live `@aggTrade` feed with its own WS-buffer→REST-
//! warmup→splice startup dance — implemented in [`vike_binance::family::trades`] (its module doc
//! has the full contract); this file only adapts `FeedCtx` into that module's plain args and spawns
//! it via the same per-key `spawn_with` bookkeeping as `subscribe_bars`.
//!
//! **Depth follows the SYMBOL's instrument class** — a `.P` subscription seeds and streams the
//! USDⓈ-M futures book, a bare one the spot book. It used to pass a hardcoded `perp = false` while
//! the shared lane built its stream name from the RAW `.P` symbol, so a perp subscription asked a
//! spot host for `btcusdt.p@depth@100ms`: a stream nothing resolves, which connects and then
//! streams nothing forever. See the family `depth_main`'s doc for the full failure shape.
//! `fetch_depth_snapshot` takes `(env, symbol, perp)` and resolves the REST host INTERNALLY rather
//! than taking a raw host string, so a caller can't pass a mismatched (host, perp) pair that
//! silently builds a nonexistent URL (review round 1 finding) — this file now forwards the lane's
//! flag instead of pinning it. `market_data.rs`'s own HFT tick track is a SEPARATE consumer and
//! still runs its own futures book (`GET /fapi/v3/depth`, per the design spec).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use vike_bridge_core::Environment;
use vike_data::{DataClient, LiveDataError, LiveDataSink, SubscriptionId, require_live_verb};
use vike_model::LiveVerb;

use super::data::fetch_klines_latest;
use crate::urls::VENUE;
use vike_binance::family::market_feed::{SEED_LIMIT, depth_main, feed_main, mark_main};

// Re-exported so this module's existing paths (and its tests) are unchanged.
pub use vike_binance::family::market_feed::FeedCtx;

// The `now_ms` receive-time stamp this module used to own is now
// `vike_binance::family::market_feed::now_ms` — still the same thin delegation to
// `vike_model::time::clock::now_ms` (the consolidation point for the `SystemTime::now()…` idiom), just
// single-sited alongside its only callers (the depth driver's `&now_ms` fn-pointer, `publish_book`,
// and `family::trades`' receive stamp), all of which moved there in rung 2.

/// REST warmup seed: the newest `SEED_LIMIT` klines (last one still forming) via the shared
/// [`super::data`] fetcher — the same kline REST endpoint + JSON→bar map the backfill uses.
/// `symbol` here is always the EXCHANGE (`.P`-stripped) symbol; `perp` picks the fapi vs sapi
/// host; `env` picks testnet vs mainnet.
fn fetch_seed(
    symbol: &str,
    interval: &str,
    perp: bool,
    env: Environment,
) -> Result<Vec<vike_model::Bar>, String> {
    fetch_klines_latest(symbol, interval, SEED_LIMIT, perp, env)
}

/// `subscribe_trades`'s thread body (matches the `spawn_with` shape) — a thin adapter unpacking
/// `FeedCtx` into the plain trait-object/closure args
/// [`vike_binance::family::trades::run_trades_feed`] takes.
///
/// A trailing `.P` marks a USDⓈ-M perp: it's stripped to the exchange `api_symbol` for the fapi
/// WS + REST warmup, while the ORIGINAL `.P`-suffixed `symbol` stays the `series_symbol`
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
        &ctx.spec,
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

/// The DOM depth thread body (matches the `spawn_with` shape) — supplies Aster's snapshot fetch to
/// the shared lane, forwarding the perp flag the lane derives from the symbol (it used to pass a
/// hardcoded `false`, so a `.P` subscription seeded the SPOT book while asking for a stream name no
/// host resolved). `market_data.rs`'s separate HFT tick track still runs its own futures book.
fn depth_thread_main(symbol: String, interval: String, ctx: FeedCtx, env: Environment) {
    depth_main(symbol, interval, ctx, move |sym, perp| {
        crate::market_data::fetch_depth_snapshot(env, sym, perp)
    })
}

/// All live feed threads, keyed by [`SubscriptionId`] — one stop flag + one [`JoinHandle`] per
/// subscription (per-key lifecycle: [`Feeds::unsubscribe`] stops+joins exactly one stream,
/// [`DataClient::shutdown`] stops+joins them all). Nothing is ever detached.
///
/// [`JoinHandle`]: std::thread::JoinHandle
pub struct Feeds {
    sink: Arc<dyn LiveDataSink>,
    pub status: Arc<Mutex<String>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    /// Testnet vs mainnet — set once at construction ([`Feeds::new`] defaults to
    /// [`Environment::Demo`]; [`Feeds::with_env`] takes it explicitly). Every host lookup a
    /// spawned subscription thread makes reads this via its [`FeedCtx`]'s `spec`.
    env: Environment,
    next_id: u64,
    subs: HashMap<SubscriptionId, (Arc<AtomicBool>, std::thread::JoinHandle<()>)>,
    /// Earliest live aggTrade id ever observed per symbol — see [`Feeds::earliest_live_ids`] for
    /// the full contract; every `subscribe_trades` call clones this same `Arc` into its spawned
    /// thread.
    earliest_live_ids: Arc<Mutex<HashMap<String, u64>>>,
    /// Whether a perp `subscribe_bars` also opens the venue's `@markPrice@1s` stream — the venue's
    /// `mark_streams` row (`Feeds::with_mark_streams`), else [`MARK_STREAM_DEFAULT_ON`]. Aster
    /// ships default-**OFF**: its `markPrice` grammar is assumed from the binance-family charter
    /// but has NEVER been observed on Aster's own live feed, so the stream is opt-in (a
    /// `venue.aster.mark_streams = 1` row) until a market-data smoke on the prod rigs confirms the
    /// wire shape. Until then a perp values off the resolver's bar-close rung, exactly as before
    /// this feature.
    mark_streams: bool,
    /// PER-SYMBOL reference-counted companion mark streams (the mark pump has no id of its own at
    /// the `DataClient` seam), so charting one perp at two intervals opens ONE mark socket and
    /// [`Feeds::unsubscribe`] stops it only when the last bars subscription releases it.
    mark_pairings: vike_bridge_core::MarkPairings<SubscriptionId>,
}

/// Aster's mark stream ships default-OFF: its `@markPrice@1s` grammar is assumed from the
/// binance-family charter but has never been observed on Aster's own venue, so pairing is opt-in
/// via a `venue.aster.mark_streams = 1` row (see [`Feeds::mark_streams`]). Flip to `true` only
/// after a market-data smoke on the prod rigs confirms the wire shape.
const MARK_STREAM_DEFAULT_ON: bool = false;

/// Whether a `subscribe_bars` on `symbol` should ALSO open the venue's `@markPrice@1s` stream:
/// the `.P` perp tag AND the resolved mark-streams knob. Pure — the ONE place the pairing
/// predicate lives, so the spawn site and its tests read the same law. Spot has no mark price at
/// all. (The knob is the venue's `mark_streams` row, default-OFF for Aster — see
/// [`MARK_STREAM_DEFAULT_ON`].)
///
/// ⚠ The tag was a bare `ends_with(".P")` literal here — the repeated idiom
/// `crates/vike-catalog/src/symbol.rs`'s module doc exists to end, and the binance twin carried the
/// identical copy. Byte-identical: `split_perp_at` on a `Naming::PerpSuffix` venue is exactly
/// `strip_suffix(PERP_SUFFIX).is_some()`.
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
    /// entry in the per-key map, so it counts — which is the answer the caller wants.
    ///
    /// ⚠ Reads `subs` rather than a `FeedRegistry`: this crate is the one binance-family sibling
    /// that was never converted to the shared registry (it keeps its own `next_id` + `subs` map),
    /// so the same question has a different spelling here. Same answer.
    pub fn status_writer_lanes(&self) -> usize {
        self.subs.len()
    }

    /// `sink` receives every seed/close/forming/mark call from every subscription this `Feeds`
    /// spawns (shared — construct once, subscribe many). `wake` is the GUI repaint nudge fired on
    /// status-string / forming-bar changes; pass `|| {}` for a headless caller. Defaults `env` to
    /// [`Environment::Demo`] (testnet) — use [`Feeds::with_env`] for an explicit tier (e.g. a
    /// credentialed mainnet mount). Also self-constructs the empty `earliest_live_ids` map — see
    /// [`Feeds::earliest_live_ids`].
    pub fn new(sink: Arc<dyn LiveDataSink>, wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self::with_env(sink, wake, Environment::Demo)
    }

    /// Same as [`Feeds::new`] but with an explicit [`Environment`] — the GUI mount constructs this
    /// with the app's configured tier so a credentialed `Live` run reaches mainnet while an
    /// uncredentialed/`Demo` run stays on testnet (mirrors `AsterExecutionClient::spawn`'s `env`
    /// threading).
    pub fn with_env(
        sink: Arc<dyn LiveDataSink>,
        wake: impl Fn() + Send + Sync + 'static,
        env: Environment,
    ) -> Self {
        Feeds {
            sink,
            status: Arc::new(Mutex::new("connecting to Aster…".into())),
            wake: Arc::new(wake),
            env,
            next_id: 0,
            subs: HashMap::new(),
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

    /// The shared earliest-live-aggTrade-id-per-symbol handle: each `subscribe_trades` thread
    /// records `min(existing, its startup splice's first id)` for its own symbol right after that
    /// splice (see `family::trades::run_trades_feed`'s module doc, "Earliest-live-id reporting"). A
    /// backfill thread (mirroring binance's usage) would clone this handle once at construction
    /// and read it as the strictly-older paging boundary. A symbol with no completed
    /// `subscribe_trades` splice yet simply has no entry in the map.
    pub fn earliest_live_ids(&self) -> Arc<Mutex<HashMap<String, u64>>> {
        self.earliest_live_ids.clone()
    }

    /// Fallible spawn of the feed thread for `(symbol, interval)`: allocates a fresh
    /// [`SubscriptionId`] and a dedicated stop flag, then runs the shared `feed_main` on its own
    /// thread — an OS thread-spawn failure is returned (not panicked) so
    /// [`DataClient::subscribe_bars`] can map it to a [`LiveDataError`].
    ///
    /// A trailing `.P` on `symbol` marks a USDⓈ-M perp (the catalog's distinct-symbol tag,
    /// matching `ASTER:BTCUSDT.P`): it is stripped to the exchange symbol for the fapi REST seed +
    /// WS URL, while the ORIGINAL `.P`-suffixed `symbol` stays the sink/core series label (so a
    /// perp's series key never collides with its spot twin).
    pub fn try_spawn(&mut self, symbol: &str, interval: &str) -> std::io::Result<SubscriptionId> {
        let (api_symbol, is_perp) = vike_catalog::split_perp_at(VENUE, symbol);
        let api_symbol = api_symbol.to_string();
        let series_symbol = symbol.to_string();
        let env = self.env;
        let api_for_mark = api_symbol.clone();
        let id = self.spawn_with(symbol, interval, move |_symbol, interval, ctx| {
            feed_main(series_symbol, api_symbol, interval, is_perp, ctx, move |sym, iv, perp| {
                fetch_seed(sym, iv, perp, env)
            })
        })?;
        let series = symbol.to_string();
        self.pair_mark_stream(id, symbol, move |_s, _i, ctx| mark_main(series, api_for_mark, ctx));
        Ok(id)
    }

    /// Attach the venue's REAL mark stream (`@markPrice@1s`, mark-slot semantics) to the bars
    /// subscription `bars_id` just created for `symbol`. Aster ships default-OFF (opt in with a
    /// `venue.aster.mark_streams = 1` row), so by default this is a no-op and valuation stays on
    /// the resolver's bar-close rung. When opted in
    /// it spawns at most ONE mark socket per symbol ([`vike_bridge_core::MarkPairings`]); a second
    /// bars subscription on the same perp only reference-counts the running one.
    ///
    /// FAIL-SOFT, two distinct shapes — Aster's mark stream is Binance-grammar by the family
    /// charter but UNVERIFIED against its own venue: (1) a frame whose shape does not match yields
    /// `Ok(None)` from the decoder → `FrameOutcome::Ignore`, i.e. SILENTLY nothing, and valuation
    /// keeps falling back to the resolver's bar-close rung — harmless but invisible; (2) a REJECTED
    /// WS upgrade (a `@markPrice@1s` path this venue does not serve) re-enters the shared
    /// reconnect/backoff loop INDEFINITELY, surfacing only as a status string — there is no
    /// operator health signal and no give-up. Neither shape can corrupt a price; both can hide a
    /// dead mark stream, so an Aster market-data smoke is the gate before relying on this feed.
    /// Production and the pairing tests share this method — only `body` differs (tests pass a
    /// network-free stand-in).
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

    /// Shared per-key spawn bookkeeping: allocate a fresh id + stop flag, run `body` on its own
    /// thread, and track the `(stop flag, JoinHandle)` pair so `unsubscribe`/`shutdown` can stop
    /// and join it later. `body` is the shared `feed_main` in production; tests substitute a
    /// network-free stand-in to exercise the per-key lifecycle deterministically without a real
    /// venue connection.
    fn spawn_with(
        &mut self,
        symbol: &str,
        interval: &str,
        body: impl FnOnce(String, String, FeedCtx) + Send + 'static,
    ) -> std::io::Result<SubscriptionId> {
        let stop = Arc::new(AtomicBool::new(false));
        let ctx = FeedCtx {
            sink: Arc::clone(&self.sink),
            status: Arc::clone(&self.status),
            wake: Arc::clone(&self.wake),
            stop: Arc::clone(&stop),
            spec: crate::trades::spec(self.env),
        };
        let (symbol, interval) = (symbol.to_string(), interval.to_string());
        let h = std::thread::Builder::new().name(format!("feed-{symbol}@{interval}")).spawn(
            move || {
                vike_exec::affinity::pin_current_thread(
                    vike_exec::affinity::Role::MarketData,
                    "aster",
                );
                body(symbol, interval, ctx)
            },
        )?;
        let id = SubscriptionId(self.next_id);
        self.next_id += 1;
        self.subs.insert(id, (stop, h));
        Ok(id)
    }
}

impl DataClient for Feeds {
    fn subscribe_bars(
        &mut self,
        symbol: &str,
        interval: &str,
    ) -> Result<SubscriptionId, LiveDataError> {
        self.try_spawn(symbol, interval)
            .map_err(|e| LiveDataError::Subscribe(format!("aster {symbol}@{interval}: {e}")))
    }

    fn subscribe_quotes(&mut self, _symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        // Caps-driven refusal (aster live_data.quotes = false): stays in lockstep with the table.
        require_live_verb("aster", LiveVerb::Quotes)?;
        unreachable!("aster declares no live quotes")
    }

    /// Start a live `@aggTrade` stream for `symbol`: WS-first buffered startup, REST
    /// `aggTrades` warmup, a `handoff` splice (dedup by id), then the ordinary live loop
    /// — see [`vike_binance::family::trades`]'s module doc for the full startup/reconnect contract.
    /// Data flows out through [`LiveDataSink::trade`]; the returned id stops+joins just this stream
    /// (same per-key bookkeeping as `subscribe_bars`/`subscribe_depth`). Also clones
    /// [`Feeds::earliest_live_ids`] into the spawned thread, which records that splice's oldest id
    /// for `symbol` before emitting it.
    ///
    /// A trailing `.P` routes to the USDⓈ-M perp tape (fapi WS + REST), stripped to the exchange
    /// symbol for those endpoints while trades still emit under the original `.P` label — the same
    /// `series`/`api`/`is_perp` split `subscribe_bars` uses for the perp kline feed. Spot (no
    /// `.P`) is byte-identical to the pre-perp behavior.
    fn subscribe_trades(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        let earliest_ids = Arc::clone(&self.earliest_live_ids);
        self.spawn_with(symbol, "trades", move |s, iv, ctx| {
            trades_thread_main(s, iv, ctx, earliest_ids)
        })
        .map_err(|e| LiveDataError::Subscribe(format!("aster {symbol} trades: {e}")))
    }

    fn subscribe_book(&mut self, _symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        // Caps-driven refusal (aster live_data.book = false); use subscribe_depth for L2 depth.
        require_live_verb("aster", LiveVerb::Book)?;
        unreachable!("aster declares no lossless book lane")
    }

    /// Start a live L2 partial-book-depth stream for `symbol` (`@depth@100ms`). Data flows out
    /// through [`LiveDataSink::l2_snapshot`]; the returned id stops+joins just this stream. See the
    /// family `depth_main`'s doc for the spot-only caveat (matches the binance template).
    fn subscribe_depth(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        let env = self.env;
        self.spawn_with(symbol, "depth", move |s, iv, ctx| depth_thread_main(s, iv, ctx, env))
            .map_err(|e| LiveDataError::Subscribe(format!("aster {symbol} depth: {e}")))
    }

    /// Stop + JOIN exactly the stream `id` names — plus its companion perp mark stream when this
    /// was the LAST bars subscription holding that symbol's mark stream; every other subscription
    /// on this `Feeds` keeps running. Unknown ids (already stopped, never issued) are a no-op.
    fn unsubscribe(&mut self, id: SubscriptionId) {
        let ids = std::iter::once(id).chain(self.mark_pairings.detach(id));
        for id in ids {
            if let Some((stop, h)) = self.subs.remove(&id) {
                stop.store(true, Ordering::Relaxed);
                let _ = h.join();
            }
        }
    }

    /// Deterministic teardown: raise every stop flag, then JOIN every feed thread (each notices
    /// within one read-timeout tick).
    fn shutdown(&mut self) {
        self.mark_pairings.clear();
        for (stop, _) in self.subs.values() {
            stop.store(true, Ordering::Relaxed);
        }
        for (_, (_, h)) in self.subs.drain() {
            let _ = h.join();
        }
    }
}

#[path = "market_feed_tests.rs"]
#[cfg(test)]
mod market_feed_tests;
