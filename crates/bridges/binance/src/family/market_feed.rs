//! The Binance-grammar live kline + DOM-depth feed body — shared by vike-binance
//! (`crate::market_feed`) and vike-aster (`vike_aster::market_feed`), each passing its own
//! [`FamilySpec`] (F17, dedup rung 2).
//!
//! Aster's kline/depth wire grammar is Binance-verbatim (same `@kline_<interval>` /
//! `@depth@100ms` stream names, 12-element klines, U/u depth recovery), so the two venues'
//! `market_feed.rs` files were copies. The frame decode, the DOM depth lane over
//! `vike_bridge_core::depth::run_depth_feed`, and the seed-then-stream body live here once.
//!
//! **Dedup A6 (wave 3):** the kline WS session LIFECYCLE (connect, read-timeout stop poll,
//! server-ping auto-pong, stop-aware 30×100 ms reconnect backoff) rides the shared
//! [`vike_bridge_core::market_pump`] driver now — behavior-identical to the pre-driver copy
//! (bybit was the wave-3 proof venue). Only the PROTOCOL stays here: the URL-path subscription
//! (no subscribe frame — see `pump_opts`'s br7 note), the fixture-tested frame decode, the
//! REST warmup seed, and the sink emission with the exact status strings.
//!
//! **What deliberately stays per-venue.** The `Feeds` struct itself, its constructors, its
//! `spawn_with` per-key bookkeeping and its `DataClient` impl remain in each venue's own module:
//! Aster's `Feeds` carries an `Environment` field and a `with_env` constructor that Binance has no
//! concept of, and Rust's orphan rule forbids Aster adding inherent constructors to a type owned by
//! vike-binance. Both venues' scripted lifecycle tests also reach their own `Feeds`' private
//! `subs`/`env` directly. So the SHELL is per-venue and the BODY is shared — which is where the
//! duplication actually was.
//!
//! **Host resolution is the one real delta** and is passed in, never decided here: Binance hands in
//! a `const` [`UrlTable`] ([`crate::market_feed::BINANCE_URLS`]) and is NOT env-resolved; Aster
//! maps its `urls::urls_for(env)` into the same shape.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use vike_bridge_core::depth::{BookOp, infer_tick_size, run_depth_feed};
use vike_bridge_core::klines::kline_to_bar;
use vike_bridge_core::market_pump::{FrameOutcome, MarketPumpOpts, run_market_feed};
use vike_bridge_core::pump_spec::market_pump_spec;
use vike_bridge_core::stream_health::HealthEvent;
use vike_data::{LiveDataSink, StreamStatus};
use vike_model::{Bar, L2Book};

use super::FamilySpec;
use super::depth::{DepthOutcome, DepthSnapshot, apply_depth_event};

/// The DEPTH driver's read timeout (the market pumps' own read timeout now rides the
/// `MarketPumpSpec` row [`pump_opts`] consumes).
pub const READ_TIMEOUT: Duration = Duration::from_secs(2);
/// Newest N klines for the REST warmup seed.
pub const SEED_LIMIT: usize = 1000;

// --- kline frames --------------------------------------------------------------------------

#[derive(Deserialize)]
struct KlineMsg {
    k: KlineData,
}
#[derive(Deserialize)]
struct KlineData {
    t: i64,
    o: String,
    h: String,
    l: String,
    c: String,
    v: String,
    x: bool,
}

/// One decoded `@kline_<interval>` frame: the bar plus whether the venue marked it CLOSED (`x`).
pub struct KlineFrame {
    pub bar: Bar,
    pub closed: bool,
}

/// Decode ONE `@kline_<interval>` text frame. `Ok(None)` = not a kline frame (the caller skips it);
/// `Err` = a malformed numeric field (the caller drops the session so it reconnects) — exactly the
/// two-tier handling both venues' pre-driver session loops had inline.
pub fn decode_kline_frame(txt: &str) -> Result<Option<KlineFrame>, std::num::ParseFloatError> {
    let Ok(m) = serde_json::from_str::<KlineMsg>(txt) else {
        return Ok(None);
    };
    let bar = kline_to_bar(
        m.k.t,
        m.k.o.parse::<f64>()?,
        m.k.h.parse::<f64>()?,
        m.k.l.parse::<f64>()?,
        m.k.c.parse::<f64>()?,
        m.k.v.parse::<f64>().unwrap_or(0.0),
    );
    Ok(Some(KlineFrame { bar, closed: m.k.x }))
}

