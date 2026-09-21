//! Live Deribit public market data → the vike-data live seam (split-plane I9): the venue's
//! [`vike_data::DataClient`] — the pump half of the new market-data plane
//! ([`crate::market_data`] holds the pure decoders). Until this module the venue HAD no live
//! market `DataClient` at all (`pump_spec` said `NoPump`; `LiveDataCaps::NONE`); the three public
//! feeds in [`crate::dvol`]/[`crate::options_feed`] stream mark/IV/ticker overlays, not
//! `DataClient` verbs.
//!
//! Same shape as the okx/bybit `Feeds`: one STOPPABLE thread per subscription, keyed by
//! [`SubscriptionId`] via the shared [`FeedRegistry`]; every WS session rides the shared
//! [`run_market_feed`] driver with the knobs CONSUMED from this venue's
//! `vike_bridge_core::pump_spec` row (row ownership — the values live there once). All four lanes
//! are keyless public MAINNET ([`crate::options_feed::MAINNET_WS`] — the crate's two-networks
//! split puts every public read on the real book; the authed exec half is testnet).
//!
//! The four lanes, and what each emits:
//! * **bars** (`chart.trades.{inst}.{res}`): REST warmup seed via [`crate::data`]'s pager (the
//!   SAME endpoint + column rules as backfill, so live and historical bars agree), then the live
//!   OHLCV channel. ⚠ Deribit pushes NO closed-bar flag — [`BarFolder`] infers a close when a push
//!   opens a NEWER bucket, so a bucket's `close_bar` lands with the first push of its successor
//!   (trade-driven: on a quiet instrument that can be late). Empty buckets are never fabricated,
//!   and a bucket that spanned a reconnect closes from the pre-outage state — best-effort by
//!   construction; the lossless history is the REST lane.
//! * **quotes** (`quote.{inst}`): venue-throttled top-of-book → [`LiveDataSink::quote`].
//! * **trades** (`trades.{inst}.100ms`): executed prints → [`LiveDataSink::trade`] (`amount`
//!   verbatim in venue contract units — the module-doc rule in [`crate::market_data`]).
//! * **book** (`book.{inst}.100ms`): the DeltaSync `change_id` chain folded into ONE standing
//!   [`L2Book`] behind an `Arc` (copy-on-write via `Arc::make_mut`, the bybit pattern) →
//!   [`LiveDataSink::book`]. A chain gap ([`MdEvent::Resync`]) ends the session — the reconnect's
//!   fresh subscribe delivers a fresh snapshot, so resync == resubscribe with the driver's
//!   backoff bounding the outage. The tick grid is inferred from each snapshot's own levels
//!   ([`infer_tick_size`]), so a re-seed can never keep a stale grid.
//!
//! Honestly refused / not wired: `subscribe_depth` (the conflating DOM `l2_snapshot` lane —
//! refused through the declared caps row via [`require_live_verb`], so the refusal cannot drift
//! from `vike_model::venue_caps::DERIBIT`); the recording-lane `LiveDataSink::book_update` raw
//! deltas (the folded-state `book` verb is what this feed serves — polymarket remains the one
//! `book_update` emitter); mark-stream pairing (deribit marks stream chain-wide via
//! [`crate::options_feed`]'s markprice feed, not a per-bars companion socket).
//!
//! Keepalive: every lane sends the app-level `public/test` ping the row's cadence declares — its
//! reply is the idle watchdog's only dependable inbound on a QUIET channel (a dead option's book
//! can be silent for minutes), the exact idiom [`crate::user_data`] proved on the private side.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use vike_bridge_core::depth::infer_tick_size;
use vike_bridge_core::market_pump::{FrameOutcome, MarketPumpOpts, SessionStatus, run_market_feed};
use vike_bridge_core::pump_spec::market_pump_spec;
use vike_data::{
    DataClient, FeedRegistry, LiveDataError, LiveDataSink, SubscriptionId, require_live_verb,
};
use vike_model::{Bar, L2Book, LiveVerb, now_ms};

use crate::market_data::{
    MdEvent, RpcReply, book_channel, chart_channel, classify_rpc_reply, parse_book_snapshot,
    parse_chart_bar, parse_quote, parse_trades, public_subscribe_frame, quote_channel, route_frame,
    trades_channel,
};
use crate::options_feed::MAINNET_WS;

