//! Live Deribit public market data → the vike-data live seam (split-plane I9): the venue's
//! [`vike_data::DataClient`] — the pump half of the new market-data plane
//! ([`crate::market_data`] holds the pure decoders). Until this module the venue HAD no live
//! market `DataClient` at all (`pump_spec` said `NoPump`; `LiveDataCaps::NONE`); the three public
//! feeds in [`crate::dvol`]/[`crate::options_feed`] stream mark/IV/ticker overlays, not
//! `DataClient` verbs.
//!
//! Same shape as the okx/bybit `Feeds`: one STOPPABLE thread per subscription, keyed by
//! [`SubscriptionId`] via the shared [`FeedRegistry`]; every WS session rides the shared
//! [`run_market_feed`] driver (the depth lane rides the shared depth driver, below) with the knobs
//! CONSUMED from this venue's `vike_bridge_core::pump_spec` row (row ownership — the values live
//! there once). All five lanes are keyless public MAINNET ([`crate::options_feed::MAINNET_WS`] —
//! the crate's two-networks split puts every public read on the real book; the authed exec half
//! is testnet).
//!
//! The five lanes, and what each emits:
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
//! * **depth** (`book.{inst}.100ms`, the SAME channel the book lane folds): the conflating DOM
//!   lane — the top `DEPTH_LEVELS` (200) of the folded book, published as
//!   [`LiveDataSink::l2_snapshot`] (the bybit/okx shape). It is a SECOND socket on the same
//!   channel rather than a tap on the book lane's, because a DOM and a lossless consumer subscribe
//!   independently (one `Feeds` serves both through the datahub's feed broker) and each needs its
//!   own anchor. It rides the shared [`run_depth_feed`] driver — the one every CEX sibling's depth
//!   lane uses — for its dead-transport and data-freshness watchdogs, its `stream_status`
//!   disclosure and its journal voice, with `decode_depth_frame` as the venue's whole protocol.
//!   ⚠ **Why this channel and not Deribit's grouped `book.{inst}.{group}.{depth}.{interval}`**:
//!   that one is a full snapshot per frame (no chain to track) but its `depth` parameter tops out
//!   at 20 levels (measured 2026-10-04: `20` is served; `50` and `100` answer an empty `result`),
//!   and a ladder wants the 200 bybit publishes.
//!   Sizes are the venue's CONTRACT unit, verbatim (USD notional on the inverse perpetuals, base
//!   coin on the linear/USDC ones and on spot), exactly as the trades and book lanes carry them.
//!
//! Honestly not wired: the recording-lane `LiveDataSink::book_update` raw deltas (the folded-state
//! `book` verb is what this feed serves — polymarket remains the one `book_update` emitter);
//! mark-stream pairing (deribit marks stream chain-wide via [`crate::options_feed`]'s markprice
//! feed, not a per-bars companion socket).
//!
//! Keepalive: every lane sends the app-level `public/test` ping the row's cadence declares — its
//! reply is the idle watchdog's only dependable inbound on a QUIET channel (a dead option's book
//! can be silent for minutes), the exact idiom [`crate::user_data`] proved on the private side.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

use vike_bridge_core::depth::{BookOp, FAULT_LOG_EVERY, infer_tick_size, run_depth_feed};
use vike_bridge_core::market_pump::{FrameOutcome, MarketPumpOpts, SessionStatus, run_market_feed};
use vike_bridge_core::pump_spec::market_pump_spec;
use vike_bridge_core::stream_health::{HealthEvent, health_to_stream_status};
use vike_data::{DataClient, FeedRegistry, LiveDataError, LiveDataSink, SubscriptionId};
use vike_model::{Bar, L2Book, now_ms};

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
/// `MarketPumpSpec` row (row ownership, the `venue_tif` rule): subscribe frame replayed per
/// session, the `public/test` keepalive at the row's cadence, the idle watchdog (a dead
/// subscribe/socket reconnects; the JSON-RPC ack is classified but no ack watchdog is armed — the
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
/// alike. One string per VENUE, not per lane: all FOUR of those lanes share ONE
/// `Arc<Mutex<String>>`, so per-lane spellings would make every session boundary on any lane a text
/// change against the others. `crates/bridges/bybit/src/market_feed.rs`'s `LIVE_STATUS` carries the
/// full argument. (The depth lane is not a fifth writer: the shared depth driver it rides has no
/// status hook, so it speaks through `stream_status` only — the bybit and binance shape.)
///
/// ⚠ **Consequence of moving the quotes/trades/book writes into the `SessionStatus::Live` arm:**
/// those three lanes now read [`Feeds::new`]'s `"connecting to Deribit…"` until their first
/// confirmed frame, where they used to claim LIVE before a socket existed. `parse_feed_status`
/// reads that as `Connecting`, which `vike_tradehub::reconcile_config`'s `health_from_feed_status` maps
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

// ── depth: the DOM's conflating lane ─────────────────────────────────────────────────────────────