/// The `@kline_<interval>` WS URL for a resolved (bare) stream host.
pub fn kline_ws_url(ws_host: &str, api_symbol: &str, interval: &str) -> String {
    format!("{ws_host}/ws/{}@kline_{}", api_symbol.to_lowercase(), interval)
}

/// The `@depth@100ms` WS URL for a resolved (bare) stream host.
pub fn depth_ws_url(ws_host: &str, symbol: &str) -> String {
    format!("{ws_host}/ws/{}@depth@100ms", symbol.to_lowercase())
}

// --- the per-subscription thread context ---------------------------------------------------

/// The per-subscription wiring every feed-thread body receives. Owned here so both venues' bodies
/// are the same code; each venue's own `Feeds::spawn_with` constructs one (supplying its
/// [`FamilySpec`]) and its tests substitute a network-free stand-in with the same shape.
pub struct FeedCtx {
    pub sink: Arc<dyn LiveDataSink>,
    pub status: Arc<Mutex<String>>,
    pub wake: Arc<dyn Fn() + Send + Sync>,
    pub stop: Arc<AtomicBool>,
    /// Which venue this thread serves + its resolved hosts — set once at `Feeds` construction.
    pub spec: FamilySpec,
}

impl FeedCtx {
    pub fn set_status(&self, s: String) {
        *self.status.lock().unwrap() = s;
        (self.wake)();
    }
}

/// Machine receive time in epoch-ms. Delegates to [`vike_model::clock::now_ms`] — the consolidation
/// point for the `SystemTime::now()…` idiom. Single-sited here now that every caller (the depth
/// driver's `&now_ms` fn-pointer, `publish_book`, and `family::trades`' receive stamp) lives in
/// this module tree; the delegation itself is unchanged.
pub fn now_ms() -> i64 {
    vike_model::clock::now_ms()
}

// --- live kline session --------------------------------------------------------------------

/// The shared [`MarketPumpOpts`] both family market pumps (kline here, aggTrade in
/// [`super::trades`]) run with — CONSUMED from the calling venue's `MarketPumpSpec` row
/// ([`market_pump_spec`], row ownership; the binance and aster rows are pinned identical since
/// the family shares these pumps):
///
/// - `subscribe: None` — this family encodes the subscription IN THE URL PATH
///   (`/ws/<symbol>@kline_<interval>` / `…@aggTrade`); no subscribe frame is ever sent.
/// - `ack_timeout: None` — **net-hardening br7: why the subscribe-ack handshake the OKX/Bybit
///   kline feeds gained does NOT apply here.** Those venues open ONE public socket and send an
///   `{op:"subscribe"}` frame the venue replies to with an ACK or an `error`; a silently-dropped
///   ack/error was the hole br7 closed. This family has no subscribe frame and thus NO ack frame
///   to time out on and NO `event:"error"` reject frame to attribute. A bad symbol/interval fails
///   at `tungstenite::connect` (already a disclosed session error → reconnect) or opens but
///   streams nothing — the data-starvation case a FRESHNESS watchdog covers, which the kline bar
///   feed doesn't run (only the depth feed does; see [`depth_main`]'s
///   `DEPTH_FRESHNESS_THRESHOLD`). So there is nothing to ack and a fake ack was deliberately NOT
///   forced; the URL-subscribe contract is the note.
/// - `keepalive: None` — the venue server pings and the driver's stream auto-pongs; these lanes
///   never sent an app-level ping, so none is invented here.
/// - `idle_threshold: None` — no idle watchdog on these lanes (as before).
pub(super) fn pump_opts(venue: &str) -> MarketPumpOpts<'static> {
    market_pump_spec(venue).knobs().opts(None, None)
}

