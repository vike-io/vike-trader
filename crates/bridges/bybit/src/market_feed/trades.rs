//! The trades lane: the `publicTrade` decoder, subscribe frame and live-only feed body.

use serde_json::Value;
use vike_bridge_core::feed_frame::FeedFrame;
use vike_bridge_core::market_pump::{FrameOutcome, SessionStatus, run_market_feed};
use vike_bridge_core::pump_spec::subscribe_only_pump_opts;
use vike_model::{TradeTick, now_ms};

use super::klines::subscribe_envelope;
use super::{FeedCtx, LIVE_STATUS, VENUE, feed_route, ws_host};

// --- Trades feed (live prints) ---------------------------------------------------------------
// Bybit V5 `publicTrade.<SYMBOL>` channel: executed prints (no snapshot/seed — a live-only tape,
// unlike the `kline.*`/`orderbook.*` channels which each have a REST/WS seed counterpart). Same
// subscribe shape as the kline feed (`{"op":"subscribe",...}`) and the SAME spot-vs-linear split: a
// `.P`-suffixed perp symbol strips to the exchange symbol for the `publicTrade.<SYMBOL>` topic and
// routes to `PUBLIC_WS_LINEAR`, while spot stays on `PUBLIC_WS_SPOT` (see `perp_split`). Feeds
// `vike_model::TradeTick` through `LiveDataSink::trade`, the input the app's client-side
// tick/volume-bar aggregation reads. The Bybit twin of okx's `market_feed` trades section, and shaped
// identically to `crate::market_data::decode_trade` (the HFT-track twin), so both tracks' ticks match.
//
// **Perp re-labeling.** The linear-perp wire `s` is the plain exchange symbol (`BTCUSDT`, no `.P`),
// but the app's `TradeStore` keys the tape on `tick.symbol` (NOT the sink `symbol` arg) — so every
// parsed tick's `symbol` is overwritten to the `.P` series label (`relabel_series`) before emit, or a
// perp's prints would collide with its spot twin's key. Spot (`series_symbol == wire s`) is a no-op.
//
// Wire shape: `{"topic":"publicTrade.BTCUSDT","type":"snapshot","data":[{"T":<ms ts>,"s":"BTCUSDT",
// "S":"Buy"|"Sell","v":"<size>","p":"<price>","L":"...","i":"...","BT":false}]}` — a push may carry
// more than one print per frame. Field mapping (mirrors binance's `@aggTrade` / okx's `trades`
// mappers so every venue's ticks are shaped identically):
//
// | wire key | -> | `TradeTick` field |
// |----------|----|--------------------|
// | `p`      |    | `price` — decimal STRING -> f64 |
// | `v`      |    | `size` — decimal STRING -> f64 |
// | `T`      |    | `ts` — epoch-ms, a JSON NUMBER (unlike okx's decimal-string ts) -> i64 |
// | `S`      |    | `is_buyer_maker` — Bybit's `S` is the TAKER/aggressor side; Binance's `m` |
// |          |    | convention is `true` exactly when the BUYER was the MAKER (i.e. the |
// |          |    | taker/aggressor SOLD) — so `S=="Sell"` -> `true`, `S=="Buy"` -> `false`. |
// |          |    | (Identical to `crate::market_data::decode_trade`'s `S == "Sell"` rule.) |
// | `s`      |    | `symbol` — read off the per-row wire field. |
// | `i`/`L`  |    | (trade-id / tick-direction — not carried; `vike_model::TradeTick` has neither) |
//
// `local_ts` is stamped by the pump at receive time (0 from the pure mapper — deterministic/
// testable, same "0 = not stamped" convention the binance/okx mappers document).
//
// **Non-finite/non-positive guard:** a decoded price/size that fails to parse, is non-finite, or is
// `<= 0.0` is rejected in place (mirrors okx's/binance's `is_valid_trade` — a downstream volume fold
// would spin on `+Inf` or corrupt on a zero/garbage size).

