//! Deribit options MARK-PRICE streaming feed (public, keyless WS) — the LIVE twin of the REST
//! book-summary in [`crate::chain`]. Subscribes `markprice.options.{index}` (`btc_usd`/`eth_usd`),
//! which pushes the chain-wide mark price + implied vol for EVERY listed option on the index: a full
//! snapshot the instant the subscription lands, then changed-only deltas as marks move. The decoded
//! rows are handed to a caller sink (`on_rows`) that folds them onto the live option grid, so the
//! grid's IV / mark / greeks stay live BETWEEN the coarse REST snapshots — killing the 30s-poll
//! staleness the grid had when its only source was [`crate::chain::DeribitOptionsProvider`].
//!
//! Modeled 1:1 on [`crate::dvol`]: the whole connect → subscribe → read → idle-watchdog →
//! backoff-reconnect lifecycle is the shared [`vike_bridge_core::market_pump`] driver (so reconnect
//! == resubscribe by construction), reusing `configure_ws_stream`'s read-timeout + `TCP_NODELAY`
//! rather than hand-rolling a socket. Only the PROTOCOL lives here — the `public/subscribe` frame
//! and the pure per-frame decode ([`parse_markprice_options`]).
//!
//! WIRE UNITS (captured verbatim from mainnet, 2026-07-26 — they DIFFER from both the REST
//! book-summary and the `ticker` channel, so the fixture test below PINS them):
//! - `params.data` is an ARRAY of `{instrument_name, mark_price, iv, timestamp}`.
//! - `iv` is a DECIMAL fraction (`0.3706` == 37.06%) — NOT the percent the REST `mark_iv` carries —
//!   so it is stored VERBATIM downstream (no ÷100).
//! - `mark_price`'s UNIT depends on the index book, exactly like the REST book-summary: the
//!   `btc_usd`/`eth_usd` books are coin-settled (mark in COIN units, scaled to USD by the underlying
//!   price at the fold), while the USDC altcoin book — `sol_usdc` (SOL) — quotes premiums ALREADY in
//!   USD (passed through unscaled). The fold keys off [`crate::chain::is_usd_quoted`] to pick the
//!   scale, so a streamed mark and a re-polled mark agree byte-for-byte.
//!
//! ── TICKER (Part B): per-instrument BID/ASK ────────────────────────────────────────────────────
//! A SECOND public feed in this file streams live BEST BID/ASK for a FOCUSED set of instruments via
//! [`ticker_channel`] (`ticker.{instrument}.100ms`). Where `markprice.options.{index}` pushes mark+IV
//! chain-wide, `ticker` pushes the full per-instrument top-of-book — bid/ask/mark/mark_iv/OI/volume/
//! underlying — so the VISIBLE strikes gain a live bid/ask overlay the coarse REST poll can't match.
//! Identical lifecycle (the shared `market_pump` driver, reconnect == resubscribe) and keyless-public
//! transport; only the wire SHAPE + UNITS differ (captured verbatim from mainnet 2026-07-26, pinned
//! by [`parse_ticker`]'s fixture test):
//! - `params.data` is a SINGLE OBJECT (one instrument), NOT the array `markprice.options` sends.
//! - `best_bid_price`/`best_ask_price`/`mark_price` are COIN units (scaled to USD downstream by the
//!   chain spot, same [`crate::chain::is_usd_quoted`] rule as the markprice fold + the REST path).
//! - `mark_iv` is a PERCENT (`66.33` == 66.33%, the REST `mark_iv` convention → ÷100 downstream) —
//!   which DIFFERS from `markprice.options`'s already-decimal `iv`; the two folds divide accordingly.
//! - `volume` is nested under `stats.volume`; `open_interest`/`underlying_price` are top-level.
//!
//! DISABLED-by-absence: nothing here runs unless a caller spawns the feed. The REST chain path is
//! untouched; this only ADDS liveness on top of it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::Value;

use vike_bridge_core::json::get_f64_opt;
use vike_bridge_core::market_pump::{
    FrameOutcome, MarketPumpOpts, PumpBackoff, SessionStatus, run_market_feed,
};
use vike_model::now_ms;

/// Mainnet public WS endpoint (keyless) — the same host the REST chain + order transport use.
pub const MAINNET_WS: &str = "wss://www.deribit.com/ws/api/v2";

/// Silent-stall watchdog window: `markprice.options` publishes continuously while options trade, so
/// a healthy feed never trips it; a dead socket behind an open TCP conn reconnects within this long.
const IDLE_SECS: u64 = 30;
/// Bounded TCP dial so a black-holed route can't pin the feed thread past the stop flag.
const CONNECT_SECS: u64 = 10;
/// Reconnect backoff after a session fault — the crypto venues' classic 3s.
const BACKOFF_SECS: u64 = 3;

