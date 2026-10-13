//! Live OKX V5 kline (candlestick) market data → the vike-data live seam — the OKX twin of
//! binance's `market_feed` module (the kline feed implementing [`vike_data::DataClient`], as
//! distinct from [`crate::market_data`], the L2/tick HFT feed).
//!
//! One file per lane beside this root: `klines` (candles + REST warmup seed), `mark` (the mark-price
//! channel), `trades` and `depth` (the 400-level DOM book, on its own driver). This root keeps
//! [`Feeds`], the shared `FeedCtx` and the subscribe-outcome envelope every lane classifies first.
//!
//! Same shape as the Binance feed: one STOPPABLE thread per (symbol, interval) — REST warmup seed,
//! then the public `candle<bar>` WS. Data flows through the [`vike_data::LiveDataSink`] handed to
//! [`Feeds::new`] at construction: closed bars via `close_bar` (the lossless lane), intrabar
//! forming updates + last-price ticks via `forming_bar`/`bar_close_tick` (the wait-free conflating
//! lane; a `*-SWAP` bars subscription ALSO opens the `mark-price` channel feeding the REAL-mark
//! verb `mark_tick`, default ON, the venue's `mark_streams` row via [`Feeds::with_mark_streams`]).
//! Threads poll a PER-SUBSCRIPTION stop flag on a socket read timeout, so
//! [`Feeds::unsubscribe`]/[`Feeds::shutdown`] (via `impl DataClient for Feeds`) join
//! deterministically.
//!
//! **Dedup A6 (wave 3):** the WS session LIFECYCLE (connect + subscribe replay, subscribe-ack
//! watchdog, read-timeout stop poll, stop-aware 30×100 ms reconnect backoff) now rides the shared
//! [`vike_bridge_core::market_pump`] driver, and the per-subscription stop/join bookkeeping rides
//! [`vike_data::FeedRegistry`] — behavior-identical to the pre-driver copy (bybit was the wave-3
//! proof venue; okx is a follow-up conversion). Only the PROTOCOL stays here: the pure frame
//! classifiers ([`route_frame`]/`route_trades_frame`), the `candle<bar>`/`trades` subscribe
//! frames, the REST warmup seed, and the sink emission. The kline/trades channels send NO
//! app-level keepalive (`Keepalive` stays `None` — OKX relies on push frequency here, exactly as
//! before; only the depth feed pings, via its own `run_depth_feed` driver).
//!
//! **Closed-bar detection: the `confirm` flag (authoritative — not ts-rollover).** An OKX WS candle
//! datum is the array `[ts, o, h, l, c, vol, volCcy, volCcyQuote, confirm]` — the SAME shape as the
//! REST `history-candles` row — where `confirm` is `"1"` once the candle is FINAL and `"0"` while it
//! is forming. OKX pushes repeated updates to the in-progress candle (so a naive reader sees the same
//! `ts` many times, then a new `ts` on rollover). Rather than *infer* a close from a `ts` rollover
//! (which would emit the previous bar only when the NEXT one starts, and misclassify the current bar
//! until then), this feed reads OKX's explicit `confirm` marker — present in the candle channel and
//! the exact analogue of Binance's kline `x` / Bybit's `confirm`. `confirm=="1"` → CLOSED bar via
//! `close_bar`; else a forming bar via `forming_bar`.
//!
//! Endpoint: `wss://ws.okx.com:8443/ws/v5/public`. Subscribe:
//! `{"op":"subscribe","args":[{"channel":"candle<bar>","instId":"<SYMBOL>"}]}` where `<bar>` is OKX's
//! bar code ("1m","1H","1D",…) from [`super::data::bar_code`] — the SAME map the REST candle fetcher
//! uses. OKX has no server WS-protocol ping on this channel and relies on candle-push frequency for
//! keepalive; a dropped connection self-heals via the reconnect loop (consistent with the OKX L2
//! feed).
//!
//! [`route_frame`]: klines::route_frame

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use vike_bridge_core::feed_frame::FeedFrame;
use vike_data::{
    DataClient, FeedRegistry, LiveDataError, LiveDataSink, StreamLabel, SubscriptionId,
    require_live_verb,
};
use vike_venues::LiveVerb;

mod depth;
pub mod klines;
pub mod mark;
pub mod trades;

use depth::depth_main;
use klines::feed_main;
use mark::mark_main;
use trades::trades_main;

const VENUE: &str = "okx";