const VENUE: &str = "deribit";

/// REST warmup depth: newest N bars (the last one still forming) — the binance/bybit seed size,
/// and comfortably inside one page of the deribit chart endpoint's ~5001-row cap.
const SEED_LIMIT: i64 = 1000;

/// The app-level `public/test` keepalive payload (the row declares the CADENCE; only the payload
/// text lives in the venue — [`vike_bridge_core::pump_spec::PumpKnobs::opts`]'s contract). Its
/// reply is a JSON-RPC result OBJECT, classified [`RpcReply::Other`] — inbound activity that
/// resets the idle watchdog, never mistaken for a subscribe ack.
const KEEPALIVE_PING: &str = r#"{"jsonrpc":"2.0","id":9929,"method":"public/test","params":{}}"#;

/// The shared [`MarketPumpOpts`] every Deribit market lane runs with — CONSUMED from this venue's
/// `MarketPumpSpec` row (row ownership, the tif.rs rule): subscribe frame replayed per session,
/// the `public/test` keepalive at the row's cadence, the idle watchdog (a dead subscribe/socket
/// reconnects; the JSON-RPC ack is classified but no ack watchdog is armed — the
/// [`crate::options_feed`] precedent), and the roster's shared backoff + bounded dial.
fn pump_opts(subscribe: &str) -> MarketPumpOpts<'_> {
    market_pump_spec(VENUE).knobs().opts(Some(subscribe), Some(KEEPALIVE_PING))
}

/// REST warmup seed: the newest [`SEED_LIMIT`] bars via [`crate::data::fetch_klines_range`] — the
/// same endpoint, resolution map and column rules as the backfill pager, end-anchored at now.
fn fetch_seed(symbol: &str, interval: &str) -> Result<Vec<Bar>, String> {
    let step = vike_model::time::interval_ms(interval)
        .ok_or_else(|| format!("deribit: unsupported interval {interval:?}"))?;
    let end = now_ms();
    crate::data::fetch_klines_range(symbol, interval, end - SEED_LIMIT * step, end)
}

struct FeedCtx {
    sink: Arc<dyn LiveDataSink>,
    status: Arc<Mutex<String>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    stop: Arc<AtomicBool>,
}

/// The ONE healthy status text every Deribit lane publishes — bars, quotes, trades and book
/// alike. One string per VENUE, not per lane: all FOUR lanes share ONE `Arc<Mutex<String>>`, so
/// per-lane spellings would make every session boundary on any lane a text change against the
/// others. `crates/bridges/bybit/src/market_feed.rs`'s `LIVE_STATUS` carries the full argument.
///
/// ⚠ **Consequence of moving the quotes/trades/book writes into the `SessionStatus::Live` arm:**
/// those three lanes now read [`Feeds::new`]'s `"connecting to Deribit…"` until their first
/// confirmed frame, where they used to claim LIVE before a socket existed. `parse_feed_status`
/// reads that as `Connecting`, which `vike_ops::reconcile_config`'s `health_from_feed_status` maps
/// to `Healthy` — and deribit contributes no `recon_feed_statuses` row in any case, so nothing is
/// gated on it. `bars_main` keeps its seed-time write and is unaffected.
const LIVE_STATUS: &str = "LIVE · Deribit";

impl FeedCtx {
    fn set_status(&self, s: String) {
        *self.status.lock().unwrap() = s;
        (self.wake)();
    }
}

/// The shared first step of every lane's `on_text`: decode the frame and settle the JSON-RPC
/// reply lane — `Ok(v)` hands the parsed NOTIFICATION back to the lane; `Err(outcome)` is the
/// driver verdict for everything else (ack → `Confirm`, venue error / dead subscribe → `Fatal`,
/// keepalive reply / junk → `Ignore`).
fn settle_reply(txt: &str) -> Result<Value, FrameOutcome> {
    let Ok(v) = serde_json::from_str::<Value>(txt) else {
        return Err(FrameOutcome::Ignore);
    };
    match classify_rpc_reply(&v) {
        RpcReply::Ack => Err(FrameOutcome::Confirm),
        RpcReply::Error(msg) => Err(FrameOutcome::Fatal(msg)),
        RpcReply::Other => Err(FrameOutcome::Ignore),
        RpcReply::NotReply => Ok(v),
    }
}