/// The seed-then-stream feed-thread body. `fetch_seed` is the venue's own kline REST warmup (each
/// crate's `data::fetch_klines_latest`, with any `env` already bound by the caller) — the one part
/// that can't be shared, because the two crates' fetchers have different arities.
///
/// The WS session/reconnect LIFECYCLE (connect, read-timeout stop poll, server-ping auto-pong,
/// stop-aware 30×100 ms reconnect backoff) rides the shared
/// [`vike_bridge_core::market_pump::run_market_feed`] driver (dedup A6, wave 3 — bybit is the
/// worked example); only the PROTOCOL stays here: the URL-path subscription, the fixture-tested
/// [`decode_kline_frame`], and the sink emission. Each decoded kline emits under the SERIES symbol
/// (last price → the conflated mark cache, then the lossless `close_bar` or conflating
/// `forming_bar` lane); a malformed numeric field ends the session as an error so it reconnects —
/// exactly the pre-driver `decode_kline_frame(txt)?` propagation, same status text.
///
/// `series_symbol` is the catalog/sink label (`.P`-suffixed for a perp — the core key);
/// `api_symbol` is the `.P`-stripped exchange symbol used for the REST seed + WS URL. Spot
/// subscriptions pass `series_symbol == api_symbol` and `is_perp = false`. `is_perp` picks the
/// host from the spec's [`UrlTable`] (the `KlineMsg` frame shape is identical on both).
///
/// [`UrlTable`]: super::UrlTable
pub fn feed_main(
    series_symbol: String,
    api_symbol: String,
    interval: String,
    is_perp: bool,
    ctx: FeedCtx,
    fetch_seed: impl Fn(&str, &str, bool) -> Result<Vec<Bar>, String>,
) {
    let venue = ctx.spec.venue;
    let key = format!("{series_symbol}@{interval}");
    // REST warmup: the last kline in the response is the still-forming one — seed the
    // closed prefix, route the tail through the forming lane. Once per FEED, not per session —
    // a reconnect does not re-seed (the pre-driver behavior).
    match fetch_seed(&api_symbol, &interval, is_perp) {
        Ok(mut bars) => {
            let forming = if bars.len() > 1 { bars.pop() } else { None };
            ctx.sink.seed_bars(venue, &series_symbol, &interval, bars);
            if let Some(f) = forming {
                ctx.sink.bar_close_tick(venue, &series_symbol, f.close, f.ts);
                ctx.sink.forming_bar(venue, &series_symbol, &interval, f);
            }
            ctx.set_status(format!("LIVE · {}", ctx.spec.display));
        }
        Err(e) => ctx.set_status(format!("{key} seed error: {e}")),
    }
    let url = kline_ws_url(ctx.spec.urls.ws(is_perp), &api_symbol, &interval);
    run_market_feed(
        &url,
        &pump_opts(venue),
        &ctx.stop,
        &now_ms,
        |txt| match decode_kline_frame(txt) {
            Ok(Some(frame)) => {
                let bar = frame.bar;
                // last price -> the core's bar-close cache (conflated) — under the SERIES symbol,
                // so it lands on the same key the chart/core reads. `bar_close_tick`, NOT
                // `mark_tick`: a candle close is not the venue mark (mark-slot semantics; the
                // real mark rides the perp `@markPrice@1s` pump, `mark_main`).
                ctx.sink.bar_close_tick(venue, &series_symbol, bar.close, bar.ts);
                if frame.closed {
                    // bar CLOSED — lossless lane (a missed close = a series hole)
                    ctx.sink.close_bar(venue, &series_symbol, &interval, bar);
                } else {
                    ctx.sink.forming_bar(venue, &series_symbol, &interval, bar);
                }
                (ctx.wake)();
                FrameOutcome::Confirm
            }
            Ok(None) => FrameOutcome::Ignore, // not a kline frame
            // A malformed numeric field drops the session so it reconnects (the pre-driver
            // two-tier handling); the `ParseFloatError` Display is the same status text as before.
            Err(e) => FrameOutcome::Fatal(e.to_string()),
        },
        || {}, // no dataless-tick judgment on this lane (the polymarket freshness knob)
        |e| ctx.set_status(format!("{key} ws error (reconnecting): {e}")),
    );
}