/// The subscribe OUTCOME envelope every lane on this socket shares (net-hardening br7), classified
/// BEFORE a lane's own decode: a frame carrying an `event` is never data. `event:"error"` is an
/// attributable [`FeedFrame::Error`] carrying OKX's `code`+`msg` (it was silently dropped),
/// `event:"subscribe"` is [`FeedFrame::Ack`], any other `event` (e.g. `"unsubscribe"` — not
/// attributable, not a confirm) is [`FeedFrame::Ignored`]. `None`: no `event`, so the lane decodes
/// the frame as data. An OKX subscribe ack also carries `arg.channel` but no `data`, which is why it
/// is caught here rather than left to fall through to a lane's "no data → Ignored" arm.
fn subscribe_envelope<T>(v: &Value) -> Option<FeedFrame<T>> {
    Some(match v.get("event").and_then(Value::as_str)? {
        "error" => {
            let code = v.get("code").and_then(Value::as_str).unwrap_or("?");
            let msg = v.get("msg").and_then(Value::as_str).unwrap_or("(no message)");
            FeedFrame::Error(format!("OKX subscribe error {code}: {msg}"))
        }
        "subscribe" => FeedFrame::Ack,
        _ => FeedFrame::Ignored,
    })
}

struct FeedCtx {
    sink: Arc<dyn LiveDataSink>,
    status: Arc<Mutex<String>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    stop: Arc<AtomicBool>,
}

/// The ONE healthy status text every OKX lane publishes — candle, mark and trades alike.
/// One string per VENUE, not per lane: all three lanes share ONE `Arc<Mutex<String>>`, so per-lane
/// spellings would make every session boundary on one lane a text change against the other's. The
/// bybit twin (`crates/bridges/bybit/src/market_feed.rs`'s `LIVE_STATUS`) carries the full
/// argument and the journal-volume hazard it avoids.
///
/// ⚠ **Consequence of moving `trades_main`'s write into the `SessionStatus::Live` arm:** on a mount
/// that subscribes trades and NOT candles, the shared string reads [`Feeds::new`]'s
/// `"connecting to OKX…"` until the first confirmed frame, where it used to claim LIVE before a
/// socket existed. `parse_feed_status` reads that as `Connecting`, which
/// `vike_tradehub::reconcile_config`'s `health_from_feed_status` maps to `Healthy` — the SAFE direction
/// for a gate that can only suppress. `feed_main` keeps its seed-time write, so the daemon's actual
/// mount (bars) is unaffected.
const LIVE_STATUS: &str = "LIVE · OKX";