/// One decoded `markprice.options` row: the venue's mark price + implied vol for one option.
#[derive(Debug, Clone, PartialEq)]
pub struct MarkPriceRow {
    /// Verbatim venue instrument id, e.g. `"BTC-31JUL26-67000-C"` — the join key onto the grid.
    pub instrument_name: String,
    /// Mark price in COIN units (a fraction of the underlying) — scale to USD by the underlying
    /// price at the folding site.
    pub mark_price: f64,
    /// Implied vol as a DECIMAL fraction (`0.37` == 37%) — stored verbatim, no ÷100.
    pub iv: f64,
}

/// The chain-wide mark-price channel for an index, e.g. `"btc_usd"` → `"markprice.options.btc_usd"`.
pub fn markprice_options_channel(index: &str) -> String {
    format!("markprice.options.{}", index.to_ascii_lowercase())
}

/// Deribit `public/subscribe` frame for keyless public channels (the public twin of the private
/// auth subscribe; replayed verbatim each session by the driver). The `id` is a fixed sentinel — the
/// driver disarms its handshake on the first [`FrameOutcome::Confirm`], never by id-matching the ack.
fn build_public_subscribe(channels: &[String]) -> String {
    serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "public/subscribe",
        "params": {"channels": channels},
    })
    .to_string()
}

/// Pure decode: a `markprice.options.*` subscription frame → its rows, or `None` for any other frame
/// (wrong method/channel, a non-array `data`, an ack/response). A malformed ROW (missing any of the
/// three load-bearing fields) is SKIPPED, not fatal — a single bad row never drops the whole frame.
pub fn parse_markprice_options(frame: &Value) -> Option<Vec<MarkPriceRow>> {
    if frame.get("method").and_then(|m| m.as_str()) != Some("subscription") {
        return None;
    }
    let params = frame.get("params")?;
    let channel = params.get("channel").and_then(|c| c.as_str()).unwrap_or("");
    if !channel.starts_with("markprice.options.") {
        return None;
    }
    let data = params.get("data")?.as_array()?;
    let mut out = Vec::with_capacity(data.len());
    for row in data {
        let name = row.get("instrument_name").and_then(|v| v.as_str());
        let mark = get_f64_opt(row, "mark_price");
        let iv = get_f64_opt(row, "iv");
        if let (Some(name), Some(mark), Some(iv)) = (name, mark, iv) {
            out.push(MarkPriceRow { instrument_name: name.to_string(), mark_price: mark, iv });
        }
    }
    Some(out)
}

/// Handle one inbound TEXT frame: decode → hand the rows to `on_rows` → confirm the subscription.
/// [`FrameOutcome::Confirm`] for any valid markprice frame (proves the sub is live to the driver,
/// even an empty delta), [`FrameOutcome::Ignore`] for anything else (the subscribe ack, a heartbeat,
/// junk).
fn on_markprice_frame(txt: &str, on_rows: &mut impl FnMut(Vec<MarkPriceRow>)) -> FrameOutcome {
    let Ok(frame) = serde_json::from_str::<Value>(txt) else {
        return FrameOutcome::Ignore;
    };
    let Some(rows) = parse_markprice_options(&frame) else {
        return FrameOutcome::Ignore;
    };
    if !rows.is_empty() {
        on_rows(rows);
    }
    FrameOutcome::Confirm
}

/// Join handle for a spawned markprice feed — deterministic teardown (raise stop, join the thread),
/// the [`crate::dvol::DvolFeed`] twin. DROPPING the handle detaches the thread: it owns its own stop
/// clone and keeps reconnecting for the process lifetime (matching the app's other detached fetch
/// threads); call [`Self::shutdown`] for a clean stop+join.
pub struct MarkPriceFeed {
    stop: Arc<AtomicBool>,
    handle: std::thread::JoinHandle<()>,
}

impl MarkPriceFeed {
    /// Stop the feed and join its thread.
    pub fn shutdown(self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.handle.join();
    }
}