// --- live perp mark-price stream (mark-slot semantics, W2-T4) --------------------------------
// The USDⓈ-M `<symbol>@markPrice@1s` stream: the venue's REAL mark price (the price liquidation
// and funding key off), pushed every second. Feeds `LiveDataSink::mark_tick` — the `PriceBoard`
// MARK slot — while the kline pump's candle closes ride `bar_close_tick`. Perp-only: spot has no
// mark price. Same URL-path-subscribe pump shape as the kline lane (no subscribe frame, no ack —
// the br7 note on `pump_opts` applies verbatim).

#[derive(Deserialize)]
struct MarkMsg {
    e: String,
    #[serde(rename = "E")]
    event_ts: i64,
    p: String,
}

/// One decoded `markPriceUpdate` frame: the venue's REAL mark price + its event time (ms).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MarkFrame {
    pub px: f64,
    pub ts: i64,
}

/// Decode ONE `@markPrice@1s` text frame. `Ok(None)` = not a mark frame OR a dead (0/neg/NaN)
/// mark price (skipped in place — a conflating lane tolerates a dropped frame); `Err` = a
/// malformed `p` decimal (the caller drops the session so it reconnects) — the same two-tier
/// handling as [`decode_kline_frame`].
pub fn decode_mark_frame(txt: &str) -> Result<Option<MarkFrame>, std::num::ParseFloatError> {
    let Ok(m) = serde_json::from_str::<MarkMsg>(txt) else {
        return Ok(None);
    };
    if m.e != "markPriceUpdate" {
        return Ok(None);
    }
    let px = m.p.parse::<f64>()?;
    if !vike_bridge_core::is_valid_mark(px) {
        return Ok(None);
    }
    Ok(Some(MarkFrame { px, ts: m.event_ts }))
}

/// The `@markPrice@1s` WS URL for a resolved (bare) PERP stream host.
pub fn mark_ws_url(ws_host: &str, api_symbol: &str) -> String {
    format!("{ws_host}/ws/{}@markPrice@1s", api_symbol.to_lowercase())
}

/// The perp mark-price feed-thread body (matches each venue's `spawn_with` shape). Emits every
/// valid mark under the SERIES symbol (the `.P`-suffixed core key) via `mark_tick`; a malformed
/// decimal ends the session as an error so it reconnects (the kline lane's two-tier rule). No
/// REST seed — the mark is a live-only valuation stream; the resolver's bar-close fallback covers
/// the pre-first-frame window.
pub fn mark_main(series_symbol: String, api_symbol: String, ctx: FeedCtx) {
    let venue = ctx.spec.venue;
    let url = mark_ws_url(ctx.spec.urls.ws(true), &api_symbol);
    run_market_feed(
        &url,
        &pump_opts(venue),
        &ctx.stop,
        &now_ms,
        |txt| match decode_mark_frame(txt) {
            Ok(Some(m)) => {
                ctx.sink.mark_tick(venue, &series_symbol, m.px, m.ts);
                (ctx.wake)();
                FrameOutcome::Confirm
            }
            Ok(None) => FrameOutcome::Ignore,
            Err(e) => FrameOutcome::Fatal(e.to_string()),
        },
        || {},
        |e| ctx.set_status(format!("{series_symbol} mark ws error (reconnecting): {e}")),
    );
}

// --- L2 depth (full managed book) feed ------------------------------------------------------
// The diff-depth stream `<symbol>@depth@100ms`, managed the documented way: seed from a REST depth
// snapshot, then fold diffs under the `U`/`u` sequence rule (a gap re-snapshots) — reusing the
// pure, fixture-tested primitives in [`super::depth`]. Delivers the top [`DEPTH_LEVELS`] of the
// maintained book through the conflating lane [`LiveDataSink::l2_snapshot`] on every applied diff,
// so the DOM renders a deep book (hundreds of levels) rather than a 20-level partial. The GUI-side
// store re-infers the display tick from the delivered levels.

