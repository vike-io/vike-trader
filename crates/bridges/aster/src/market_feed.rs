//! Live Aster market data (spot + USDⓈ-M perp) → the vike-data live seam: the venue face of the
//! shared [`vike_binance::family::market_feed`].
//!
//! One STOPPABLE thread per (symbol, interval): REST kline backfill (warmup seed), then the
//! public kline WS. Data flows through the [`vike_data::LiveDataSink`] handed to [`Feeds::new`] /
//! [`Feeds::with_env`] at construction — closed bars via `close_bar` (the lossless lane), intrabar
//! forming updates + last-price ticks via `forming_bar`/`bar_close_tick` (the wait-free conflating
//! lane; a perp bars subscription MAY ALSO open the `@markPrice@1s` stream feeding the REAL-mark
//! verb `mark_tick` — but Aster ships this default OFF because its mark grammar, assumed from the
//! family charter, is UNVERIFIED against Aster's own feed: opt in with `VIKE_MARK_STREAMS_ASTER=1`
//! after its market-data smoke runs) — into the core-owned bar cache; the
//! GUI renders from `CoreSnapshot.bars`. Threads poll a
//! PER-SUBSCRIPTION stop flag on a socket read timeout, so [`Feeds::unsubscribe`]/
//! [`Feeds::shutdown`] (via `impl DataClient for Feeds`) stop+join deterministically (the
//! teardown gate) — `unsubscribe` stops exactly one `(symbol, interval)` stream, leaving every
//! other subscription on the same `Feeds` running.
//!
//! Aster's kline/depth wire grammar is Binance-verbatim (same `@kline_<interval>`/`@depth@100ms`
//! stream names, 12-element klines, U/u depth recovery), so the frame decode, the WS session loop,
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
use vike_binance::family::market_feed::{SEED_LIMIT, depth_main, feed_main, mark_main};

// Re-exported so this module's existing paths (and its tests) are unchanged.
pub use vike_binance::family::market_feed::FeedCtx;

// The `now_ms` receive-time stamp this module used to own is now
// `vike_binance::family::market_feed::now_ms` — still the same thin delegation to
// `vike_model::clock::now_ms` (the consolidation point for the `SystemTime::now()…` idiom), just
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
/// (sink/core label) — the SAME [`vike_catalog::split_perp`] split `try_spawn` uses for the perp kline
/// feed. Spot (`series_symbol == api_symbol`, `is_perp = false`) is byte-identical to the
/// pre-perp behavior.
fn trades_thread_main(
    symbol: String,
    _interval: String,
    ctx: FeedCtx,
    earliest_ids: Arc<Mutex<HashMap<String, u64>>>,
) {
    let (api_symbol, is_perp) = vike_catalog::split_perp(&symbol);
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
    /// Whether a perp `subscribe_bars` also opens the venue's `@markPrice@1s` stream — resolved
    /// once at construction via [`vike_bridge_core::mark_streams_enabled_for`]. Aster ships
    /// default-**OFF** ([`MARK_STREAM_DEFAULT_ON`]): its `markPrice` grammar is assumed from the
    /// binance-family charter but has NEVER been observed on Aster's own live feed, so the stream
    /// is opt-in (`VIKE_MARK_STREAMS_ASTER=1`) until a market-data smoke on the prod rigs confirms
    /// the wire shape. The global `VIKE_MARK_STREAMS=0` master kill still overrides. Until then a
    /// perp values off the resolver's bar-close rung, exactly as before this feature.
    mark_streams: bool,
    /// PER-SYMBOL reference-counted companion mark streams (the mark pump has no id of its own at
    /// the `DataClient` seam), so charting one perp at two intervals opens ONE mark socket and
    /// [`Feeds::unsubscribe`] stops it only when the last bars subscription releases it.
    mark_pairings: vike_bridge_core::MarkPairings<SubscriptionId>,
}

/// Aster's mark stream ships default-OFF: its `@markPrice@1s` grammar is assumed from the
/// binance-family charter but has never been observed on Aster's own venue, so pairing is opt-in
/// via `VIKE_MARK_STREAMS_ASTER=1` (see [`Feeds::mark_streams`]). Flip to `true` only after a
/// market-data smoke on the prod rigs confirms the wire shape.
const MARK_STREAM_DEFAULT_ON: bool = false;

/// Whether a `subscribe_bars` on `symbol` should ALSO open the venue's `@markPrice@1s` stream:
/// the `.P` perp tag AND the resolved mark-streams knob. Pure — the ONE place the pairing
/// predicate lives, so the spawn site and its tests read the same law. Spot has no mark price at
/// all. (The knob itself is resolved at construction: default-OFF for Aster, opt-in via
/// `VIKE_MARK_STREAMS_ASTER=1` — see [`MARK_STREAM_DEFAULT_ON`].)
fn should_pair_mark(symbol: &str, mark_streams: bool) -> bool {
    mark_streams && symbol.ends_with(".P")
}