/// Spawn the persistent public `markprice.options` feed: connect `ws_url`, subscribe
/// `markprice.options.{index}` for each `indexes` entry (e.g. `["btc_usd", "eth_usd"]`), and hand
/// each decoded batch of [`MarkPriceRow`]s to `on_rows`. Runs the shared
/// reconnect/subscribe/idle-watchdog lifecycle. Keyless — nothing here is auth-gated.
pub fn spawn_deribit_markprice_options_feed(
    ws_url: String,
    indexes: Vec<String>,
    mut on_rows: impl FnMut(Vec<MarkPriceRow>) + Send + 'static,
) -> MarkPriceFeed {
    let channels: Vec<String> = indexes.iter().map(|i| markprice_options_channel(i)).collect();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = Arc::clone(&stop);
    let handle = std::thread::Builder::new()
        .name("deribit-markprice-options".to_string())
        .spawn(move || {
            let _span = tracing::info_span!("markprice_options_feed", venue = "deribit").entered();
            let sub = build_public_subscribe(&channels);
            let opts = MarketPumpOpts {
                subscribe: Some(&sub),
                keepalive: None, // continuous mark frames + tungstenite auto-pong keep it warm
                ack_timeout: None, // the idle watchdog reconnects a dead subscribe; the ack is Ignored
                idle_threshold: Some(Duration::from_secs(IDLE_SECS)),
                read_timeout: Duration::from_secs(1),
                backoff: PumpBackoff::Fixed(Duration::from_secs(BACKOFF_SECS)),
                connect_timeout: Some(Duration::from_secs(CONNECT_SECS)),
            };
            // `&now_ms` (the vike_model fn item) coerces to `&dyn Fn() -> i64` — the same call shape
            // dvol/polymarket use; no wrapping closure (which would trip `redundant_closure`).
            run_market_feed(
                &ws_url,
                &opts,
                &stop_thread,
                &now_ms,
                |txt| on_markprice_frame(txt, &mut on_rows),
                || {},
                |s| match s {
                    // No status mutex on this lane — the arm is the DECLARATION that it was
                    // classified, mirroring the `warn!` below at the same once-per-session cadence.
                    SessionStatus::Live => tracing::info!(
                        target: "vike_deribit::options_feed", "markprice feed session live"
                    ),
                    SessionStatus::Error(e) => tracing::warn!(
                        target: "vike_deribit::options_feed", error = %e, "markprice feed session error"
                    ),
                },
            );
        })
        .expect("spawn deribit markprice options feed");
    MarkPriceFeed { stop, handle }
}

// ── ticker.{instrument}.{interval} — the per-instrument BID/ASK feed (Part B) ────────────────────

/// Ticker aggregation cadence. Deribit publishes `ticker.{inst}.100ms` on a 100 ms clock (the other
/// documented rates are `.raw` = every change and `.agg2` = ~1/s); 100 ms is the freshness sweet spot
/// for a visible-strike bid/ask overlay without a raw-rate frame storm.
const TICKER_INTERVAL: &str = "100ms";

/// One decoded `ticker.{instrument}.{interval}` row: the full per-instrument top-of-book. Every
/// numeric field is `Option` (absent-stays-absent — the venue can omit `stats.volume` on some
/// instruments): [`parse_ticker`] copies present fields, [`crate::chain::is_usd_quoted`]-scaled at the
/// fold, and the fold leaves an absent field's prior grid value untouched.
#[derive(Debug, Clone, PartialEq)]
pub struct TickerRow {
    /// Verbatim venue instrument id, e.g. `"BTC-27JUL26-58000-C"` — the join key onto the grid.
    pub instrument_name: String,
    /// Best bid in COIN units (scale to USD downstream); `None` when the field is absent.
    pub best_bid: Option<f64>,
    /// Best ask in COIN units (scale to USD downstream).
    pub best_ask: Option<f64>,
    /// Mark price in COIN units (scale to USD downstream) — same unit as `markprice.options`.
    pub mark_price: Option<f64>,
    /// Implied vol as a PERCENT (`66.33` == 66.33%) — the REST `mark_iv` convention, ÷100
    /// downstream. NOTE this DIFFERS from `markprice.options`'s already-decimal `iv`.
    pub mark_iv: Option<f64>,
    /// Open interest (contracts).
    pub open_interest: Option<f64>,
    /// 24h volume — carried under `stats.volume` on the wire (nested, not a top-level key).
    pub volume: Option<f64>,
    /// Underlying index price at publish. Informational: the fold scales premiums by the chain's own
    /// REST-seeded spot (so every strike scales by ONE consistent value), not this per-row price.
    pub underlying_price: Option<f64>,
}

/// The per-instrument ticker channel, e.g. `("BTC-27JUL26-58000-C", "100ms")` →
/// `"ticker.BTC-27JUL26-58000-C.100ms"`. Instrument ids are case-SENSITIVE (uppercase) — unlike the
/// lowercased index in [`markprice_options_channel`], the name is passed through verbatim.
pub fn ticker_channel(instrument: &str, interval: &str) -> String {
    format!("ticker.{instrument}.{interval}")
}