/// How many levels per side to publish to the DOM (the managed book holds far more; this is the
/// render/transmit window — deep enough for a grouped ladder, cheap to clone each 100 ms).
const DEPTH_LEVELS: usize = 200;
/// Reconnect backoff after a socket error or sequence gap — throttles resync so a persistent gap
/// can't hammer the weight-50 depth snapshot into a rate-limit ban.
const DEPTH_BACKOFF: Duration = Duration::from_secs(3);
/// Net-hardening §B dead-transport watchdog: no depth frame of ANY kind (a `@depth@100ms` diff OR a
/// server WS Ping) for this long means the socket is silently dead → the driver discloses a gap and
/// reconnects+re-seeds. Sized well above the ~100ms diff cadence and the server-ping interval so a
/// healthy book (near-continuous updates, whose control frames also refresh the liveness clock)
/// never false-trips; only true transport silence does.
const DEPTH_IDLE_THRESHOLD: Duration = Duration::from_secs(30);
/// §B data-freshness threshold for the depth book: how long the book may go without an applied
/// update, behind a still-live transport, before disclosing `Stale` (disclose-only, no auto-action;
/// the fast dead-socket signal is [`DEPTH_IDLE_THRESHOLD`], 30s). **Validated by live measurement
/// (2026-07-11) across the binance/bybit/okx keyless depth feeds: liquid pairs (BTC/ETH) update
/// ~0.1s, mid-caps ~3.6s max, and the market's THINNEST actively-traded pairs gapped 44–47s (a
/// near-dead pair updated only twice in 5 min).** 120s clears the thin-pair worst case with ~2.5×
/// margin, so only a genuinely near-dead book trips — where the `Stale` disclosure is accurate. Kept
/// at 120s (measured floor, not a placeholder); a uniform per-venue value must cover the thinnest
/// subscribable symbol (#111). Measured on Binance; carried to Aster, whose own liquidity profile is
/// UNVERIFIED against it until its testnet smoke runs (treat as provisional there).
const DEPTH_FRESHNESS_THRESHOLD: Duration = Duration::from_secs(120);
/// Net-hardening timed book re-seed: forced fresh-snapshot cadence. Neither venue publishes a book
/// checksum, so a dropped/mis-applied `@depth@100ms` diff that does NOT break the `U`/`u` seq chain
/// corrupts the managed book silently — no `BookOp::Gap`, no idle, no freshness trip fires, and it
/// would stay wrong until the next natural reconnect (hours). Forcing a re-snapshot every
/// `DEPTH_RESEED_INTERVAL` bounds that corruption window to <= this. The driver reuses the gap
/// handler's reconnect + re-seed path but discloses NO transport gap (a scheduled refresh, not an
/// outage). 5 min is a conservative default — well above the reconnect + weight-50 depth snapshot
/// cost, so the churn is negligible; `None` would disable it.
const DEPTH_RESEED_INTERVAL: Duration = Duration::from_secs(300);
/// Placeholder tick for the empty book seeded when a snapshot fetch fails transiently — never used
/// for quantization (the empty book carries no levels and is replaced by the next real seed); it
/// only needs to be positive so `L2Book` construction is valid.
const DEPTH_SEED_FALLBACK_TICK: f64 = 0.01;

/// Map the driver-neutral [`HealthEvent`] onto the `vike_data::StreamStatus` disclosure vocabulary
/// (1:1). Kept out of `vike-bridge-core` because that crate is deliberately `vike-data`-free; the
/// depth driver owns the `StreamHealth` that dedups transport gaps across reconnects now, so this is
/// a pure translation at the `LiveDataSink` boundary.
fn health_to_stream_status(ev: HealthEvent) -> StreamStatus {
    match ev {
        HealthEvent::Gap { at_ts_ms } => StreamStatus::GapStart { at_ts_ms },
        HealthEvent::Live { gap_started_ts_ms } => StreamStatus::Live { gap_started_ts_ms },
        HealthEvent::Stale { newest_data_ts_ms, now_ms } => {
            StreamStatus::Stale { newest_data_ts_ms, now_ms }
        }
    }
}

/// Publish the top [`DEPTH_LEVELS`] of `book` to the DOM sink — carrying the book's tick so the GUI
/// quantizes on the SAME grid — + nudge a repaint. The stamped `ts` is the RECEIPT time, which is
/// what a staleness check wants (how long since the last update), not the venue event time.
fn publish_book(book: &L2Book, symbol: &str, ctx: &FeedCtx) {
    let (bids, asks) = book.top_n(DEPTH_LEVELS);
    ctx.sink.l2_snapshot(ctx.spec.venue, symbol, book.tick_size, bids, asks, now_ms());
    (ctx.wake)();
}

