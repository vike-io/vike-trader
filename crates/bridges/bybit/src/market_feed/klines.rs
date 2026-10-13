//! The kline lane: frame classifier, subscribe frame and the REST-seeded feed-thread body.

use serde::Deserialize;
use serde_json::Value;
use vike_bridge_core::feed_frame::{FeedFrame, KlineBar};
use vike_bridge_core::klines::kline_to_bar;
use vike_bridge_core::market_pump::{
    FrameOutcome, MarketPumpOpts, MarketStream, SessionStatus, connect_market_stream,
    run_market_feed_on,
};
use vike_bridge_core::pump_spec::subscribe_only_pump_opts;
use vike_marketdata::Bar;
use vike_model::now_ms;

use super::{FeedCtx, LIVE_STATUS, SEED_LIMIT, VENUE, feed_route, ws_host};
use crate::data::{fetch_klines_latest, interval_code};

/// One Bybit `kline` datum. Named fields, so the wire order (`open,close,high,low`) is irrelevant;
/// `start` is the bar-open ms and `confirm` is Bybit's CLOSED marker. o/h/l/c/v are decimal strings
/// preserved to their exact f64 bits on parse.
#[derive(Deserialize)]
struct KlineData {
    start: i64,
    open: String,
    high: String,
    low: String,
    close: String,
    volume: String,
    confirm: bool,
}

#[derive(Deserialize)]
struct KlineMsg {
    topic: String,
    data: Vec<KlineData>,
}

/// The subscribe OUTCOME envelope every lane on this socket shares (net-hardening br7), classified
/// BEFORE a lane's own decode: a frame carrying an `op` is never data. `{op:"subscribe",
/// success:true}` is [`FeedFrame::Ack`]; `success:false` an attributable [`FeedFrame::Error`] carrying
/// Bybit's `ret_msg` (it was silently dropped); any other op (a `ping`/`pong` keepalive reply) is
/// [`FeedFrame::Ignored`]. `None`: no `op`, so the lane decodes the frame as data.
pub(super) fn subscribe_envelope<T>(v: &Value) -> Option<FeedFrame<T>> {
    let op = v.get("op").and_then(Value::as_str)?;
    if op != "subscribe" {
        return Some(FeedFrame::Ignored);
    }
    Some(if v.get("success").and_then(Value::as_bool).unwrap_or(false) {
        FeedFrame::Ack
    } else {
        let msg = v.get("ret_msg").and_then(Value::as_str).unwrap_or("(no message)");
        FeedFrame::Error(format!("Bybit subscribe rejected: {msg}"))
    })
}

/// One kline datum → a [`Bar`] via the shared binance `kline_to_bar`, so bars are the SAME shape
/// across venues. `None` if any required price string fails to parse; volume defaults to 0.0.
fn data_to_bar(k: &KlineData) -> Option<Bar> {
    Some(kline_to_bar(
        k.start,
        k.open.parse::<f64>().ok()?,
        k.high.parse::<f64>().ok()?,
        k.low.parse::<f64>().ok()?,
        k.close.parse::<f64>().ok()?,
        k.volume.parse::<f64>().unwrap_or(0.0),
    ))
}

/// Route ONE Bybit WS text frame → a classified kline frame. Pure — testable without a socket. The
/// subscribe OUTCOME envelope is classified FIRST ([`subscribe_envelope`]). Otherwise a `kline.*`
/// frame's first datum maps to a [`Bar`] in a [`KlineBar`] whose `closed` is the venue's `confirm`.
/// Non-JSON keepalives and non-kline topics → [`FeedFrame::Ignored`].
pub fn route_frame(text: &str) -> FeedFrame<KlineBar> {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return FeedFrame::Ignored; // bare-text "ping"/"pong" keepalive is not JSON
    };
    if let Some(envelope) = subscribe_envelope(&v) {
        return envelope;
    }
    // A kline data frame: {topic, data:[kline]} (the non-`op` JSON path).
    let Ok(msg) = serde_json::from_value::<KlineMsg>(v) else {
        return FeedFrame::Ignored;
    };
    if !msg.topic.starts_with("kline.") {
        return FeedFrame::Ignored;
    }
    let Some(k) = msg.data.first() else {
        return FeedFrame::Ignored;
    };
    let Some(bar) = data_to_bar(k) else {
        return FeedFrame::Ignored;
    };
    FeedFrame::Data(KlineBar { bar, closed: k.confirm })
}