impl Feeds {
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
            mark_streams: vike_bridge_core::mark_streams_enabled_for(
                "aster",
                MARK_STREAM_DEFAULT_ON,
            ),
            mark_pairings: Default::default(),
        }
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
        let (api_symbol, is_perp) = vike_catalog::split_perp(symbol);
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
    /// subscription `bars_id` just created for `symbol`. Aster ships default-OFF (opt in with
    /// `VIKE_MARK_STREAMS_ASTER=1`; the master kill `VIKE_MARK_STREAMS=0` still overrides), so by
    /// default this is a no-op and valuation stays on the resolver's bar-close rung. When opted in
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

#[cfg(test)]
mod tests {
    use super::{FeedCtx, Feeds};
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    use vike_bridge_core::Environment;
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
        assert_eq!(feeds.subs.len(), 1, "only the unsubscribed stream is removed");
        assert!(feeds.subs.contains_key(&id2), "the other subscription keeps running");

        feeds.shutdown();
        assert!(feeds.subs.is_empty());
    }

    #[test]
    fn unsubscribe_of_an_unknown_id_is_a_no_op() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        let id = feeds.spawn_with("BTCUSDT", "1m", fake_feed_body).expect("spawn ok");
        feeds.unsubscribe(vike_data::SubscriptionId(id.0 + 100)); // never issued
        assert_eq!(feeds.subs.len(), 1, "unknown id must not disturb the real subscription");
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
        assert!(feeds.subs.is_empty());
    }

    #[test]
    fn quotes_and_book_are_unsupported() {
        // subscribe_trades is deliberately NOT asserted here — it wires a real feed thread (real
        // WS connect + REST warmup), so exercising it from a unit test would attempt a live
        // network connection. Its pure decode/splice logic is covered by trades.rs's own tests;
        // the loop itself is covered by the crate's live-smoke conventions (see
        // `tests/aster_market_data_smoke.rs`).
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

    /// `Feeds::new` defaults to testnet (`Environment::Demo`); `with_env` takes an explicit tier —
    /// the one behavioral divergence this port adds over the binance template (which has no
    /// concept of testnet/mainnet: it's hardcoded to one host per instrument class).
    #[test]
    fn new_defaults_to_demo_env_with_env_takes_explicit_tier() {
        let sink = Arc::new(RecordingSink::default());
        let demo = Feeds::new(sink.clone(), || {});
        assert_eq!(demo.env, Environment::Demo);
        let live = Feeds::with_env(sink, || {}, Environment::Live);
        assert_eq!(live.env, Environment::Live);
    }

    /// Rung-2 guard: the kline + depth WS URLs the shared feed builds for Aster must be the
    /// `env`-resolved sapi/fapi stream hosts — depth spot-only (the futures book is
    /// `market_data.rs`'s track).
    #[test]
    fn aster_kline_and_depth_urls_are_env_resolved() {
        use vike_binance::family::market_feed::{depth_ws_url, kline_ws_url};
        let urls = crate::trades::spec(Environment::Demo).urls;
        assert_eq!(
            kline_ws_url(urls.ws(false), "BTCUSDT", "1m"),
            "wss://sstream.asterdex-testnet.com/ws/btcusdt@kline_1m"
        );
        assert_eq!(
            kline_ws_url(urls.ws(true), "BTCUSDT", "1m"),
            "wss://fstream.asterdex-testnet.com/ws/btcusdt@kline_1m"
        );
        assert_eq!(
            depth_ws_url(urls.ws(false), "BTCUSDT"),
            "wss://sstream.asterdex-testnet.com/ws/btcusdt@depth@100ms"
        );
        let live = crate::trades::spec(Environment::Live).urls;
        assert_eq!(
            kline_ws_url(live.ws(true), "BTCUSDT", "1m"),
            "wss://fstream.asterdex.com/ws/btcusdt@kline_1m"
        );
    }

    /// The pairing PREDICATE (mark-slot semantics, W2-T4): only a `.P` perp pairs a mark stream,
    /// and only while the knob is on. Spot has no venue mark price at all.
    #[test]
    fn only_perps_pair_a_mark_stream_and_only_while_the_knob_is_on() {
        assert!(super::should_pair_mark("BTCUSDT.P", true));
        assert!(!super::should_pair_mark("BTCUSDT", true), "spot has no mark price");
        assert!(!super::should_pair_mark("BTCUSDT.P", false), "VIKE_MARK_STREAMS=0 suppresses");
    }

    /// SPAWN side, network-free: `pair_mark_stream` is the production path `try_spawn` calls (only
    /// `body` differs here). A perp spawns a companion `@markPrice@1s` stream (Binance-grammar
    /// `mark_main`, reused from the family); unsubscribing the bars id stops BOTH. Aster's mark
    /// stream is default-OFF, so this OPTS IN explicitly (the production opt-in is
    /// `VIKE_MARK_STREAMS_ASTER=1`) to exercise the spawn mechanics.
    #[test]
    fn an_opted_in_perp_bars_subscription_spawns_and_then_stops_its_mark_stream() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        feeds.mark_streams = true; // opt in (Aster ships default-OFF)
        let bars_id = feeds.spawn_with("BTCUSDT.P", "1m", fake_feed_body).expect("spawn ok");
        let other_id = feeds.spawn_with("ETHUSDT", "1m", fake_feed_body).expect("spawn ok");
        feeds.pair_mark_stream(bars_id, "BTCUSDT.P", fake_feed_body);
        assert_eq!(feeds.subs.len(), 3, "bars + mark + the unrelated feed");

        feeds.unsubscribe(bars_id);
        assert_eq!(feeds.subs.len(), 1, "bars AND mark stopped together");
        assert!(feeds.subs.contains_key(&other_id), "unrelated subscriptions keep running");
        assert!(feeds.mark_pairings.is_empty(), "the pairing is consumed");
        feeds.shutdown();
    }

    #[test]
    fn a_spot_bars_subscription_spawns_no_mark_stream() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        feeds.mark_streams = true; // even opted in, spot has no mark price
        let bars_id = feeds.spawn_with("BTCUSDT", "1m", fake_feed_body).expect("spawn ok");
        feeds.pair_mark_stream(bars_id, "BTCUSDT", fake_feed_body);
        assert_eq!(feeds.subs.len(), 1, "no companion stream for spot");
        feeds.shutdown();
    }

    /// Aster ships default-OFF: a freshly constructed `Feeds` (no `VIKE_MARK_STREAMS_ASTER=1`)
    /// spawns NO mark stream even for a perp — the unverified-wire safety default, pinned. (Runs
    /// with a clean process env in CI; the pure resolver is proven in `vike_bridge_core`.)
    #[test]
    fn aster_defaults_to_no_mark_stream_until_opted_in() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        assert!(!feeds.mark_streams, "Aster's mark stream is OFF by default");
        let bars_id = feeds.spawn_with("BTCUSDT.P", "1m", fake_feed_body).expect("spawn ok");
        feeds.pair_mark_stream(bars_id, "BTCUSDT.P", fake_feed_body);
        assert_eq!(feeds.subs.len(), 1, "default-off -> no mark stream even for a perp");
        feeds.shutdown();
    }

    /// The knob off (`VIKE_MARK_STREAMS=0`, or the default-off state) suppresses the spawn even for
    /// a perp — the knob's whole job, pinned independently of the env-resolved default.
    #[test]
    fn the_mark_streams_knob_off_suppresses_the_perp_spawn() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        feeds.mark_streams = false;
        let bars_id = feeds.spawn_with("BTCUSDT.P", "1m", fake_feed_body).expect("spawn ok");
        feeds.pair_mark_stream(bars_id, "BTCUSDT.P", fake_feed_body);
        assert_eq!(feeds.subs.len(), 1, "knob off -> no mark stream even for a perp");
        feeds.shutdown();
    }

    /// Per-symbol dedupe: a 1m AND a 5m chart on the same perp share ONE mark socket (opted in).
    #[test]
    fn two_intervals_on_one_perp_share_a_single_mark_stream() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        feeds.mark_streams = true; // opt in (Aster ships default-OFF)
        let bars_1m = feeds.spawn_with("BTCUSDT.P", "1m", fake_feed_body).expect("spawn ok");
        feeds.pair_mark_stream(bars_1m, "BTCUSDT.P", fake_feed_body);
        let bars_5m = feeds.spawn_with("BTCUSDT.P", "5m", fake_feed_body).expect("spawn ok");
        feeds.pair_mark_stream(bars_5m, "BTCUSDT.P", fake_feed_body);
        assert_eq!(feeds.subs.len(), 3, "two bars feeds but only ONE mark stream");

        feeds.unsubscribe(bars_1m);
        assert_eq!(feeds.subs.len(), 2, "the mark stream the 5m chart still needs stays up");
        feeds.unsubscribe(bars_5m);
        assert!(feeds.subs.is_empty(), "the last release stops the mark stream too");
        feeds.shutdown();
    }
}