/// Depth feed thread body (matches each venue's `spawn_with` shape; `interval` is unused for
/// depth). The shared [`vike_bridge_core::depth`] driver runs the connect/seed/read-loop/publish/
/// backoff lifecycle; this family supplies its PROTOCOL as three closures: REST-seed the managed
/// book (post-connect, so no diff is missed in the fetch window — the tick is inferred once from the
/// dense snapshot and reused), fold each `@depth@100ms` diff under the U/u rule, and publish. A
/// sequence gap surfaces as [`BookOp::Gap`] → the driver does a throttled reconnect + re-seed (so a
/// persistent gap can't hammer the snapshot endpoint). Stays SILENT on the status string.
///
/// `fetch_snapshot` is the venue's own REST depth fetch, already bound to the host it should hit —
/// the one part that isn't shared, because each venue resolves that host differently.
///
/// **Splits `.P` like the kline and trades feeds do.** It did not, and that was a live data bug.
///
/// This lane hardcoded the SPOT host and passed the RAW core symbol, so a perp subscription built
/// `wss://stream.binance.com:9443/ws/btcusdt.p@depth@100ms` — a stream no venue resolves. It failed
/// in the worst possible shape: the socket CONNECTS, the REST seed is rejected, the empty fallback
/// book's `last_seq = 0` trips [`BookOp::Gap`] on the first diff, and the driver reconnects and
/// re-seeds forever — publishing one EMPTY book per cycle. `vike-recorder` stored those as
/// `kind=depth` rows (`price 0, size 0, tick 0.01`) for hours with nothing logged anywhere.
///
/// Both halves must move together, which is why `fetch_snapshot` takes the flag rather than a
/// pre-bound host: the WS host comes from `urls.ws(is_perp)`, and the REST seed has to hit the
/// matching host AND path or it 404s against a host that does not serve that route.
pub fn depth_main(
    symbol: String,
    _interval: String,
    ctx: FeedCtx,
    fetch_snapshot: impl Fn(&str, bool) -> Result<DepthSnapshot, Box<dyn std::error::Error>>,
) {
    let venue = ctx.spec.venue;
    // TWO names, deliberately: `api_sym` is the WIRE form (`.P` stripped) that builds the URL and
    // the REST seed, while `sym` stays the SERIES label the sink is keyed on — a perp must not land
    // under its spot twin's key. Collapsing them is a real mistake, caught live: the depth series
    // wrote to `symbol=BTCUSDT` while its trade sibling wrote `symbol=BTCUSDT.P`.
    let (api_sym, is_perp) = vike_catalog::split_perp(&symbol);
    let sym = symbol.as_str();
    let url = depth_ws_url(ctx.spec.urls.ws(is_perp), api_sym);
    let seed = || {
        // On a transient snapshot failure DON'T return None: the driver reads None as "WS-seeded,
        // wait for the snapshot frame" (correct for Bybit/OKX) and would then Ignore every diff
        // forever with no reconnect — the DOM would stay blank until the socket drops (~24h). Seed an
        // EMPTY book instead: its last_seq=0 makes the first @depth diff trip BookOp::Gap, so the
        // driver backs off, reconnects, and re-seeds (retrying the snapshot) — the same self-heal the
        // HFT twin (family::depth's pump) uses. A real snapshot replaces the book (and its tick) next
        // time.
        match fetch_snapshot(api_sym, is_perp) {
            Ok((seq, bids, asks)) => {
                let tick = infer_tick_size(&bids, &asks);
                let mut b = L2Book::new(tick);
                b.apply_snapshot(seq, &bids, &asks);
                Some(b)
            }
            // Empty book with a placeholder tick (replaced on the next successful seed); last_seq=0
            // forces the Gap→resync above.
            Err(_) => Some(L2Book::new(DEPTH_SEED_FALLBACK_TICK)),
        }
    };
    let decode = |txt: &str, book: &mut Option<L2Book>| -> BookOp {
        let Some(b) = book.as_mut() else { return BookOp::Ignored }; // seed always returns Some here
        let Ok(ev) = serde_json::from_str::<serde_json::Value>(txt) else { return BookOp::Ignored };
        match apply_depth_event(b, &ev) {
            // §B data-freshness: the depthUpdate frame's top-level `E` is the venue EVENT-TIME
            // (epoch-ms) the driver clocks staleness off (0 if absent → receive-time fallback).
            DepthOutcome::Applied => {
                BookOp::Updated(ev.get("E").and_then(serde_json::Value::as_i64).unwrap_or(0))
            }
            DepthOutcome::Gap => BookOp::Gap,
            DepthOutcome::Stale | DepthOutcome::Ignored => BookOp::Ignored,
        }
    };
    let publish = |b: &L2Book| publish_book(b, sym, &ctx);
    // Net-hardening §B: map each driver-neutral `HealthEvent` onto the sink's machine-readable
    // disclosure vocabulary so a DOM consumer sees a dead/stale book, not a silently frozen ladder.
    // The depth driver owns the `StreamHealth` that dedups transport gaps across reconnects (one
    // outage → one GapStart/Live pair) now; the venue just maps + discloses.
    let on_health = |ev: HealthEvent| {
        ctx.sink.stream_status(venue, sym, "depth", health_to_stream_status(ev));
    };
    // The raw @depth stream needs no app-level keepalive (server pings are answered).
    run_depth_feed(
        &url,
        None,
        None,
        seed,
        decode,
        publish,
        on_health,
        &ctx.stop,
        READ_TIMEOUT,
        DEPTH_IDLE_THRESHOLD,
        DEPTH_FRESHNESS_THRESHOLD,
        Some(DEPTH_RESEED_INTERVAL),
        &now_ms,
        DEPTH_BACKOFF,
    );
}