/// Levels per side published to the DOM — bybit's number (`crates/bridges/bybit/src/market_feed.rs`'s
/// `DEPTH_LEVELS`) and the datahub's own per-subscriber ceiling
/// (`crates/vike-datahub-client/src/market.rs`'s `MD_DEPTH_LEVELS_CEILING`), so this feed never
/// withholds a row any consumer can ask for.
const DEPTH_LEVELS: usize = 200;

/// Reconnect backoff after a socket error or a chain gap — the siblings' 3 s, and the figure this
/// venue's pump row declares for its other four lanes.
const DEPTH_BACKOFF: Duration = Duration::from_secs(3);

/// Net-hardening §B data-freshness threshold: how long the book may go without an applied update,
/// behind a still-live transport, before `Stale` is disclosed (disclose-only, no auto-action). The
/// siblings' 120 s, whose measurement is `crates/bridges/bybit/src/market_feed.rs`'s
/// `DEPTH_FRESHNESS_THRESHOLD`: it clears a thin pair's worst gap. ⚠ A far-dated OPTION's book can
/// be silent for longer (this module's keepalive note), and will read `Stale` there — which is
/// true, and a disclosure rather than a fault.
const DEPTH_FRESHNESS_THRESHOLD: Duration = Duration::from_secs(120);

/// Net-hardening timed re-seed, on the siblings' 300 s. Deribit's `prev_change_id` chain already
/// turns every dropped frame into a [`BookOp::Gap`], so this is parity rather than a second line of
/// defence — and a scheduled refresh costs one snapshot (MEASURED 2026-10-04: `BTC-PERPETUAL`'s is
/// 46 KB; a deep spot book's, `BTC_USDC`'s, is 1.1 MB — 23,912 bids and 18,450 asks) with no
/// transport gap disclosed.
const DEPTH_RESEED_INTERVAL: Duration = Duration::from_secs(300);

/// The venue EVENT-TIME (epoch-ms) of a `book.*` notification — `params.data.timestamp` — for the
/// §B data-freshness watchdog (`0` if absent → the driver falls back to receive-time). Re-parses
/// just that field; the fixture-tested [`crate::market_data`] decoders don't surface it and this
/// keeps them untouched.
fn frame_ts_ms(txt: &str) -> i64 {
    serde_json::from_str::<Value>(txt)
        .ok()
        .and_then(|v| v.pointer("/params/data/timestamp").and_then(Value::as_i64))
        .unwrap_or(0)
}

/// Publish the top [`DEPTH_LEVELS`] to the DOM sink — carrying the book's tick, so a sink
/// quantizes on the SAME grid the feed folded on — and nudge a repaint. The stamped `ts` is the
/// RECEIPT time, which is what a staleness check wants (how long since the last update).
fn publish_book(book: &L2Book, symbol: &str, ctx: &FeedCtx) {
    let (bids, asks) = book.top_n(DEPTH_LEVELS);
    ctx.sink.l2_snapshot(VENUE, symbol, book.tick_size, bids, asks, now_ms());
    (ctx.wake)();
}

/// The venue's REFUSAL of the subscribe itself, if `txt` is one: a JSON-RPC `error` envelope, or a
/// `result` array naming no channel — what a misspelled or expired instrument gets. MEASURED
/// 2026-10-04: `book.NOT-AN-INSTRUMENT.100ms` answers `{"result":[]}` and then NOTHING, ever, so
/// without this a bad key would idle behind a keepalive'd socket until the freshness watchdog said
/// `Stale` two minutes later. `None` for every other frame — an ack, the keepalive's reply, data.
fn subscribe_refusal(txt: &str) -> Option<String> {
    let v = serde_json::from_str::<Value>(txt).ok()?;
    match classify_rpc_reply(&v) {
        RpcReply::Error(why) => Some(why),
        RpcReply::Ack | RpcReply::Other | RpcReply::NotReply => None,
    }
}

/// Fold one `book.*` WS frame into the (maybe not-yet-anchored) book slot — the depth driver's
/// `decode` body, named so the frame→[`BookOp`] mapping is unit-testable without a socket.
///
/// Before the first `snapshot` every frame is [`BookOp::Ignored`] (the ack, a keepalive reply, a
/// change that outran its anchor). The snapshot builds the book on the tick grid inferred from its
/// own levels and anchors the chain at its `change_id`; from then on frames fold through the
/// fixture-tested [`route_frame`], whose `prev_change_id` rule maps a dropped frame to
/// [`BookOp::Gap`] — on which the shared driver discloses the gap and reconnects + re-seeds
/// immediately. A replayed frame (`change_id` already reflected) and a quote/trade row on a mixed
/// socket are [`BookOp::Ignored`] with the book untouched.
fn fold_depth_frame(txt: &str, sym: &str, book: &mut Option<L2Book>) -> BookOp {
    match book.as_mut() {
        None => {
            let Ok(v) = serde_json::from_str::<Value>(txt) else { return BookOp::Ignored };
            let Some((seq, bids, asks)) = parse_book_snapshot(&v) else { return BookOp::Ignored };
            let mut bk = L2Book::new(infer_tick_size(&bids, &asks));
            bk.apply_snapshot(seq, &bids, &asks);
            *book = Some(bk);
            BookOp::Updated(frame_ts_ms(txt))
        }
        Some(bk) => match route_frame(txt, sym, bk) {
            MdEvent::BookUpdated => BookOp::Updated(frame_ts_ms(txt)),
            MdEvent::Resync => BookOp::Gap,
            MdEvent::Quote(_) | MdEvent::Trades(_) | MdEvent::Ignored => BookOp::Ignored,
        },
    }
}