impl FeedCtx {
    fn set_status(&self, s: String) {
        *self.status.lock().unwrap() = s;
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
    /// Whether a SWAP `subscribe_bars` also opens the `mark-price` stream — the venue's
    /// `mark_streams` row (`Feeds::with_mark_streams`), else [`MARK_STREAM_DEFAULT_ON`] (mark-slot
    /// semantics, W2-T4).
    mark_streams: bool,
    /// PER-SYMBOL reference-counted companion mark streams (the mark pump has no id of its own at
    /// the `DataClient` seam), so charting one SWAP at two intervals opens ONE mark socket and
    /// [`Feeds::unsubscribe`] stops it only when the last bars subscription releases it.
    mark_pairings: vike_bridge_core::MarkPairings<SubscriptionId>,
}

/// This venue's mark wire is documented, so its mark stream ships ON; `venue.okx.mark_streams = 0`
/// turns it off (decision 0095).
const MARK_STREAM_DEFAULT_ON: bool = true;

/// Whether a `subscribe_bars` on `inst` should ALSO open the `mark-price` stream: a `*-SWAP`
/// perpetual AND the venue's `mark_streams` row. Pure — the ONE place the pairing predicate lives,
/// so the spawn site and its tests read the same law.
///
/// SCOPE, deliberately narrow: OKX also publishes `mark-price` for DATED FUTURES
/// (`BTC-USD-240329`) and for OPTIONS, and the decoder would handle those rows unchanged — the
/// channel shape is identical. They are excluded because `*-SWAP` is the only contract class the
/// rest of this bridge charts and values today; a dated-future or option position therefore keeps
/// valuing through the resolver's bar-close rung. Widening this predicate needs no decoder change,
/// only an instId classifier that does not mistake a SPOT pair (`BTC-USDT`, which has NO mark
/// price) for a dated contract.
fn should_pair_mark(inst: &str, mark_streams: bool) -> bool {
    mark_streams && inst.ends_with("-SWAP")
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
            status: Arc::new(Mutex::new("connecting to OKX…".into())),
            wake: Arc::new(wake),
            registry: FeedRegistry::with_spawn_hook(|| {
                vike_exec::affinity::pin_current_thread(
                    vike_exec::affinity::Role::MarketData,
                    "okx",
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

    /// Fallible spawn of the feed thread for `(inst, interval)`: allocates a fresh
    /// [`SubscriptionId`] and a dedicated stop flag, then runs [`feed_main`] on its own thread —
    /// an OS thread-spawn failure is returned (not panicked) so [`DataClient::subscribe_bars`] can
    /// map it to a [`LiveDataError`].
    ///
    /// A `*-SWAP` inst (an OKX perpetual) also opens the venue's REAL `mark-price` stream — see
    /// [`should_pair_mark`] for the predicate and its deliberate dated-futures exclusion.
    pub fn try_spawn(&mut self, inst: &str, interval: &str) -> std::io::Result<SubscriptionId> {
        let id = self.spawn_with(inst, interval, feed_main)?;
        self.pair_mark_stream(id, inst, mark_main);
        Ok(id)
    }

    /// Attach the venue's REAL `mark-price` stream (mark-slot semantics — default ON, a
    /// `mark_streams = 0` row disables) to the bars subscription `bars_id` just created for `inst`.
    /// Spawns at most ONE mark socket per inst ([`vike_bridge_core::MarkPairings`]); a second bars
    /// subscription on the same SWAP only reference-counts the running one. Fail-soft: if the
    /// extra thread can't spawn, the bars feed stays live and valuation falls back to the
    /// resolver's bar-close rung, exactly the no-mark-stream behavior. Production and the pairing
    /// tests share this method — only `body` differs (tests pass a network-free stand-in).
    fn pair_mark_stream(
        &mut self,
        bars_id: SubscriptionId,
        inst: &str,
        body: impl FnOnce(String, String, FeedCtx) + Send + 'static,
    ) {
        if self.mark_pairings.attach(bars_id, inst, should_pair_mark(inst, self.mark_streams)) {
            return; // disabled/non-SWAP, or an existing stream was reference-counted
        }
        if let Ok(mid) = self.spawn_with(inst, "mark", body) {
            self.mark_pairings.record(bars_id, inst, mid);
        }
    }

    /// Shared per-key spawn bookkeeping, now one [`FeedRegistry::spawn`] call (dedup A6): the
    /// registry allocates the id + stop flag and owns the join handle; this venue wrapper only
    /// assembles its own [`FeedCtx`] around the registry-issued stop flag. `body` is the shared
    /// `feed_main` in production; tests substitute a network-free stand-in to exercise the per-key
    /// lifecycle deterministically without a real venue connection.
    fn spawn_with(
        &mut self,
        inst: &str,
        interval: &str,
        body: impl FnOnce(String, String, FeedCtx) + Send + 'static,
    ) -> std::io::Result<SubscriptionId> {
        let (sink, status, wake) =
            (Arc::clone(&self.sink), Arc::clone(&self.status), Arc::clone(&self.wake));
        let (inst, interval) = (inst.to_string(), interval.to_string());
        self.registry.spawn(format!("feed-okx-{inst}@{interval}"), move |stop| {
            let ctx = FeedCtx { sink, status, wake, stop };
            body(inst, interval, ctx)
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
            .map_err(|e| LiveDataError::Subscribe(format!("okx {symbol}@{interval}: {e}")))
    }

    fn subscribe_quotes(&mut self, _symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        // Caps-driven refusal (okx live_data.quotes = false): stays in lockstep with the table.
        require_live_verb(VENUE, LiveVerb::Quotes)?;
        unreachable!("okx declares no live quotes")
    }

    /// Start a live `trades` stream for `inst` (the OKX public `trades` channel — executed
    /// prints, no REST seed). Data flows out through [`LiveDataSink::trade`]; the returned id
    /// stops+joins just this stream (same per-key bookkeeping as `subscribe_bars`/
    /// `subscribe_depth`).
    fn subscribe_trades(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.spawn_with(symbol, StreamLabel::Trades.as_str(), trades_main)
            .map_err(|e| LiveDataError::Subscribe(format!("okx {symbol} trades: {e}")))
    }

    fn subscribe_book(&mut self, _symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        // Caps-driven refusal (okx live_data.book = false); use subscribe_depth for L2 depth.
        require_live_verb(VENUE, LiveVerb::Book)?;
        unreachable!("okx declares no lossless book lane")
    }

    /// Start a live L2 depth stream for `inst` (the OKX `books` 400-level channel). Data flows out
    /// through [`LiveDataSink::l2_snapshot`]; the returned id stops+joins just this stream.
    fn subscribe_depth(&mut self, inst: &str) -> Result<SubscriptionId, LiveDataError> {
        self.spawn_with(inst, StreamLabel::Depth.as_str(), depth_main)
            .map_err(|e| LiveDataError::Subscribe(format!("okx {inst} depth: {e}")))
    }

    /// Stop + JOIN exactly the stream `id` names — plus its companion SWAP mark stream when this
    /// was the LAST bars subscription holding that inst's mark stream; every other subscription on
    /// this `Feeds` keeps running. Unknown ids (already stopped, never issued) are a no-op.
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
