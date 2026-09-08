//! Live Binance Spot market data → the vike-data live seam (R5c): the venue face of the shared
//! [`crate::family::market_feed`].
//!
//! One STOPPABLE thread per (symbol, interval): REST kline backfill (warmup seed), then the
//! public kline WS. Data flows through the [`vike_data::LiveDataSink`] handed to [`Feeds::new`]
//! at construction — closed bars via `close_bar` (the lossless lane), intrabar forming updates +
//! last-price ticks via `forming_bar`/`bar_close_tick` (the wait-free conflating lane; a perp
//! bars subscription ALSO opens the `@markPrice@1s` stream feeding the REAL-mark verb
//! `mark_tick`, default ON via `VIKE_MARK_STREAMS`) — into the
//! core-owned bar cache; the GUI renders from `CoreSnapshot.bars`. Threads poll a
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
// `vike_model::clock::now_ms` (the consolidation point for the `SystemTime::now()…` idiom), just
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
/// (sink/core label) — the SAME [`vike_catalog::split_perp`] split `try_spawn` uses for the perp kline
/// feed. Spot (`series_symbol == api_symbol`, `is_perp = false`) is byte-identical to the pre-perp
/// behavior.
fn trades_thread_main(
    symbol: String,
    _interval: String,
    ctx: FeedCtx,
    earliest_ids: Arc<Mutex<HashMap<String, u64>>>,
) {
    let (api_symbol, is_perp) = vike_catalog::split_perp(&symbol);
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
    /// Whether a perp `subscribe_bars` also opens the venue's `@markPrice@1s` stream — resolved
    /// once at construction from `VIKE_MARK_STREAMS` (default ON; mark-slot semantics, W2-T4).
    mark_streams: bool,
    /// PER-SYMBOL reference-counted companion mark streams (the mark pump has no id of its own at
    /// the `DataClient` seam), so charting one perp at two intervals opens ONE mark socket and
    /// [`Feeds::unsubscribe`] stops it only when the last bars subscription releases it.
    mark_pairings: vike_bridge_core::MarkPairings<SubscriptionId>,
}

/// Whether a `subscribe_bars` on `symbol` should ALSO open the venue's `@markPrice@1s` stream:
/// the `.P` perp tag AND the `VIKE_MARK_STREAMS` knob. Pure — the ONE place the pairing predicate
/// lives, so the spawn site and its tests read the same law. Spot has no mark price at all.
fn should_pair_mark(symbol: &str, mark_streams: bool) -> bool {
    mark_streams && symbol.ends_with(".P")
}

impl Feeds {
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
            mark_streams: vike_bridge_core::mark_streams_enabled(),
            mark_pairings: Default::default(),
        }
    }

    /// The shared earliest-live-aggTrade-id-per-symbol handle (SP3 T2): each `subscribe_trades`
    /// thread records `min(existing, its startup splice's first id)` for its own symbol right
    /// after that splice (see `family::trades::run_trades_feed`'s module doc, "Earliest-live-id
    /// reporting"). `vike-app`'s backfill thread (SP3 Task 3) clones this handle once at
    /// construction and reads it as the strictly-older paging boundary: backfill ids must stay
    /// `< min_live_id` (`global-constraints.md`'s no-double-count invariant). A symbol with no
    /// completed `subscribe_trades` splice yet simply has no entry in the map.
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
        let (api_symbol, is_perp) = vike_catalog::split_perp(symbol);
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
    /// `VIKE_MARK_STREAMS=0` disables) to the bars subscription `bars_id` just created for
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
        if let Some(mark_id) = self.mark_pairings.detach(id) {
            self.registry.stop_join(mark_id);
        }
        self.registry.stop_join(id);
    }

    /// Raise every feed thread's stop flag and JOIN NOTHING — phase one of a teardown that spans
    /// several clients (`vike_data::live::DataClient::begin_shutdown`). This venue matters most for
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

#[cfg(test)]
mod tests {
    use super::{FeedCtx, Feeds};
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    use vike_data::DataClient;

    // The shared capturing sink (testing-arch Phase 4d) — replaces the inline formatted-string
    // RecordingSink copy that used to live here (same canonical `calls()` line forms).
    use vike_data::RecordingSink;

    /// A network-free stand-in for the shared `feed_main`: just polls its own stop flag, same
    /// cadence as the real feed's WS loop, without ever touching a socket. Lets the per-key
    /// lifecycle (spawn/unsubscribe/shutdown) be exercised deterministically in `cargo test`.
    fn fake_feed_body(_symbol: String, _interval: String, ctx: FeedCtx) {
        while !ctx.stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn subscribe_returns_distinct_ids() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        let id1 = feeds.spawn_with("BTCUSDT", "1m", fake_feed_body).expect("spawn ok");
        let id2 = feeds.spawn_with("ETHUSDT", "1m", fake_feed_body).expect("spawn ok");
        assert_ne!(id1, id2, "distinct ids per subscribe");
        feeds.shutdown();
    }