/// The shared non-finite/non-positive guard (mirrors okx's/`vike_binance::trades`'s `is_valid_trade`).
fn is_valid_trade(price: f64, size: f64) -> bool {
    price.is_finite() && price > 0.0 && size.is_finite() && size > 0.0
}

/// One `publicTrade` channel data row -> a `TradeTick` (`local_ts` left at the "not stamped"
/// sentinel `0` — the pump stamps machine receive time just before each `sink.trade` emit, same
/// convention as the binance/okx mappers). `None` on a missing/unparseable field, a
/// non-finite/non-positive price or size, or an unrecognized `S` side.
fn row_to_trade_tick(row: &Value) -> Option<TradeTick> {
    let symbol = row.get("s").and_then(Value::as_str)?;
    let price = row.get("p").and_then(Value::as_str)?.parse::<f64>().ok()?;
    let size = row.get("v").and_then(Value::as_str)?.parse::<f64>().ok()?;
    if !is_valid_trade(price, size) {
        return None;
    }
    // Bybit sends `T` as a JSON number (epoch-ms), unlike okx's decimal-string `ts`.
    let ts = row.get("T").and_then(Value::as_i64)?;
    let is_buyer_maker = match row.get("S").and_then(Value::as_str)? {
        "Sell" => true, // taker SOLD -> the buyer was the MAKER (Binance `m` convention)
        "Buy" => false,
        _ => return None,
    };
    Some(TradeTick { ts, local_ts: 0, price, size, is_buyer_maker, symbol: symbol.to_string() })
}

/// PURE: one Bybit `publicTrade` channel WS push -> zero or more `TradeTick`s (fixture-tested, no
/// socket). Anything that is not a `publicTrade`-topic data push — another topic, a subscribe
/// ack/reject envelope (which carries `op`, not `topic`), or a frame with no/empty `data` array —
/// yields an empty vec; a malformed/unparseable individual row is skipped in place rather than
/// failing the whole push (mirrors okx's `parse_trades` per-element tolerance).
pub fn parse_trades(payload: &Value) -> Vec<TradeTick> {
    let topic = payload.get("topic").and_then(Value::as_str);
    if !topic.is_some_and(|t| t.starts_with("publicTrade")) {
        return Vec::new();
    }
    let Some(rows) = payload.get("data").and_then(Value::as_array) else {
        return Vec::new();
    };
    rows.iter().filter_map(row_to_trade_tick).collect()
}

/// Re-label every parsed tick's `symbol` to the sink/core `series_symbol` (the `.P` key for a perp).
/// The linear-perp wire `s` is the plain exchange symbol (`BTCUSDT`, no `.P`), but the app's
/// `TradeStore` keys the tape on `tick.symbol` — so a perp's prints must carry the `.P` series key or
/// they collide with the spot twin. For SPOT `series_symbol == wire s`, so this is byte-identical (a
/// no-op overwrite of the same string). Mirrors the kline feed's "emit under `series_symbol`" rule.
pub(super) fn relabel_series(mut ticks: Vec<TradeTick>, series_symbol: &str) -> Vec<TradeTick> {
    for t in &mut ticks {
        t.symbol = series_symbol.to_string();
    }
    ticks
}

/// The subscribe frame for the public `publicTrade` channel on `symbol` — same
/// `{"op":"subscribe","args":[...]}` shape as [`subscribe_frame`] (klines), just the `publicTrade`
/// topic with no interval code.
///
/// [`subscribe_frame`]: super::klines::subscribe_frame
pub fn trades_subscribe_frame(symbol: &str) -> String {
    serde_json::json!({
        "op": "subscribe",
        "args": [format!("publicTrade.{symbol}")]
    })
    .to_string()
}

