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
//! ── THE ROW TYPE: both channels emit `vike_marketdata::OptionTicker` ───────────────────────────
//! Feed values have ONE home (`docs/decisions/0126-every-instrument-class-is-a-typed-contract-in-vike-model-and-feed-values-live-in-vike-marketdata.md`):
//! this module defines no row type of its own. Both decoders fill a [`OptionTicker`] with exactly the
//! fields their channel carried and leave every other field `None`:
//! - a `markprice.options` row sets `mark` and `mark_iv`. It is an `OptionTicker`, not a
//!   `vike_marketdata::MarkPrice`, because the channel's load-bearing second value is the implied
//!   vol and `MarkPrice` has no field that can hold it; `OptionTicker`'s own doc names this channel.
//!   The two channels stay apart by WHICH stream delivered them (two spawn functions, two folds),
//!   never by inspecting a row.
//! - a `ticker` row sets every field the frame carries and `OptionTicker` can hold: bid/ask with
//!   sizes, `last`, `mark`, the three implied vols, the five greeks, `open_interest`, `volume`,
//!   `underlying_price` and `index_price` ([`parse_ticker`]'s field map). It is DECODE AND STORE:
//!   `vike-options`' grid fold reads only `bid`/`ask`/`mark`/`mark_iv` and re-derives its own
//!   greeks, so the extra fields change no grid value.
//! - `symbol` is the VENUE'S OWN instrument name exactly as the channel sends it
//!   (`"BTC-31JUL26-67000-C"`), NOT the `"SYMBOL.VENUE"` form `OptionTicker`'s field doc describes:
//!   the only consumer (`vike-options`' option-grid folds) looks it up in the option index, which
//!   is keyed by that bare name.
//! - `ts` is the row's wire `timestamp` (epoch ms, the venue's publish time) when the frame carries
//!   one, else the RECEIVE time the caller passes (`recv_ms`). Every captured `markprice.options` row
//!   carries `timestamp`; the captured `ticker` fixture does not, so there `ts` is the receive time
//!   unless the live frame adds one.
//! - implied vol is a DECIMAL fraction in both: the ticker's percent `mark_iv` is divided by 100
//!   HERE, in [`parse_ticker`], and nowhere else.
//!
//! WIRE UNITS (captured verbatim from mainnet, 2026-07-26 — they DIFFER from both the REST
//! book-summary and the `ticker` channel, so the fixture test below PINS them):
//! - `params.data` is an ARRAY of `{instrument_name, mark_price, iv, timestamp}`.
//! - `iv` is a DECIMAL fraction (`0.3706` == 37.06%) — NOT the percent the REST `mark_iv` carries —
//!   so it is copied VERBATIM into `mark_iv` (no ÷100).
//! - `mark_price`'s UNIT depends on the index book, exactly like the REST book-summary: the
//!   `btc_usd`/`eth_usd` books are coin-settled (mark in COIN units, scaled to USD by the underlying
//!   price at the fold), while the USDC altcoin book — `sol_usdc` (SOL) — quotes premiums ALREADY in
//!   USD (passed through unscaled). The fold picks the scale from the option's settlement currency,
//!   looked up by name in `vike-options`' option index (never parsed from the name), so a streamed
//!   mark and a re-polled mark agree byte-for-byte.
//!
//! ── TICKER (Part B): per-instrument BID/ASK ────────────────────────────────────────────────────
//! A SECOND public feed in this file streams live BEST BID/ASK for a FOCUSED set of instruments via
//! [`ticker_channel`] (`ticker.{instrument}.100ms`). Where `markprice.options.{index}` pushes mark+IV
//! chain-wide, `ticker` pushes the full per-instrument row — bid/ask with sizes, last, mark, three
//! IVs, greeks, OI, volume, underlying and index — so the VISIBLE strikes gain a live bid/ask
//! overlay the coarse REST poll can't match.
//! Identical lifecycle (the shared `market_pump` driver, reconnect == resubscribe) and keyless-public
//! transport; only the wire SHAPE + UNITS differ (captured verbatim from mainnet 2026-07-26, pinned
//! by [`parse_ticker`]'s fixture test):
//! - `params.data` is a SINGLE OBJECT (one instrument), NOT the array `markprice.options` sends.
//! - `best_bid_price`/`best_ask_price`/`mark_price`/`last_price` are COIN units (scaled to USD
//!   downstream by the chain spot; the USDC-settled options pass through unscaled — the same
//!   settlement-currency rule as the markprice fold, agreeing with the REST path's per-book rule).
//!   The greeks are NOT coin: `vega`/`theta`/`rho` are USD-scale on the coin book too
//!   ([`parse_ticker`]).
//! - `mark_iv` (and `bid_iv`/`ask_iv`) is a PERCENT on the wire (`66.33` == 66.33%, the REST
//!   `mark_iv` convention) — which DIFFERS from `markprice.options`'s already-decimal `iv`;
//!   [`parse_ticker`] divides each by 100, so both channels hand the folds a decimal.
//! - `volume` is nested under `stats.volume` and the greeks under `greeks`; `open_interest`/
//!   `underlying_price` are top-level.
//!
//! DISABLED-by-absence: nothing here runs unless a caller spawns the feed. The REST chain path is
//! untouched; this only ADDS liveness on top of it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::Value;