    #[test]
    fn unsubscribe_stops_only_that_one_feed_others_keep_running() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        let id1 = feeds.spawn_with("BTCUSDT", "1m", fake_feed_body).expect("spawn ok");
        let id2 = feeds.spawn_with("ETHUSDT", "1m", fake_feed_body).expect("spawn ok");

        feeds.unsubscribe(id1);
        assert_eq!(feeds.registry.len(), 1, "only the unsubscribed stream is removed");
        assert!(feeds.registry.contains(id2), "the other subscription keeps running");

        feeds.shutdown();
        assert!(feeds.registry.is_empty());
    }

    #[test]
    fn unsubscribe_of_an_unknown_id_is_a_no_op() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        let id = feeds.spawn_with("BTCUSDT", "1m", fake_feed_body).expect("spawn ok");
        feeds.unsubscribe(vike_data::SubscriptionId(id.0 + 100)); // never issued
        assert_eq!(feeds.registry.len(), 1, "unknown id must not disturb the real subscription");
        feeds.shutdown();
    }

    #[test]
    fn shutdown_joins_every_feed() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        feeds.spawn_with("BTCUSDT", "1m", fake_feed_body).expect("spawn ok");
        feeds.spawn_with("ETHUSDT", "1m", fake_feed_body).expect("spawn ok");
        feeds.spawn_with("SOLUSDT", "1m", fake_feed_body).expect("spawn ok");
        feeds.shutdown(); // must return only once every thread has actually joined
        assert!(feeds.registry.is_empty());
    }

    #[test]
    fn quotes_and_book_are_unsupported() {
        // subscribe_trades is deliberately NOT asserted here anymore — Task B2 wired it to a real
        // feed thread (real WS connect + REST warmup), so exercising it from a unit test would
        // attempt a live network connection. Its pure decode/splice logic is covered by
        // trades.rs's own tests; the loop itself is covered by the venue's live-smoke
        // conventions (see the crate's `tests/binance_market_data_smoke.rs` for the pattern).
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        assert!(matches!(
            feeds.subscribe_quotes("BTCUSDT"),
            Err(vike_data::LiveDataError::Unsupported(_))
        ));
        assert!(matches!(
            feeds.subscribe_book("BTCUSDT"),
            Err(vike_data::LiveDataError::Unsupported(_))
        ));
    }

    /// Rung-2 guard: the kline + depth WS URLs the shared feed builds for Binance must be exactly
    /// the hosts this venue used before the `UrlTable` extraction — spot on `stream.binance.com`,
    /// perp on `fstream.binance.com`, depth spot-only.
    #[test]
    fn binance_kline_and_depth_urls_are_unchanged() {
        use crate::family::market_feed::{depth_ws_url, kline_ws_url};
        let urls = super::BINANCE_URLS;
        assert_eq!(
            kline_ws_url(urls.ws(false), "BTCUSDT", "1m"),
            "wss://stream.binance.com:9443/ws/btcusdt@kline_1m"
        );
        assert_eq!(
            kline_ws_url(urls.ws(true), "BTCUSDT", "1m"),
            "wss://fstream.binance.com/ws/btcusdt@kline_1m"
        );
        assert_eq!(
            depth_ws_url(urls.ws(false), "BTCUSDT"),
            "wss://stream.binance.com:9443/ws/btcusdt@depth@100ms"
        );
    }

    /// **The depth URL a `.P` symbol produces** — the bug this test exists to prevent from
    /// returning. `depth_main` splits the core symbol and picks the host from the flag, so a perp
    /// reaches the FUTURES stream with the suffix stripped. Before, both halves were wrong at once:
    /// spot host + raw symbol = `stream.binance.com:9443/ws/btcusdt.p@depth@100ms`, a stream no
    /// venue resolves, which connects and then silently streams nothing.
    #[test]
    fn a_perp_depth_subscription_reaches_the_futures_stream() {
        use crate::family::market_feed::depth_ws_url;
        let urls = super::BINANCE_URLS;
        let (sym, is_perp) = vike_catalog::split_perp("BTCUSDT.P");
        assert!(is_perp);
        assert_eq!(
            depth_ws_url(urls.ws(is_perp), sym),
            "wss://fstream.binance.com/ws/btcusdt@depth@100ms",
            "measured live: this stream pushes ~139 frames/15s, the broken one pushed 0"
        );
        // …and a spot symbol is byte-identical to before.
        let (sym, is_perp) = vike_catalog::split_perp("BTCUSDT");
        assert!(!is_perp);
        assert_eq!(
            depth_ws_url(urls.ws(is_perp), sym),
            "wss://stream.binance.com:9443/ws/btcusdt@depth@100ms"
        );
    }

    /// The REST seed must follow the WS host: a perp seed hits `fapi` + the `v1` futures path.
    /// Pairing a spot host with a futures path (or the reverse) 404s, and the DOM lane treats a
    /// failed seed as transient and retries it forever — which is exactly how the live bug hid.
    #[test]
    fn the_depth_seed_url_follows_the_instrument_class() {
        use crate::market_data::{MAINNET_REST, PERP_REST, depth_snapshot_url_for};
        assert_eq!(
            depth_snapshot_url_for(true, "BTCUSDT"),
            format!("{PERP_REST}/fapi/v1/depth?symbol=BTCUSDT&limit=1000")
        );
        assert_eq!(
            depth_snapshot_url_for(false, "BTCUSDT"),
            format!("{MAINNET_REST}/api/v3/depth?symbol=BTCUSDT&limit=1000")
        );
    }

    /// The pairing PREDICATE (mark-slot semantics, W2-T4): only a `.P` perp pairs a mark stream,
    /// and only while the knob is on. Spot has no venue mark price at all.
    #[test]
    fn only_perps_pair_a_mark_stream_and_only_while_the_knob_is_on() {
        assert!(super::should_pair_mark("BTCUSDT.P", true));
        assert!(!super::should_pair_mark("BTCUSDT", true), "spot has no mark price");
        assert!(!super::should_pair_mark("BTCUSDT.P", false), "VIKE_MARK_STREAMS=0 suppresses");
        assert!(!super::should_pair_mark("BTCUSDT", false));
    }

    /// SPAWN side, network-free: `pair_mark_stream` is the production path `try_spawn` calls (only
    /// `body` differs here). A perp spawns a second, companion stream; unsubscribing the bars id
    /// stops BOTH, while unrelated subscriptions keep running.
    #[test]
    fn a_perp_bars_subscription_spawns_and_then_stops_its_mark_stream() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        let bars_id = feeds.spawn_with("BTCUSDT.P", "1m", fake_feed_body).expect("spawn ok");
        let other_id = feeds.spawn_with("ETHUSDT", "1m", fake_feed_body).expect("spawn ok");
        feeds.pair_mark_stream(bars_id, "BTCUSDT.P", fake_feed_body);
        assert_eq!(feeds.registry.len(), 3, "bars + mark + the unrelated feed");
        assert!(feeds.mark_pairings.mark_id_of("BTCUSDT.P").is_some());

        feeds.unsubscribe(bars_id);
        assert_eq!(feeds.registry.len(), 1, "bars AND mark stopped together");
        assert!(feeds.registry.contains(other_id), "unrelated subscriptions keep running");
        assert!(feeds.mark_pairings.is_empty(), "the pairing is consumed");
        feeds.shutdown();
    }

    /// A SPOT bars subscription pairs nothing — the mark spawn branch never runs.
    #[test]
    fn a_spot_bars_subscription_spawns_no_mark_stream() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        let bars_id = feeds.spawn_with("BTCUSDT", "1m", fake_feed_body).expect("spawn ok");
        feeds.pair_mark_stream(bars_id, "BTCUSDT", fake_feed_body);
        assert_eq!(feeds.registry.len(), 1, "no companion stream for spot");
        assert!(feeds.mark_pairings.is_empty());
        feeds.shutdown();
    }

    /// `VIKE_MARK_STREAMS=0` (resolved into `Feeds.mark_streams` at construction) suppresses the
    /// spawn even for a perp — the knob's whole job, pinned. Set on the struct rather than through
    /// the process env so the test stays independent of test-harness parallelism.
    #[test]
    fn the_mark_streams_knob_off_suppresses_the_perp_spawn() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        feeds.mark_streams = false;
        let bars_id = feeds.spawn_with("BTCUSDT.P", "1m", fake_feed_body).expect("spawn ok");
        feeds.pair_mark_stream(bars_id, "BTCUSDT.P", fake_feed_body);
        assert_eq!(feeds.registry.len(), 1, "knob off -> no mark stream even for a perp");
        assert!(feeds.mark_pairings.is_empty());
        feeds.shutdown();
    }

    /// Per-symbol dedupe: a 1m AND a 5m chart on the same perp share ONE mark socket, released
    /// only when the last bars subscription goes.
    #[test]
    fn two_intervals_on_one_perp_share_a_single_mark_stream() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        let bars_1m = feeds.spawn_with("BTCUSDT.P", "1m", fake_feed_body).expect("spawn ok");
        feeds.pair_mark_stream(bars_1m, "BTCUSDT.P", fake_feed_body);
        let bars_5m = feeds.spawn_with("BTCUSDT.P", "5m", fake_feed_body).expect("spawn ok");
        feeds.pair_mark_stream(bars_5m, "BTCUSDT.P", fake_feed_body);
        assert_eq!(feeds.registry.len(), 3, "two bars feeds but only ONE mark stream");

        feeds.unsubscribe(bars_1m);
        assert_eq!(feeds.registry.len(), 2, "the mark stream the 5m chart still needs stays up");
        feeds.unsubscribe(bars_5m);
        assert!(feeds.registry.is_empty(), "the last release stops the mark stream too");
        feeds.shutdown();
    }
}