/// [`route_frame`]'s trades-pump twin: classifies ONE raw WS text frame for [`trades_main`].
/// The subscribe OUTCOME envelope is checked first ([`subscribe_envelope`]). Otherwise
/// [`parse_trades`] decodes the push; an empty result (non-trades topic, or every row malformed) is
/// [`FeedFrame::Ignored`].
///
/// [`route_frame`]: super::klines::route_frame
fn route_trades_frame(text: &str) -> FeedFrame<Vec<TradeTick>> {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return FeedFrame::Ignored; // bare-text "ping"/"pong" keepalive is not JSON
    };
    if let Some(envelope) = subscribe_envelope(&v) {
        return envelope;
    }
    let ticks = parse_trades(&v);
    if ticks.is_empty() { FeedFrame::Ignored } else { FeedFrame::Data(ticks) }
}

/// `subscribe_trades`'s thread body — the trades twin of [`feed_main`], on the same shared
/// [`run_market_feed`] driver (dedup A6): identical subscribe-replay, br7 ack-watchdog, stop-poll,
/// and 30×100 ms backoff lifecycle, scoped down to prints (no closed/forming bar routing, no
/// mark-tick). Live-only: unlike [`feed_main`]/[`depth_main`], there is no REST warmup/seed here —
/// Bybit's `publicTrade` channel has no historical REST counterpart threaded into this feed, and a
/// tick/volume-bar builder needs no seed to start folding live prints.
///
/// `series_symbol` is the catalog/sink label (`.P`-suffixed for a perp — the `TradeStore` key);
/// `api_symbol` is the `.P`-stripped exchange symbol used for the `publicTrade` subscribe topic, and
/// `is_perp` picks the spot-vs-linear host. Spot passes `series_symbol == api_symbol`, `is_perp =
/// false`, so this is byte-identical to the pre-perp behavior for spot. Emitted ticks are re-labeled
/// to `series_symbol` via [`relabel_series`], because the linear wire `s` is the plain `BTCUSDT` and
/// the `TradeStore` keys on `tick.symbol`.
///
/// [`feed_main`]: super::klines::feed_main
/// [`depth_main`]: super::depth::depth_main
pub(super) fn trades_main(series_symbol: String, ctx: FeedCtx) {
    // Same shape as `feed_main`'s route resolution, and for the same reasons — see its comment.
    let (api_symbol, book) = match feed_route(&series_symbol) {
        Ok(v) => v,
        Err(e) => {
            ctx.set_status(format!("{series_symbol} trades: {e}"));
            return;
        }
    };
    let host = ws_host(book);
    let sub = trades_subscribe_frame(&api_symbol);
    run_market_feed(
        host,
        &subscribe_only_pump_opts(VENUE, &sub),
        &ctx.stop,
        &now_ms,
        |txt| {
            route_trades_frame(txt).outcome(|ticks| {
                // Re-label to the `.P` series key BEFORE emit — the `TradeStore` keys on
                // `tick.symbol`, and a linear perp's wire `s` is the plain `BTCUSDT`. Spot is a
                // no-op (series_symbol == wire s), so it stays byte-identical.
                for mut tick in relabel_series(ticks, &series_symbol) {
                    tick.local_ts = now_ms(); // machine receive time (dual-timestamp capture)
                    ctx.sink.trade(VENUE, &series_symbol, tick);
                }
                (ctx.wake)();
                FrameOutcome::Confirm // data confirms the br7 subscribe handshake
            })
        },
        || {}, // no dataless-tick judgment on this lane
        |s| match s {
            // Replaces the unconditional spawn-time `"LIVE · Bybit trades {series_symbol}"` this
            // function used to write before opening a socket — the MIRROR IMAGE of the kline
            // latch: a claim that the lane is live made before a connection exists, which on a
            // dead venue reads healthy forever. The claim is now made when the venue has actually
            // ACKed, and spells [`LIVE_STATUS`] rather than a per-lane text.
            SessionStatus::Live => ctx.set_status(LIVE_STATUS.into()),
            SessionStatus::Error(e) => {
                ctx.set_status(format!("{series_symbol} trades ws error (reconnecting): {e}"))
            }
        },
    );
}