/// The subscribe frame: the kline stream `kline.<code>.<SYMBOL>` (`code` = Bybit's interval code).
pub fn subscribe_frame(code: &str, symbol: &str) -> String {
    serde_json::json!({
        "op": "subscribe",
        "args": [format!("kline.{code}.{symbol}")]
    })
    .to_string()
}

/// `series_symbol` is the catalog/sink label (`.P`-suffixed for a perp — the core key); `api_symbol`
/// is the `.P`-stripped exchange symbol used for the REST seed + WS subscribe topic (IDENTICAL shape
/// on all three categories — Bybit does not encode category in the topic, only the socket URL). Spot
/// subscriptions pass `series_symbol == api_symbol` and `Category::Spot`, so this is byte-identical
/// to the pre-perp behavior for spot. `book` picks BOTH the seed's REST category and the socket
/// ([`ws_host`]) — one value, so the warmup and the stream cannot reach different books.
///
/// The WS session/reconnect lifecycle rides the shared [`run_market_feed`] driver (dedup A6);
/// each decoded frame folds through the fixture-tested [`route_frame`] inside the `on_text`
/// closure — a data frame emits under the SERIES symbol (last price → the conflated mark cache,
/// then the lossless `close_bar` or conflating `forming_bar` lane) and confirms the br7 subscribe
/// handshake; an ack confirms it dataless; a venue REJECT is surfaced attributably
/// ([`FrameOutcome::Fatal`], never silently dropped).
///
/// [`run_market_feed`]: vike_bridge_core::market_pump::run_market_feed
pub(super) fn feed_main(series_symbol: String, interval: String, ctx: FeedCtx) {
    let key = format!("{series_symbol}@{interval}");
    // Interval code up-front: an unsupported interval is a permanent config error, so report it and
    // stop (no reconnect could ever fix it).
    let code = match interval_code(&interval) {
        Ok(c) => c.to_string(),
        Err(e) => {
            ctx.set_status(format!("{key}: {e}"));
            return;
        }
    };
    // ...and the ROUTE, on the same terms and for the same reason: a symbol whose book cannot be
    // decided is a permanent config error on this subscription, not something a reconnect fixes. It
    // is resolved HERE rather than at the spawn site deliberately — the first call per process
    // reads the venue's instrument listings, and doing that on the caller's thread would stall the
    // picker; this thread already blocks on a REST seed three lines down. The refusal lands in the
    // status string, which is what an operator actually sees.
    let (api_symbol, book) = match feed_route(&series_symbol) {
        Ok(v) => v,
        Err(e) => {
            ctx.set_status(format!("{key}: {e}"));
            return;
        }
    };
    // REST warmup: newest N klines; Bybit serves the in-progress candle as the last one — seed the
    // closed prefix, route the forming tail through the market lane (mirrors binance).
    //
    // ⚠ **"Once per feed, NOT per session" is the rule for the SEED, and it used to be the rule for
    // the STATUS too.** That second half was the 42-hour the CI box latch: the healthy write below fired
    // once, here, and the pump's only other writer was the session-error hook — so one blip wrote a
    // `Degraded`-reading string and no later session could rewrite it, suppressing this venue's
    // reconcile leg 2,516 times while four sockets stayed ESTABLISHED. The seed rule is unchanged
    // (a reconnect must not re-seed — the pre-driver behaviour, and re-seeding would double-emit
    // closed bars); the STATUS is now disclosed per SESSION by `feed_body`'s `SessionStatus::Live`
    // arm, and the write here covers only the window before the first frame arrives.
    match fetch_klines_latest(&api_symbol, &interval, SEED_LIMIT, book) {
        Ok(mut bars) => {
            let forming = if bars.len() > 1 { bars.pop() } else { None };
            ctx.sink.seed_bars(VENUE, &series_symbol, &interval, bars);
            if let Some(f) = forming {
                ctx.sink.bar_close_tick(VENUE, &series_symbol, f.close, f.ts);
                ctx.sink.forming_bar(VENUE, &series_symbol, &interval, f);
            }
            ctx.set_status(LIVE_STATUS.into());
        }
        Err(e) => ctx.set_status(format!("{key} seed error: {e}")),
    }
    let host = ws_host(book);
    let sub = subscribe_frame(&code, &api_symbol);
    let opts = subscribe_only_pump_opts(VENUE, &sub);
    feed_body(&series_symbol, &interval, &opts, &ctx, || {
        connect_market_stream(host, opts.read_timeout, opts.connect_timeout)
    });
}

