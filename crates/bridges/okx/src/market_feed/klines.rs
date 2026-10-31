//! The candle lane: the `candle<bar>` frame classifier, subscribe frame and feed-thread body.

use serde_json::Value;
use vike_bridge_core::feed_frame::{FeedFrame, KlineBar};
use vike_bridge_core::klines::kline_to_bar;
use vike_bridge_core::market_pump::{FrameOutcome, SessionStatus, run_market_feed};
use vike_bridge_core::pump_spec::subscribe_only_pump_opts;
use vike_marketdata::Bar;
use vike_model::now_ms;

use super::{FeedCtx, LIVE_STATUS, VENUE, subscribe_envelope};
use crate::data::{bar_code, fetch_klines_latest};
use crate::market_data::BUSINESS_WS;

const SEED_LIMIT: usize = 100; // OKX history-candles caps a single request at 100 rows

/// The `i`-th element of an OKX candle row as a `&str`, if present and a string.
fn s(r: &[Value], i: usize) -> Option<&str> {
    r.get(i).and_then(Value::as_str)
}

/// One OKX candle row (`[ts,o,h,l,c,vol,…,confirm]`, decimal strings) → `(Bar, is_closed)`. `None`
/// if a required field is missing/unparseable. `confirm == "1"` ⇒ the candle is FINAL; volume
/// defaults to 0.0 on a parse miss. Uses the shared binance `kline_to_bar` so bars match every venue.
fn row_to_bar(r: &[Value]) -> Option<(Bar, bool)> {
    let ts = s(r, 0)?.parse::<i64>().ok()?;
    let o = s(r, 1)?.parse::<f64>().ok()?;
    let h = s(r, 2)?.parse::<f64>().ok()?;
    let l = s(r, 3)?.parse::<f64>().ok()?;
    let c = s(r, 4)?.parse::<f64>().ok()?;
    let v = s(r, 5).and_then(|x| x.parse::<f64>().ok()).unwrap_or(0.0);
    let confirmed = s(r, 8) == Some("1"); // OKX confirm flag: "1"=closed, "0"=forming
    Some((kline_to_bar(ts, o, h, l, c, v), confirmed))
}

/// Route ONE OKX WS text frame → a classified kline frame. Pure — testable without a socket. The
/// subscribe OUTCOME envelope is classified FIRST ([`subscribe_envelope`]). Otherwise only a
/// `candle*` channel data frame carries a kline, as a [`KlineBar`] whose `closed` is `confirm=="1"`.
/// Other channels and the bare-text `pong` keepalive → [`FeedFrame::Ignored`].
pub fn route_frame(text: &str) -> FeedFrame<KlineBar> {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return FeedFrame::Ignored; // e.g. the bare-text "pong" keepalive is not JSON
    };
    if let Some(envelope) = subscribe_envelope(&v) {
        return envelope;
    }
    let channel = v.get("arg").and_then(|a| a.get("channel")).and_then(Value::as_str);
    if !channel.is_some_and(|c| c.starts_with("candle")) {
        return FeedFrame::Ignored; // non-candle channels
    }
    let Some(row) =
        v.get("data").and_then(Value::as_array).and_then(|d| d.first()).and_then(Value::as_array)
    else {
        return FeedFrame::Ignored;
    };
    let Some((bar, closed)) = row_to_bar(row) else {
        return FeedFrame::Ignored;
    };
    FeedFrame::Data(KlineBar { bar, closed })
}

/// The subscribe frame: the kline stream `candle<bar>` on `inst` (`bar` = OKX's bar code).
pub fn subscribe_frame(bar: &str, inst: &str) -> String {
    serde_json::json!({
        "op": "subscribe",
        "args": [{"channel": format!("candle{bar}"), "instId": inst}]
    })
    .to_string()
}

/// The kline feed's thread body. `interval` is the ORIGINAL binance-style string ("1m") kept as
/// the series key so series line up across venues; the OKX bar code drives the `candle<bar>`
/// subscribe.
///
/// The WS session/reconnect lifecycle rides the shared [`run_market_feed`] driver (dedup A6);
/// each decoded frame folds through the fixture-tested [`route_frame`] inside the `on_text`
/// closure — a data frame emits under `inst` (last price → the conflated mark cache, then the
/// lossless `close_bar` or conflating `forming_bar` lane) and confirms the br7 subscribe
/// handshake; an ack confirms it dataless; a venue REJECT is surfaced attributably
/// ([`FrameOutcome::Fatal`], never silently dropped).
pub(super) fn feed_main(inst: String, interval: String, ctx: FeedCtx) {
    let key = format!("{inst}@{interval}");
    // Bar code up-front: an unsupported interval is a permanent config error, so report it and stop.
    let bar = match bar_code(&interval) {
        Ok(b) => b.to_string(),
        Err(e) => {
            ctx.set_status(format!("{key}: {e}"));
            return;
        }
    };
    // REST warmup: newest N CLOSED candles. history-candles is closed-only, so there is NO forming
    // tail to pop — the WS `candle` channel delivers the in-progress bar. Once per feed, NOT per
    // session — a reconnect does not re-seed (the pre-driver behavior).
    match fetch_klines_latest(&inst, &interval, SEED_LIMIT) {
        Ok(bars) => {
            ctx.sink.seed_bars(VENUE, &inst, &interval, bars);
            ctx.set_status(LIVE_STATUS.into());
        }
        Err(e) => ctx.set_status(format!("{key} seed error: {e}")),
    }
    let sub = subscribe_frame(&bar, &inst);
    // ⚠ The BUSINESS host: the public one rejects `candle<bar>` ("wrong URL or channel"), which this driver reads
    // as a session error and retries for ever, so a kline lane there is silent and never delivers a bar.
    run_market_feed(
        BUSINESS_WS,
        &subscribe_only_pump_opts(VENUE, &sub),
        &ctx.stop,
        &now_ms,
        // A subscribe ACK confirms the handshake and a REJECT is the attributable session error
        // (`FeedFrame::outcome`); only a candle reaches the closure.
        |txt| {
            route_frame(txt).outcome(|KlineBar { bar: candle, closed }| {
                // last price -> the core's bar-close cache (conflated), same as binance; data also
                // confirms the br7 subscribe handshake. `bar_close_tick`, NOT `mark_tick`: a candle
                // close is not the venue mark (mark-slot semantics; the real mark rides the SWAP
                // `mark-price` pump, `mark_main`).
                ctx.sink.bar_close_tick(VENUE, &inst, candle.close, candle.ts);
                if closed {
                    // bar CLOSED — lossless lane (a missed close = a series hole)
                    ctx.sink.close_bar(VENUE, &inst, &interval, candle);
                } else {
                    ctx.sink.forming_bar(VENUE, &inst, &interval, candle);
                }
                (ctx.wake)();
                FrameOutcome::Confirm
            })
        },
        || {}, // no dataless-tick judgment on this lane (the polymarket freshness knob)
        |s| match s {
            // THE FIX: the seed-time write above covers only the FIRST session; this covers every
            // later one. Identical text, so a repeat says nothing.
            SessionStatus::Live => ctx.set_status(LIVE_STATUS.into()),
            SessionStatus::Error(e) => {
                ctx.set_status(format!("{key} ws error (reconnecting): {e}"))
            }
        },
    );
}