// ── bars: the closed-bar inference ───────────────────────────────────────────────────────────────

/// What [`BarFolder::fold`] made of one live chart push that MOVED the picture. A struct behind an
/// `Option` rather than a `{Stale, Roll{..}}` enum: the two arms differ by a whole [`Bar`] each, so
/// the enum shape is a `clippy::large_enum_variant` (CI runs `-D warnings`) and `None`-for-stale
/// reads at least as well.
#[derive(Debug, Clone, PartialEq)]
pub struct BarRoll {
    /// The PREVIOUS bucket, present exactly when this push opened a newer one — Deribit sends no
    /// closed flag, so the successor's first push IS the close signal.
    pub closed: Option<Bar>,
    /// The current bucket's latest state.
    pub forming: Bar,
}

/// The stateful closed-bar inference over the flag-less `chart.trades` stream (module doc). One
/// per bars subscription; the state lives in the feed's `on_text` closure, so it survives
/// reconnects (the shared driver reuses the closure across sessions).
#[derive(Default)]
pub struct BarFolder {
    forming: Option<Bar>,
}

impl BarFolder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one pushed bar: same bucket → an updated forming picture; a NEWER bucket → the held
    /// bucket closes and the new one forms; an OLDER bucket (a replayed/out-of-order push) →
    /// `None`, emitting nothing.
    pub fn fold(&mut self, bar: Bar) -> Option<BarRoll> {
        let closed = match &self.forming {
            Some(cur) if bar.ts < cur.ts => return None,
            Some(cur) if bar.ts > cur.ts => Some(cur.clone()),
            _ => None,
        };
        self.forming = Some(bar.clone());
        Some(BarRoll { closed, forming: bar })
    }
}

/// `subscribe_bars`'s thread body: resolution check (a permanent config error stops the feed — no
/// reconnect fixes an interval the venue does not serve), REST warmup ONCE (not per session — the
/// sibling venues' rule), then the live chart channel through [`BarFolder`].
fn bars_main(symbol: String, interval: String, ctx: FeedCtx) {
    let key = format!("{symbol}@{interval}");
    let resolution = match crate::data::resolution_code(&interval) {
        Ok(r) => r,
        Err(e) => {
            ctx.set_status(format!("{key}: {e}"));
            return;
        }
    };
    match fetch_seed(&symbol, &interval) {
        Ok(mut bars) => {
            let forming = if bars.len() > 1 { bars.pop() } else { None };
            ctx.sink.seed_bars(VENUE, &symbol, &interval, bars);
            if let Some(f) = forming {
                ctx.sink.bar_close_tick(VENUE, &symbol, f.close, f.ts);
                ctx.sink.forming_bar(VENUE, &symbol, &interval, f);
            }
            ctx.set_status(LIVE_STATUS.into());
        }
        Err(e) => ctx.set_status(format!("{key} seed error: {e}")),
    }
    let sub = public_subscribe_frame(&[chart_channel(&symbol, resolution)]);
    let mut folder = BarFolder::new();
    run_market_feed(
        MAINNET_WS,
        &pump_opts(&sub),
        &ctx.stop,
        &now_ms,
        |txt| {
            let v = match settle_reply(txt) {
                Ok(v) => v,
                Err(outcome) => return outcome,
            };
            let Some(bar) = parse_chart_bar(&v) else {
                return FrameOutcome::Ignore;
            };
            let Some(roll) = folder.fold(bar) else {
                return FrameOutcome::Ignore; // an older bucket — a replayed/out-of-order push
            };
            if let Some(c) = roll.closed {
                // the successor's first push IS the close signal — lossless lane
                ctx.sink.close_bar(VENUE, &symbol, &interval, c);
            }
            ctx.sink.bar_close_tick(VENUE, &symbol, roll.forming.close, roll.forming.ts);
            ctx.sink.forming_bar(VENUE, &symbol, &interval, roll.forming);
            (ctx.wake)();
            FrameOutcome::Confirm
        },
        || {},
        |s| match s {
            // THE FIX: the seed-time write covers the FIRST session only; this covers every later
            // one. Identical text, so a repeat says nothing.
            SessionStatus::Live => ctx.set_status(LIVE_STATUS.into()),
            SessionStatus::Error(e) => {
                ctx.set_status(format!("{key} ws error (reconnecting): {e}"))
            }
        },
    );
}