/// [`feed_main`]'s SESSION half: everything from the dial onwards, with the dial itself handed to
/// the caller.
///
/// ⚠ **The split exists so the reconnect path can be TESTED**, and the defect it was cut for is
/// why that matters. Bybit's kline lane wrote its healthy status exactly once, before the pump
/// ever ran, and the pump's only status writer was the session-ERROR hook — so one transient blip
/// latched a `Degraded`-reading string that no later successful session ever rewrote. On the CI box
/// that suppressed the venue's reconcile leg once a minute for 42 hours while four sockets stayed
/// ESTABLISHED and ~2,100 events a minute flowed in. Nothing in this crate could reproduce it,
/// because [`feed_main`] dials a real socket and the closures that own the status string were
/// unreachable without one. `tests/offline/market_feed_scripted.rs` drives THESE closures — the
/// real [`route_frame`], the real hooks, a real [`FeedCtx`] — over a scripted stream pair.
///
/// Same shape as `crates/bridges/binance/src/family/trades.rs`'s `run_trades_feed`, which has
/// taken its connect closure since it was written; this is that seam applied one lane over rather
/// than a new idea.
pub fn feed_body<S: MarketStream>(
    series_symbol: &str,
    interval: &str,
    opts: &MarketPumpOpts<'_>,
    ctx: &FeedCtx,
    connect: impl FnMut() -> Result<S, String>,
) {
    let key = format!("{series_symbol}@{interval}");
    run_market_feed_on(
        connect,
        opts,
        &ctx.stop,
        &now_ms,
        // A subscribe ACK confirms the handshake and a REJECT is the attributable session error
        // (`FeedFrame::outcome`); only a bar reaches the closure.
        |txt| {
            route_frame(txt).outcome(|KlineBar { bar, closed }| {
                // last price -> the core's bar-close cache (conflated), same as binance — under the
                // SERIES symbol so it lands on the same key the chart/core reads; data also confirms
                // br7. `bar_close_tick`, NOT `mark_tick`: a candle close is not the venue mark
                // (mark-slot semantics; the real mark rides the perp `tickers` pump, `mark_main`).
                ctx.sink.bar_close_tick(VENUE, series_symbol, bar.close, bar.ts);
                if closed {
                    // bar CLOSED — lossless lane (a missed close = a series hole)
                    ctx.sink.close_bar(VENUE, series_symbol, interval, bar);
                } else {
                    ctx.sink.forming_bar(VENUE, series_symbol, interval, bar);
                }
                (ctx.wake)();
                FrameOutcome::Confirm
            })
        },
        || {}, // no dataless-tick judgment on this lane (the polymarket freshness knob)
        |s| match s {
            // THE FIX. The seed-time write above is the first session's healthy state before any
            // frame arrives; this is every LATER session's, and its absence is the 42-hour latch.
            // Identical text, so `set_status`'s dedup emits nothing on the common repeat — the
            // write is free until it actually says something new.
            SessionStatus::Live => ctx.set_status(LIVE_STATUS.into()),
            SessionStatus::Error(e) => {
                ctx.set_status(format!("{key} ws error (reconnecting): {e}"))
            }
        },
    );
}