use vike_bridge_core::json::{get_f64_opt, json_int};
use vike_bridge_core::market_pump::{
    FrameOutcome, MarketPumpOpts, PumpBackoff, SessionStatus, run_market_feed,
};
use vike_bridge_core::pump_spec::CONNECT_10S;
use vike_marketdata::OptionTicker;
use vike_model::now_ms;

/// Mainnet public WS endpoint (keyless) — the same host the REST chain + order transport use.
pub const MAINNET_WS: &str = "wss://www.deribit.com/ws/api/v2";

/// Silent-stall watchdog window: `markprice.options` publishes continuously while options trade, so
/// a healthy feed never trips it; a dead socket behind an open TCP conn reconnects within this long.
const IDLE_SECS: u64 = 30;
/// Reconnect backoff after a session fault — the crypto venues' classic 3s.
const BACKOFF_SECS: u64 = 3;

/// The row's wire `timestamp` (epoch ms, string-or-number), else the receive time `recv_ms` — the
/// one `ts` rule both decoders share (module doc).
fn ts_or_recv(row: &Value, recv_ms: i64) -> i64 {
    row.get("timestamp").and_then(json_int).unwrap_or(recv_ms)
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

/// Pure decode: a `markprice.options.*` subscription frame → one [`OptionTicker`] per row, or `None`
/// for any other frame (wrong method/channel, a non-array `data`, an ack/response). A malformed ROW
/// (missing any of the three load-bearing fields `instrument_name`/`mark_price`/`iv`) is SKIPPED, not
/// fatal — a single bad row never drops the whole frame. Each row sets `symbol` (the venue name),
/// `mark` (COIN units, or USD on the USDC book — scaled at the fold), `mark_iv` (the wire's decimal
/// `iv`, verbatim) and `ts` (the row's `timestamp`, else `recv_ms`); every other field is `None`.
pub fn parse_markprice_options(frame: &Value, recv_ms: i64) -> Option<Vec<OptionTicker>> {
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
            let mut t = OptionTicker::new(ts_or_recv(row, recv_ms), name);
            t.mark = Some(mark);
            t.mark_iv = Some(iv); // already a decimal on this channel: verbatim, no ÷100
            out.push(t);
        }
    }
    Some(out)
}