/// `subscribe_quotes`'s thread body: the venue-throttled `quote.{inst}` top-of-book channel.
/// Live-only (no seed — a quote stream has no history to splice).
fn quotes_main(symbol: String, _interval: String, ctx: FeedCtx) {
    let sub = public_subscribe_frame(&[quote_channel(&symbol)]);
    run_market_feed(
        MAINNET_WS,
        &pump_opts(&sub),
        &ctx.stop,
        &now_ms,
        |txt| {
            let v = match settle_reply(txt) {
                Ok(v) => v,
                Err(outcome) => return outcome,
            };
            let Some(mut q) = parse_quote(&v) else {
                return FrameOutcome::Ignore;
            };
            q.local_ts = now_ms(); // machine receive time (dual-timestamp capture)
            ctx.sink.quote(VENUE, &symbol, q);
            (ctx.wake)();
            FrameOutcome::Confirm
        },
        || {},
        |s| match s {
            // Replaces the spawn-time `"LIVE · Deribit quotes {symbol}"` write, which was made
            // before a socket existed and never again after.
            SessionStatus::Live => ctx.set_status(LIVE_STATUS.into()),
            SessionStatus::Error(e) => {
                ctx.set_status(format!("{symbol} quotes ws error (reconnecting): {e}"))
            }
        },
    );
}

/// `subscribe_trades`'s thread body: the `trades.{inst}.100ms` prints channel. Live-only.
fn trades_main(symbol: String, _interval: String, ctx: FeedCtx) {
    let sub = public_subscribe_frame(&[trades_channel(&symbol)]);
    run_market_feed(
        MAINNET_WS,
        &pump_opts(&sub),
        &ctx.stop,
        &now_ms,
        |txt| {
            let v = match settle_reply(txt) {
                Ok(v) => v,
                Err(outcome) => return outcome,
            };
            let ticks = parse_trades(&v);
            if ticks.is_empty() {
                return FrameOutcome::Ignore;
            }
            for mut tick in ticks {
                tick.local_ts = now_ms(); // machine receive time (dual-timestamp capture)
                ctx.sink.trade(VENUE, &symbol, tick);
            }
            (ctx.wake)();
            FrameOutcome::Confirm
        },
        || {},
        |s| match s {
            // Same move as the quotes lane's.
            SessionStatus::Live => ctx.set_status(LIVE_STATUS.into()),
            SessionStatus::Error(e) => {
                ctx.set_status(format!("{symbol} trades ws error (reconnecting): {e}"))
            }
        },
    );
}

/// `subscribe_book`'s thread body: ONE standing [`L2Book`] behind an `Arc` (publish = refcount
/// bump; folds copy-on-write via `Arc::make_mut`, off the single-writer core — the bybit
/// pattern), fed by [`route_frame`]'s DeltaSync chain. Each session's SNAPSHOT rebuilds the book
/// on the tick grid inferred from its own levels; a chain gap is a session fault, so the driver's
/// reconnect + resubscribe IS the resync (fresh subscribe → fresh snapshot).
fn book_main(symbol: String, _interval: String, ctx: FeedCtx) {
    let sub = public_subscribe_frame(&[book_channel(&symbol)]);
    let mut book = Arc::new(L2Book::new(0.0)); // rebuilt (grid inferred) on the first snapshot
    run_market_feed(
        MAINNET_WS,
        &pump_opts(&sub),
        &ctx.stop,
        &now_ms,
        |txt| {
            let v = match settle_reply(txt) {
                Ok(v) => v,
                Err(outcome) => return outcome,
            };
            if let Some((_seq, bids, asks)) = parse_book_snapshot(&v) {
                // A snapshot (re)builds the standing book on ITS inferred tick grid before the
                // routed apply below folds it — so a resync can never inherit a stale grid.
                book = Arc::new(L2Book::new(infer_tick_size(&bids, &asks)));
            }
            match route_frame(txt, &symbol, Arc::make_mut(&mut book)) {
                MdEvent::BookUpdated => {
                    ctx.sink.book(VENUE, &symbol, Arc::clone(&book));
                    (ctx.wake)();
                    FrameOutcome::Confirm
                }
                MdEvent::Resync => FrameOutcome::Fatal(format!(
                    "{symbol} book change_id chain broke (frames dropped) — resyncing via a \
                     fresh subscribe"
                )),
                MdEvent::Quote(_) | MdEvent::Trades(_) | MdEvent::Ignored => FrameOutcome::Ignore,
            }
        },
        || {},
        |s| match s {
            // ⚠ The SHARPEST case in the tree and the biggest single win. This lane's
            // `MdEvent::Resync` arm returns `FrameOutcome::Fatal` BY DESIGN — the reconnect IS
            // the resync — so a perfectly healthy deribit book feed writes a `Degraded`-reading
            // string as ORDINARY OPERATION. Before this arm existed that string was permanent.
            // (It is also the concrete writer `LiveFeeds::recon_feed_statuses` withholds
            // deribit's row for; that verdict is unchanged — see the write-frequency asymmetry
            // argument in its doc.)
            SessionStatus::Live => ctx.set_status(LIVE_STATUS.into()),
            SessionStatus::Error(e) => {
                ctx.set_status(format!("{symbol} book ws error (reconnecting): {e}"))
            }
        },
    );
}