/// Pure decode: a `ticker.*` subscription frame → its single row, or `None` for any other frame
/// (wrong method/channel, an ack/response, a `data` missing `instrument_name`). Unlike
/// [`parse_markprice_options`], `params.data` is ONE OBJECT (one instrument), not an array — the
/// single load-bearing field is `instrument_name` (the grid join key); every other field is optional
/// and decoded via [`get_f64_opt`] (absent stays `None`, never a fabricated `0.0`).
pub fn parse_ticker(frame: &Value) -> Option<TickerRow> {
    if frame.get("method").and_then(|m| m.as_str()) != Some("subscription") {
        return None;
    }
    let params = frame.get("params")?;
    let channel = params.get("channel").and_then(|c| c.as_str()).unwrap_or("");
    if !channel.starts_with("ticker.") {
        return None;
    }
    let data = params.get("data")?;
    let name = data.get("instrument_name").and_then(|v| v.as_str())?;
    Some(TickerRow {
        instrument_name: name.to_string(),
        best_bid: get_f64_opt(data, "best_bid_price"),
        best_ask: get_f64_opt(data, "best_ask_price"),
        mark_price: get_f64_opt(data, "mark_price"),
        mark_iv: get_f64_opt(data, "mark_iv"),
        open_interest: get_f64_opt(data, "open_interest"),
        // volume rides under a nested `stats` object, not a top-level key.
        volume: data.get("stats").and_then(|s| get_f64_opt(s, "volume")),
        underlying_price: get_f64_opt(data, "underlying_price"),
    })
}

/// Handle one inbound TEXT frame: decode → hand the row to `on_row` → confirm. [`FrameOutcome::Confirm`]
/// for any valid ticker frame (proves the sub is live to the driver), [`FrameOutcome::Ignore`] for
/// anything else (the subscribe ack, a heartbeat, junk).
fn on_ticker_frame(txt: &str, on_row: &mut impl FnMut(TickerRow)) -> FrameOutcome {
    let Ok(frame) = serde_json::from_str::<Value>(txt) else {
        return FrameOutcome::Ignore;
    };
    let Some(row) = parse_ticker(&frame) else {
        return FrameOutcome::Ignore;
    };
    on_row(row);
    FrameOutcome::Confirm
}

/// Join handle for a spawned ticker feed — the [`MarkPriceFeed`] twin (raise stop, join the thread).
/// DROPPING the handle detaches the thread (it owns its own stop clone and keeps reconnecting for the
/// process lifetime); call [`Self::shutdown`] for a clean stop+join.
pub struct TickerFeed {
    stop: Arc<AtomicBool>,
    handle: std::thread::JoinHandle<()>,
}

impl TickerFeed {
    /// Stop the feed and join its thread.
    pub fn shutdown(self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.handle.join();
    }
}

/// Spawn the persistent public `ticker.{instrument}.100ms` feed: connect `ws_url`, subscribe the
/// per-instrument ticker channel for each `instruments` entry (all in ONE `public/subscribe` — Deribit
/// accepts a bulk channel list), and hand each decoded [`TickerRow`] to `on_row`. Runs the shared
/// reconnect/subscribe/idle-watchdog lifecycle (reconnect == resubscribe). Keyless — nothing here is
/// auth-gated. The caller passes the focused visible-strike set (see
/// `vike_app_core::tools::front_expiry_focus_instruments`).
pub fn spawn_deribit_ticker_feed(
    ws_url: String,
    instruments: Vec<String>,
    mut on_row: impl FnMut(TickerRow) + Send + 'static,
) -> TickerFeed {
    let channels: Vec<String> =
        instruments.iter().map(|i| ticker_channel(i, TICKER_INTERVAL)).collect();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = Arc::clone(&stop);
    let handle = std::thread::Builder::new()
        .name("deribit-ticker".to_string())
        .spawn(move || {
            let _span = tracing::info_span!("ticker_feed", venue = "deribit").entered();
            let sub = build_public_subscribe(&channels);
            let opts = MarketPumpOpts {
                subscribe: Some(&sub),
                keepalive: None, // continuous ticker frames + tungstenite auto-pong keep it warm
                ack_timeout: None, // the idle watchdog reconnects a dead subscribe; the ack is Ignored
                idle_threshold: Some(Duration::from_secs(IDLE_SECS)),
                read_timeout: Duration::from_secs(1),
                backoff: PumpBackoff::Fixed(Duration::from_secs(BACKOFF_SECS)),
                connect_timeout: Some(Duration::from_secs(CONNECT_SECS)),
            };
            run_market_feed(
                &ws_url,
                &opts,
                &stop_thread,
                &now_ms,
                |txt| on_ticker_frame(txt, &mut on_row),
                || {},
                |s| match s {
                    // Same as the markprice lane: no status handle, so the arm is the declaration.
                    SessionStatus::Live => tracing::info!(
                        target: "vike_deribit::options_feed", "ticker feed session live"
                    ),
                    SessionStatus::Error(e) => tracing::warn!(
                        target: "vike_deribit::options_feed", error = %e, "ticker feed session error"
                    ),
                },
            );
        })
        .expect("spawn deribit ticker feed");
    TickerFeed { stop, handle }
}

#[path = "options_feed_tests.rs"]
#[cfg(test)]
mod options_feed_tests;

#[path = "ticker_tests.rs"]
#[cfg(test)]
mod ticker_tests;