#[cfg(test)]
mod mark_tests {
    use super::{MarkFrame, decode_mark_frame, mark_ws_url};

    /// A real-shaped `markPriceUpdate` frame decodes to the exact `p` bits + the event time `E`.
    #[test]
    fn mark_price_update_decodes_px_and_event_time() {
        let frame = serde_json::json!({
            "e": "markPriceUpdate",
            "E": 1_562_305_380_000_i64,
            "s": "BTCUSDT",
            "p": "11794.15000000",
            "i": "11784.62659091",
            "P": "11784.25641265",
            "r": "0.00038167",
            "T": 1_562_306_400_000_i64
        })
        .to_string();
        let m = decode_mark_frame(&frame).unwrap().expect("a mark frame");
        assert_eq!(m.px.to_bits(), 11794.15_f64.to_bits(), "the MARK price `p`, not the index");
        assert_eq!(m.ts, 1_562_305_380_000);
        assert_eq!(m, MarkFrame { px: 11794.15, ts: 1_562_305_380_000 });
    }

    /// Non-mark frames and dead marks are skipped in place; a malformed decimal is the session
    /// error (the kline lane's two-tier rule).
    #[test]
    fn non_mark_and_dead_frames_are_skipped_malformed_is_fatal() {
        // a kline frame on the same socket shape is not a mark frame
        let kline = r#"{"e":"kline","E":1,"k":{}}"#;
        assert!(decode_mark_frame(kline).unwrap().is_none());
        // not JSON at all (keepalives)
        assert!(decode_mark_frame("ping").unwrap().is_none());
        // a dead (zero) mark is dropped, not emitted
        let dead = r#"{"e":"markPriceUpdate","E":1,"p":"0.00000000"}"#;
        assert!(decode_mark_frame(dead).unwrap().is_none());
        // a malformed decimal drops the session so it reconnects
        let bad = r#"{"e":"markPriceUpdate","E":1,"p":"not-a-number"}"#;
        assert!(decode_mark_frame(bad).is_err());
    }

    /// URL grammar: lowercase symbol in the path, the documented `@markPrice@1s` suffix, perp host.
    #[test]
    fn mark_url_targets_the_1s_stream() {
        assert_eq!(
            mark_ws_url("wss://fstream.binance.com", "BTCUSDT"),
            "wss://fstream.binance.com/ws/btcusdt@markPrice@1s"
        );
    }
}