/// All live feed threads, keyed by [`SubscriptionId`] via the shared [`FeedRegistry`] (one stop
/// flag + one `JoinHandle` per subscription; [`Feeds::unsubscribe`] stops+joins exactly one
/// stream, [`DataClient::shutdown`] stops+joins them all). Nothing is ever detached — the
/// deterministic-teardown rule.
pub struct Feeds {
    sink: Arc<dyn LiveDataSink>,
    pub status: Arc<Mutex<String>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    registry: FeedRegistry,
}

impl Feeds {
    /// `sink` receives every seed/close/forming/quote/trade/book call from every subscription
    /// this `Feeds` spawns (shared — construct once, subscribe many). `wake` is the GUI repaint
    /// nudge; pass `|| {}` headless. The registry's spawn hook is the venue's HFT affinity pin
    /// (opt-in via `VIKE_PIN_CORES`, no-op otherwise).
    pub fn new(sink: Arc<dyn LiveDataSink>, wake: impl Fn() + Send + Sync + 'static) -> Self {
        Feeds {
            sink,
            status: Arc::new(Mutex::new("connecting to Deribit…".into())),
            wake: Arc::new(wake),
            registry: FeedRegistry::with_spawn_hook(|| {
                vike_exec::affinity::pin_current_thread(
                    vike_exec::affinity::Role::MarketData,
                    "deribit",
                );
            }),
        }
    }

    /// Fallible spawn of the bars feed thread for `(symbol, interval)` — an OS thread-spawn
    /// failure is returned (not panicked) so [`DataClient::subscribe_bars`] can map it to a
    /// [`LiveDataError`]. Deribit instrument names are already unambiguous (`BTC-PERPETUAL`), so
    /// there is no `.P` split — the wire symbol IS the series symbol.
    pub fn try_spawn(&mut self, symbol: &str, interval: &str) -> std::io::Result<SubscriptionId> {
        self.spawn_with(symbol, interval, bars_main)
    }