/// Handle one inbound TEXT frame received at `recv_ms`: decode → hand the rows to `on_rows` →
/// confirm the subscription. [`FrameOutcome::Confirm`] for any valid markprice frame (proves the sub
/// is live to the driver, even an empty delta), [`FrameOutcome::Ignore`] for anything else (the
/// subscribe ack, a heartbeat, junk).
fn on_markprice_frame(
    txt: &str,
    recv_ms: i64,
    on_rows: &mut impl FnMut(Vec<OptionTicker>),
) -> FrameOutcome {
    let Ok(frame) = serde_json::from_str::<Value>(txt) else {
        return FrameOutcome::Ignore;
    };
    let Some(rows) = parse_markprice_options(&frame, recv_ms) else {
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
/// each decoded batch of [`OptionTicker`]s (mark + implied vol only, [`parse_markprice_options`]) to
/// `on_rows`. Runs the shared reconnect/subscribe/idle-watchdog lifecycle. Keyless — nothing here is
/// auth-gated.
pub fn spawn_deribit_markprice_options_feed(
    ws_url: String,
    indexes: Vec<String>,
    mut on_rows: impl FnMut(Vec<OptionTicker>) + Send + 'static,
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
                connect_timeout: Some(CONNECT_10S), // a black-holed dial can't pin the thread
            };
            // `&now_ms` (the vike_model fn item) coerces to `&dyn Fn() -> i64` — the same call shape
            // dvol/polymarket use; no wrapping closure (which would trip `redundant_closure`).
            run_market_feed(
                &ws_url,
                &opts,
                &stop_thread,
                &now_ms,
                |txt| on_markprice_frame(txt, now_ms(), &mut on_rows),
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

/// The per-instrument ticker channel, e.g. `("BTC-27JUL26-58000-C", "100ms")` →
/// `"ticker.BTC-27JUL26-58000-C.100ms"`. Instrument ids are case-SENSITIVE (uppercase) — unlike the
/// lowercased index in [`markprice_options_channel`], the name is passed through verbatim.
pub fn ticker_channel(instrument: &str, interval: &str) -> String {
    format!("ticker.{instrument}.{interval}")
}

/// Pure decode: a `ticker.*` subscription frame → its single [`OptionTicker`], or `None` for any
/// other frame (wrong method/channel, an ack/response, a `data` missing `instrument_name`). Unlike
/// [`parse_markprice_options`], `params.data` is ONE OBJECT (one instrument), not an array — the
/// single load-bearing field is `instrument_name` (the grid join key, copied into `symbol`); every
/// other field is optional and decoded via [`get_f64_opt`] (absent stays `None`, never a fabricated
/// `0.0`). The fold scales the premium fields by the option's settlement currency and leaves an
/// absent field's prior grid value as its own policy says.
///
/// Field map — every key of the frame that [`OptionTicker`] has a field for:
/// - `best_bid_price` → `bid`, `best_ask_price` → `ask`, `mark_price` → `mark`, `last_price` →
///   `last` (all COIN units, or USD on the USDC book; copied verbatim, the fold scales);
/// - `best_bid_amount` → `bid_size`, `best_ask_amount` → `ask_size` (contracts, verbatim);
/// - `mark_iv` / `bid_iv` / `ask_iv` → the same-named fields ÷ 100: the wire PERCENT becomes the
///   type's decimal fraction, the ONE place this channel's IV unit is converted;
/// - `greeks.{delta, gamma, vega, theta, rho}` → the five greek fields, VERBATIM in Deribit's own
///   convention: `delta` the plain Black-Scholes delta (`0.99992` on the captured deep-ITM call),
///   `vega` per one vol POINT (1 %), `theta` per
///   calendar day, `rho` per one percentage point of rate. ⚠ Their CURRENCY is not the premium's:
///   on a coin-settled book the premium is in coin, but `vega`/`theta`/`rho` are USD-scale (the
///   captured frame's deep-ITM call has a 0.1036 BTC premium and `theta` −0.2863/day, which only a
///   USD figure can be). No conversion happens here; no consumer reads them yet;
/// - `open_interest` (contracts), `stats.volume` → `volume` (nested on the wire),
///   `underlying_price` (informational: the fold scales by the chain's REST-seeded spot, not this
///   per-row price), `index_price`, `timestamp` → `ts`, else `recv_ms`.
///
/// A key the frame lacks (or sends as `null`, as `last_price` is before the first trade) stays
/// `None`; a PRESENT value, `0.0` included, is copied as sent. Keys `OptionTicker` cannot hold
/// (`stats.high`/`low`/`price_change`/`volume_usd`, `interest_rate`, `settlement_price`,
/// `min_price`/`max_price`, `state`, `underlying_index`, `estimated_delivery_price`) are not read.
pub fn parse_ticker(frame: &Value, recv_ms: i64) -> Option<OptionTicker> {
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
    let mut t = OptionTicker::new(ts_or_recv(data, recv_ms), name);
    t.bid = get_f64_opt(data, "best_bid_price");
    t.ask = get_f64_opt(data, "best_ask_price");
    t.bid_size = get_f64_opt(data, "best_bid_amount");
    t.ask_size = get_f64_opt(data, "best_ask_amount");
    t.last = get_f64_opt(data, "last_price");
    t.mark = get_f64_opt(data, "mark_price");
    // The wire carries a PERCENT (`66.33`); the type carries a decimal fraction (`0.6633`).
    let pct = |key: &str| get_f64_opt(data, key).map(|p| p / 100.0);
    t.mark_iv = pct("mark_iv");
    t.bid_iv = pct("bid_iv");
    t.ask_iv = pct("ask_iv");
    // The greeks ride under a nested `greeks` object, in Deribit's convention (fn doc), verbatim.
    if let Some(g) = data.get("greeks") {
        t.delta = get_f64_opt(g, "delta");
        t.gamma = get_f64_opt(g, "gamma");
        t.vega = get_f64_opt(g, "vega");
        t.theta = get_f64_opt(g, "theta");
        t.rho = get_f64_opt(g, "rho");
    }
    t.open_interest = get_f64_opt(data, "open_interest");
    // volume rides under a nested `stats` object, not a top-level key.
    t.volume = data.get("stats").and_then(|s| get_f64_opt(s, "volume"));
    t.underlying_price = get_f64_opt(data, "underlying_price");
    t.index_price = get_f64_opt(data, "index_price");
    Some(t)
}

/// Handle one inbound TEXT frame received at `recv_ms`: decode → hand the row to `on_row` →
/// confirm. [`FrameOutcome::Confirm`] for any valid ticker frame (proves the sub is live to the
/// driver), [`FrameOutcome::Ignore`] for anything else (the subscribe ack, a heartbeat, junk).
fn on_ticker_frame(txt: &str, recv_ms: i64, on_row: &mut impl FnMut(OptionTicker)) -> FrameOutcome {
    let Ok(frame) = serde_json::from_str::<Value>(txt) else {
        return FrameOutcome::Ignore;
    };
    let Some(row) = parse_ticker(&frame, recv_ms) else {
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
/// accepts a bulk channel list), and hand each decoded [`OptionTicker`] ([`parse_ticker`]: IV
/// already a decimal) to `on_row`. Runs the shared
/// reconnect/subscribe/idle-watchdog lifecycle (reconnect == resubscribe). Keyless — nothing here is
/// auth-gated. The caller passes the focused visible-strike set (see
/// `vike_options::front_expiry_focus_instruments`).
pub fn spawn_deribit_ticker_feed(
    ws_url: String,
    instruments: Vec<String>,
    mut on_row: impl FnMut(OptionTicker) + Send + 'static,
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
                connect_timeout: Some(CONNECT_10S), // a black-holed dial can't pin the thread
            };
            run_market_feed(
                &ws_url,
                &opts,
                &stop_thread,
                &now_ms,
                |txt| on_ticker_frame(txt, now_ms(), &mut on_row),
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