/// The depth driver's whole `decode`: [`fold_depth_frame`], preceded — only while the book is still
/// unanchored, which is the one window a subscribe refusal can arrive in — by the refusal check.
///
/// ⚠ **A refused subscribe is spoken, and throttled.** The venue answers an unknown instrument with
/// an empty `result` and then silence ([`subscribe_refusal`]), so it maps to [`BookOp::Gap`]: the
/// driver reconnects after [`DEPTH_BACKOFF`], exactly as this venue's other four lanes do on the
/// same refusal (`classify_rpc_reply` → `Fatal`). The driver's own journal line for that fault
/// would say "sequence gap", which is wrong, so the real words go out here, at most once per
/// [`FAULT_LOG_EVERY`] — the driver's own cadence for a lane that keeps faulting. `spoke_at` is the
/// caller's throttle state (on the injected clock `now`), kept across reconnects the way the
/// driver's own fault log is.
fn decode_depth_frame(
    txt: &str,
    sym: &str,
    book: &mut Option<L2Book>,
    spoke_at: &mut Option<i64>,
    now: i64,
) -> BookOp {
    if book.is_none()
        && let Some(why) = subscribe_refusal(txt)
    {
        if spoke_at.is_none_or(|t| now - t >= FAULT_LOG_EVERY.as_millis() as i64) {
            *spoke_at = Some(now);
            tracing::warn!(
                target: "vike_deribit::market_feed",
                symbol = %sym,
                "depth subscribe refused — {why}"
            );
        }
        return BookOp::Gap;
    }
    fold_depth_frame(txt, sym, book)
}

/// `subscribe_depth`'s thread body: the DOM's conflating lane over the shared depth driver. Deribit's
/// protocol is WS-seeded (no REST): `seed` answers `None`, the first `snapshot` frame builds the book
/// and every change after it is folded and re-published as an [`LiveDataSink::l2_snapshot`]
/// ([`decode_depth_frame`] is the whole protocol, [`publish_book`] the whole emission).
///
/// The idle watchdog and the stop-poll cadence are CONSUMED from the venue's pump row (row
/// ownership), where the other four lanes read them; only the depth-specific figures above are
/// local, as in every sibling.
fn depth_main(symbol: String, _interval: String, ctx: FeedCtx) {
    let sub = public_subscribe_frame(&[book_channel(&symbol)]);
    let sym = symbol.as_str();
    let knobs = market_pump_spec(VENUE).knobs();
    let mut refusal_spoke_at: Option<i64> = None;
    let decode = |txt: &str, book: &mut Option<L2Book>| -> BookOp {
        decode_depth_frame(txt, sym, book, &mut refusal_spoke_at, now_ms())
    };
    run_depth_feed(
        MAINNET_WS,
        Some(&sub),
        Some(KEEPALIVE_PING),
        || None::<L2Book>,
        decode,
        |b: &L2Book| publish_book(b, sym, &ctx),
        // Net-hardening §B: each driver-neutral `HealthEvent` onto the sink's machine-readable
        // disclosure, under the `depth` stream label the datahub's `MdLane::Depth` maps back from.
        |ev: HealthEvent| ctx.sink.stream_status(VENUE, sym, "depth", health_to_stream_status(ev)),
        &ctx.stop,
        knobs.read_timeout,
        // `None` is the row declaring NO idle watchdog; `Duration::MAX` is that, to a driver that
        // always takes one. (The deribit row declares one.)
        knobs.idle_threshold.unwrap_or(Duration::MAX),
        DEPTH_FRESHNESS_THRESHOLD,
        Some(DEPTH_RESEED_INTERVAL),
        &now_ms,
        DEPTH_BACKOFF,
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

    /// Start the conflating DOM lane: the top [`DEPTH_LEVELS`] of the `book.{inst}.100ms` fold,
    /// delivered through [`LiveDataSink::l2_snapshot`]; the returned id stops+joins just this
    /// stream. The declared row says the same (`vike_model::venues::venue_caps::DERIBIT`'s
    /// `live_data.depth`), which is what the datahub's `MdLane::Depth` gate reads.
    fn subscribe_depth(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.spawn_with(symbol, "depth", depth_main)
            .map_err(|e| LiveDataError::Subscribe(format!("deribit {symbol} depth: {e}")))
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

#[path = "market_feed_tests.rs"]
#[cfg(test)]
mod market_feed_tests;