    /// Shared per-key spawn bookkeeping — one [`FeedRegistry::spawn`] call: the registry
    /// allocates the id + stop flag and owns the join handle; this wrapper only assembles the
    /// venue's own [`FeedCtx`] around the registry-issued stop flag. `body` is a real lane main
    /// in production; tests substitute a network-free stand-in to exercise the per-key lifecycle
    /// deterministically.
    fn spawn_with(
        &mut self,
        symbol: &str,
        interval: &str,
        body: impl FnOnce(String, String, FeedCtx) + Send + 'static,
    ) -> std::io::Result<SubscriptionId> {
        let (sink, status, wake) =
            (Arc::clone(&self.sink), Arc::clone(&self.status), Arc::clone(&self.wake));
        let (symbol, interval) = (symbol.to_string(), interval.to_string());
        self.registry.spawn(format!("feed-deribit-{symbol}@{interval}"), move |stop| {
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
            .map_err(|e| LiveDataError::Subscribe(format!("deribit {symbol}@{interval}: {e}")))
    }

    /// Start the venue-throttled `quote.{inst}` L1 stream. Data flows out through
    /// [`LiveDataSink::quote`]; the returned id stops+joins just this stream.
    fn subscribe_quotes(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.spawn_with(symbol, "quotes", quotes_main)
            .map_err(|e| LiveDataError::Subscribe(format!("deribit {symbol} quotes: {e}")))
    }

    /// Start the `trades.{inst}.100ms` prints stream. Data flows out through
    /// [`LiveDataSink::trade`].
    fn subscribe_trades(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.spawn_with(symbol, "trades", trades_main)
            .map_err(|e| LiveDataError::Subscribe(format!("deribit {symbol} trades: {e}")))
    }

    /// Start the `book.{inst}.100ms` incremental L2 stream — the lossless folded-state lane
    /// ([`LiveDataSink::book`]); gap → resync via reconnect (module doc).
    fn subscribe_book(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.spawn_with(symbol, "book", book_main)
            .map_err(|e| LiveDataError::Subscribe(format!("deribit {symbol} book: {e}")))
    }

    fn subscribe_depth(&mut self, _symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        // Caps-driven refusal (deribit live_data.depth = false): the conflating DOM lane is not
        // wired — the lossless `subscribe_book` lane is this venue's L2 surface.
        require_live_verb(VENUE, LiveVerb::Depth)?;
        unreachable!("deribit declares no conflating depth lane")
    }

    /// Stop + JOIN exactly the stream `id` names; every other subscription keeps running.
    /// Unknown ids (already stopped, never issued) are a no-op.
    fn unsubscribe(&mut self, id: SubscriptionId) {
        self.registry.stop_join(id);
    }

    /// Raise every feed thread's stop flag and JOIN NOTHING — phase one of a multi-client
    /// teardown (each thread notices within one read-timeout tick).
    fn begin_shutdown(&mut self) {
        self.registry.raise_stops();
    }

    /// Deterministic teardown: raise every stop flag, then JOIN every feed thread.
    fn shutdown(&mut self) {
        self.registry.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::{BarFolder, BarRoll, FeedCtx, Feeds, KEEPALIVE_PING};
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    use vike_bridge_core::klines::kline_to_bar;
    use vike_data::{DataClient, RecordingSink};

    /// A network-free stand-in for a lane main: polls its own stop flag at the real feeds' WS
    /// cadence without ever touching a socket — the shared lifecycle-test idiom.
    fn fake_feed_body(_symbol: String, _interval: String, ctx: FeedCtx) {
        while !ctx.stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn subscribe_returns_distinct_ids() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        let id1 = feeds.spawn_with("BTC-PERPETUAL", "1m", fake_feed_body).expect("spawn ok");
        let id2 = feeds.spawn_with("ETH-PERPETUAL", "1m", fake_feed_body).expect("spawn ok");
        assert_ne!(id1, id2, "distinct ids per subscribe");
        feeds.shutdown();
    }

    #[test]
    fn unsubscribe_stops_only_that_one_feed_others_keep_running() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        let id1 = feeds.spawn_with("BTC-PERPETUAL", "1m", fake_feed_body).expect("spawn ok");
        let id2 = feeds.spawn_with("ETH-PERPETUAL", "1m", fake_feed_body).expect("spawn ok");

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
        let id = feeds.spawn_with("BTC-PERPETUAL", "1m", fake_feed_body).expect("spawn ok");
        feeds.unsubscribe(vike_data::SubscriptionId(id.0 + 100)); // never issued
        assert_eq!(feeds.registry.len(), 1, "unknown id must not disturb the real subscription");
        feeds.shutdown();
    }

    #[test]
    fn shutdown_joins_every_feed() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        feeds.spawn_with("BTC-PERPETUAL", "1m", fake_feed_body).expect("spawn ok");
        feeds.spawn_with("ETH-PERPETUAL", "1m", fake_feed_body).expect("spawn ok");
        feeds.spawn_with("SOL_USDC-PERPETUAL", "1m", fake_feed_body).expect("spawn ok");
        feeds.shutdown(); // must return only once every thread has actually joined
        assert!(feeds.registry.is_empty());
    }

    /// The ONE refused verb, driven off the declared caps row (never a hand-rolled string).
    #[test]
    fn depth_is_unsupported_via_the_declared_row() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        assert!(matches!(
            feeds.subscribe_depth("BTC-PERPETUAL"),
            Err(vike_data::LiveDataError::Unsupported(_))
        ));
    }

