//! Live OKX V5 market data → the vike-data live seam: [`DataClientSpecOkx`], okx's half of the shared
//! data-client engine (`vike_bridge_core::data_client::ws::DataClientWebsocket`, which implements
//! [`vike_data::DataClient`]), as distinct from [`crate::feed::tick`], the L2/tick HFT feed.
//!
//! One file per lane beside this root, each holding its decoder and its plan: `klines` (candles + REST
//! warmup seed), `mark` (the mark-price channel and its funding twin), `trades` and `depth` (the
//! 400-level DOM book, on the depth driver). This root keeps the spec, the addresses it resolves once,
//! and the subscribe-outcome envelope every lane classifies first.
//!
//! The engine owns the rest: one stoppable thread per subscription (a `*-SWAP` bars subscription also
//! opens the `mark-price` channel feeding the REAL-mark verb `mark_tick`, default ON, the venue's
//! `mark_streams` row), the session driver with this venue's `pump_spec` row (subscribe replay, the
//! subscribe-ack watchdog, the keepalive cadence, the idle watchdog, the stop poll, the backoff), the status
//! words (`LIVE · OKX`), and the one place a decoded event reaches the sink. Every lane here pings with
//! [`crate::feed::tick::KEEPALIVE`]: OKX cuts a socket that pushed nothing for 30 s, which a quiet
//! instrument's candle or trades channel does, and the `pong` is what the idle watchdog hears. Every lane
//! reads OKX's `64008` service-upgrade notice ([`market_service_upgrade_notice`]): the pump's planned reconnect.
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
//! Endpoint: the BUSINESS socket ([`crate::feed::tick::PATH_WEBSOCKET_BUSINESS`] on the live `data` `business`
//! row, resolved once by [`DataClientSpecOkx::new`] like every address here); OKX serves the candle
//! channels nowhere else (the public socket every other lane here dials answers a candle subscribe
//! with a "wrong URL or channel" error). Subscribe:
//! `{"op":"subscribe","args":[{"channel":"candle<bar>","instId":"<SYMBOL>"}]}` where `<bar>` is OKX's
//! bar code ("1m","1H","1D",…) from [`crate::history::bar_code`] — the SAME map the REST candle fetcher
//! uses.

use serde_json::Value;
use vike_bridge_core::data_client::ws::{DataClientLane, DataClientLanePlan, DataClientSpec};
use vike_bridge_core::feed_frame::FeedFrame;
use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlRefusal, UrlType, VenueUrls};

use crate::feed::tick::{
    PATH_WEBSOCKET_BUSINESS, PATH_WEBSOCKET_PUBLIC, PRODUCT_BUSINESS, feed_websocket,
};

mod depth;
pub mod mark;

pub mod bar;
pub mod tick;

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

/// OKX's announced close on a market socket, the market pump's notice hook on every lane here (the public socket's
/// mark and trades, the business socket's candles): `{"event":"notice","code":"64008","msg":"The connection will soon
/// be closed for a service upgrade. Please reconnect.","connId":…}`, sent "60 seconds prior to the upgrade of the
/// WebSocket service" on every endpoint (`/ws/v5/public`, `/ws/v5/private`, `/ws/v5/business`; OKX API v5, WebSocket
/// "Notification"). The private sockets read the same frame (`crates/bridges/okx/src/execution/user_data.rs`'s
/// `service_upgrade_notice`). Any other `notice` code is not it.
pub fn market_service_upgrade_notice(text: &str) -> Option<String> {
    if !text.contains("64008") {
        return None; // a data frame is never parsed twice
    }
    let frame: Value = serde_json::from_str(text).ok()?;
    let notice = frame.get("event").and_then(Value::as_str) == Some("notice")
        && frame.get("code").and_then(Value::as_str) == Some("64008");
    notice.then(|| {
        let msg = frame.get("msg").and_then(Value::as_str).unwrap_or("");
        format!("the venue sent notice 64008: {msg}")
    })
}

/// The one subscribe frame every lane on this socket sends: `{"op":"subscribe","args":[{"channel":
/// <channel>,"instId":<inst>}]}` (`candle<bar>`, `trades`; the mark lane names two channels, `mark-price`
/// and `funding-rate`, in the same shape: `mark::mark_subscribe_frame`).
fn channel_subscribe_frame(channel: &str, instrument: &str) -> String {
    serde_json::json!({
        "op": "subscribe",
        "args": [{"channel": channel, "instId": instrument}]
    })
    .to_string()
}

/// **okx's half of the data-client engine**: the three addresses every lane dials or reads, resolved
/// ONCE from the venue's `live` public rows (a tier-less feed: a demo account's prices come from mainnet).
#[derive(Debug)]
pub struct DataClientSpecOkx {
    /// The public socket the mark, trades and depth lanes dial (`crate::feed::tick::feed_websocket`, the live
    /// `data` row plus `crate::feed::tick::PATH_WEBSOCKET_PUBLIC`).
    public_websocket: String,
    /// The BUSINESS socket the candle lane dials, the only one serving `candle<bar>`
    /// (`crate::feed::tick::PATH_WEBSOCKET_BUSINESS` on the live `data` `business` row).
    business_websocket: String,
    /// The REST base the candle lane's warmup seed reads `history-candles` on (the live `history` row).
    history_rest: String,
}

impl DataClientSpecOkx {
    /// The spec over okx's address rows `urls`.
    ///
    /// # Errors
    /// The lookup's refusal: no client is built, and the caller says why.
    pub fn new(urls: &VenueUrls) -> Result<Self, UrlRefusal> {
        Ok(DataClientSpecOkx {
            public_websocket: feed_websocket(urls, "", PATH_WEBSOCKET_PUBLIC)?,
            business_websocket: feed_websocket(urls, PRODUCT_BUSINESS, PATH_WEBSOCKET_BUSINESS)?,
            history_rest: urls
                .url(UrlType::Live, UrlChannel::History, UrlProtocol::Rest, "")?
                .to_string(),
        })
    }
}

impl DataClientSpec for DataClientSpecOkx {
    const VENUE: &'static str = VENUE;
    const DISPLAY: &'static str = "OKX";
    /// This venue's mark wire is documented, so its mark stream ships ON; `venue.okx.mark_streams = 0`
    /// turns it off (decision 0095).
    const MARK_STREAMS_DEFAULT_ON: bool = true;
    /// `history-candles` holds closed candles only: no forming tail to pop.
    const SEED_LAST_BAR_FORMING: bool = false;

    /// A `*-SWAP` perpetual pairs the `mark-price` stream.
    ///
    /// SCOPE, deliberately narrow: OKX also publishes `mark-price` for DATED FUTURES
    /// (`BTC-USD-240329`) and for OPTIONS, and the decoder would handle those rows unchanged — the
    /// channel shape is identical. They are excluded because `*-SWAP` is the only contract class the
    /// rest of this bridge charts and values today; a dated-future or option position therefore keeps
    /// valuing through the resolver's bar-close rung. Widening this predicate needs no decoder change,
    /// only an instId classifier that does not mistake a SPOT pair (`BTC-USDT`, which has NO mark
    /// price) for a dated contract.
    ///
    /// `symbol` is a USER name (decision 0290): a perpetual is the one whose kept row's instId is a `*-SWAP`.
    fn pairs_mark(&self, symbol: &str) -> bool {
        crate::instrument::inst_id_of(symbol).is_ok_and(|inst| {
            crate::instrument::InstrumentType::of_inst_id(&inst)
                == Some(crate::instrument::InstrumentType::Swap)
        })
    }

    /// `symbol` is a USER name (decision 0290: `BTCUSDT.P`): every lane subscribes and seeds its KEPT row's instId
    /// (`BTC-USDT-SWAP`) and emits under `symbol`; a name with no row is refused in words, no lane is dialled.
    fn plan(
        &self,
        lane: DataClientLane,
        symbol: &str,
        interval: &str,
    ) -> Result<DataClientLanePlan, String> {
        let instrument = &crate::instrument::inst_id_of(symbol)?;
        match lane {
            DataClientLane::Bars => {
                bar::bars_plan(instrument, interval, &self.business_websocket, &self.history_rest)
            }
            DataClientLane::Mark => Ok(mark::mark_plan(symbol, instrument, &self.public_websocket)),
            DataClientLane::Trades => {
                Ok(tick::trades::trades_plan(symbol, instrument, &self.public_websocket))
            }
            DataClientLane::Depth => Ok(depth::depth_main(instrument, &self.public_websocket)),
            // The caps row refuses both before a thread exists (`live_data.quotes`/`book` are false).
            DataClientLane::Quotes | DataClientLane::Book => {
                Err(format!("okx serves no live {} lane", lane.word(interval)))
            }
        }
    }
}

#[path = "feed_tests.rs"]
#[cfg(test)]
mod feed_tests;