    /// The keepalive payload is a well-formed `public/test` JSON-RPC request — the frame whose
    /// reply resets the idle watchdog on a quiet channel (the row declares the cadence).
    #[test]
    fn keepalive_is_a_public_test_rpc() {
        let v: serde_json::Value = serde_json::from_str(KEEPALIVE_PING).unwrap();
        assert_eq!(v["method"], "public/test");
    }

    /// `pump_opts` CONSUMES the venue's pump_spec row (row ownership): the subscribe payload and
    /// the keepalive text are the venue's; every timing knob is the row's.
    #[test]
    fn pump_opts_consume_the_declared_row() {
        let sub = super::public_subscribe_frame(&["book.BTC-PERPETUAL.100ms".to_string()]);
        let opts = super::pump_opts(&sub);
        assert_eq!(opts.subscribe, Some(sub.as_str()));
        let knobs = vike_bridge_core::pump_spec::market_pump_spec("deribit").knobs();
        let ka = opts.keepalive.expect("the row declares a keepalive cadence");
        assert_eq!(ka.payload, KEEPALIVE_PING);
        assert_eq!(Some(ka.every), knobs.keepalive_every);
        assert_eq!(opts.read_timeout, knobs.read_timeout);
        assert_eq!(opts.idle_threshold, knobs.idle_threshold);
        assert_eq!(opts.connect_timeout, knobs.connect_timeout);
    }

    // ── BarFolder: the closed-bar inference ─────────────────────────────────────────────────────

    fn bar(ts: i64, close: f64) -> vike_model::Bar {
        kline_to_bar(ts, close, close, close, close, 1.0)
    }

    #[test]
    fn the_first_push_forms_and_closes_nothing() {
        let mut f = BarFolder::new();
        let out = f.fold(bar(60_000, 100.0));
        assert_eq!(out, Some(BarRoll { closed: None, forming: bar(60_000, 100.0) }));
    }

    #[test]
    fn a_same_bucket_push_updates_the_forming_picture() {
        let mut f = BarFolder::new();
        f.fold(bar(60_000, 100.0));
        let out = f.fold(bar(60_000, 101.0));
        assert_eq!(out, Some(BarRoll { closed: None, forming: bar(60_000, 101.0) }));
    }

    #[test]
    fn a_newer_bucket_closes_the_held_one_at_its_last_state() {
        let mut f = BarFolder::new();
        f.fold(bar(60_000, 100.0));
        f.fold(bar(60_000, 101.0));
        let out = f.fold(bar(120_000, 102.0));
        assert_eq!(
            out,
            Some(BarRoll { closed: Some(bar(60_000, 101.0)), forming: bar(120_000, 102.0) }),
            "the close is the bucket's LAST observed state, not its first"
        );
    }

    /// A quiet instrument can skip buckets entirely (chart pushes are trade-driven): the held
    /// bucket still closes; the skipped empties are NOT fabricated (module-doc honesty rule).
    #[test]
    fn a_bucket_jump_closes_the_held_bar_without_fabricating_empties() {
        let mut f = BarFolder::new();
        f.fold(bar(60_000, 100.0));
        let out = f.fold(bar(300_000, 105.0));
        assert_eq!(
            out,
            Some(BarRoll { closed: Some(bar(60_000, 100.0)), forming: bar(300_000, 105.0) })
        );
    }

    #[test]
    fn an_older_bucket_is_stale_and_changes_nothing() {
        let mut f = BarFolder::new();
        f.fold(bar(120_000, 102.0));
        assert_eq!(f.fold(bar(60_000, 999.0)), None);
        // …and the forming picture is untouched.
        let out = f.fold(bar(120_000, 103.0));
        assert_eq!(out, Some(BarRoll { closed: None, forming: bar(120_000, 103.0) }));
    }
}
