//! The pure half: one market-data WS frame in, the [`DataMsg`]s it carries out; no I/O.

use vike_marketdata::{Bar, QuoteTick, TradeTick};

use super::parse_alpaca_ts_ms;

/// One decoded Alpaca market-data WS payload, mapped onto vike's wire types. Control frames
/// (`success`/`error`/`subscription` acks) decode to nothing — see [`decode_ws_message`].
#[derive(Debug, Clone)]
pub enum DataMsg {
    /// A closed 1-minute bar (`T:"b"`). Alpaca publishes no intrabar/forming bar frame, so
    /// `closed` is always `true` today; the field exists so a future forming-bar source (if
    /// Alpaca ever adds one) doesn't need a shape change here.
    Bar { symbol: String, interval: String, bar: Bar, closed: bool },
    /// An L1 quote update (`T:"q"`).
    Quote { symbol: String, q: QuoteTick },
    /// An executed-trade print (`T:"t"`).
    Trade { symbol: String, t: TradeTick },
}

/// Decode one Alpaca market-data WS frame — a JSON array of message objects — into the mapped
/// [`DataMsg`]s it carries. Pure (no I/O); fixture-tested in `tests/mapper_data.rs`. Unknown/
/// malformed elements (including the `success`/`error`/`subscription` control acks, and a
/// non-JSON-array frame) are silently skipped rather than erroring — a single bad element in a
/// batch must not drop the rest. The `error` acks are not lost: the crate-private
/// `decode_control_errors` reads them for the connection thread's log.
///
/// `venue_prefix` names which feed this frame came from (`'c'` crypto, `'s'` equities) — reserved
/// for a future symbol-namespacing need (e.g. disambiguating a ticker that exists on both feeds);
/// today's mapping doesn't depend on it, since Alpaca's `T`/`S` fields are already unambiguous
/// within one connection.
pub fn decode_ws_message(venue_prefix: char, text: &str) -> Vec<DataMsg> {
    let _ = venue_prefix;
    let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(text) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(arr.len());
    for v in &arr {
        let Some(msg_type) = v.get("T").and_then(|t| t.as_str()) else { continue };
        let Some(symbol) = v.get("S").and_then(|s| s.as_str()) else { continue };
        let ts = v.get("t").and_then(|t| t.as_str()).map(parse_alpaca_ts_ms).unwrap_or(0);
        match msg_type {
            "b" => {
                let (Some(o), Some(h), Some(l), Some(c)) = (
                    v.get("o").and_then(serde_json::Value::as_f64),
                    v.get("h").and_then(serde_json::Value::as_f64),
                    v.get("l").and_then(serde_json::Value::as_f64),
                    v.get("c").and_then(serde_json::Value::as_f64),
                ) else {
                    continue;
                };
                let volume = v.get("v").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
                let bar = Bar {
                    ts,
                    open: o,
                    high: h,
                    low: l,
                    close: c,
                    volume,
                    funding: None,
                    bid: None,
                    ask: None,
                    symbol: Some(symbol.to_string()),
                };
                out.push(DataMsg::Bar {
                    symbol: symbol.to_string(),
                    interval: "1m".to_string(),
                    bar,
                    closed: true,
                });
            }
            "q" => {
                let (Some(bp), Some(ap)) = (
                    v.get("bp").and_then(serde_json::Value::as_f64),
                    v.get("ap").and_then(serde_json::Value::as_f64),
                ) else {
                    continue;
                };
                let bid_size = v.get("bs").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
                let ask_size = v.get("as").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
                let q = QuoteTick {
                    ts,
                    local_ts: 0,
                    bid: bp,
                    ask: ap,
                    bid_size,
                    ask_size,
                    symbol: symbol.to_string(),
                };
                out.push(DataMsg::Quote { symbol: symbol.to_string(), q });
            }
            "t" => {
                let Some(p) = v.get("p").and_then(serde_json::Value::as_f64) else { continue };
                let size = v.get("s").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
                let t = TradeTick {
                    ts,
                    local_ts: 0,
                    price: p,
                    size,
                    // Alpaca's trade message carries no maker/taker side flag.
                    is_buyer_maker: false,
                    symbol: symbol.to_string(),
                };
                out.push(DataMsg::Trade { symbol: symbol.to_string(), t });
            }
            // "success" / "error" / "subscription" control acks, or any other verb — ignored.
            _ => continue,
        }
    }
    out
}

/// The longest `msg` kept from a venue error frame: the text is the peer's, and a log line stays
/// bounded whatever it sends.
const MAX_ERROR_MSG_CHARS: usize = 200;

/// One `{"T":"error","code":N,"msg":"…"}` control frame the venue sent on the data socket.
///
/// [`decode_ws_message`] drops these on purpose (they carry no `S`, so they are not data). This is
/// the other half: it lets the connection thread SAY why a session is about to end, where it used
/// to log only that it ended. The server closes the socket after such a frame (a clean close,
/// code 1000, follows a `406` in [a captured session](https://forum.alpaca.markets/t/alpaca-streaming-bars-errors/4924)),
/// and the redial wait that follows is the fixed row of `vike_venues::venue_timings::ALPACA` — this
/// type changes neither. The codes this crate's own notes record (`data.rs`'s module doc, verified
/// live 2026-07-14): `406 connection limit exceeded`, `402 auth failed`, `409 insufficient
/// subscription`; the rest are logged as the venue sent them, never interpreted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ControlError {
    /// The venue's numeric code; `None` when the frame carries none or not a non-negative integer.
    pub(super) code: Option<u64>,
    /// The venue's text, cut to [`MAX_ERROR_MSG_CHARS`]; empty when the frame carries none.
    pub(super) msg: String,
}

/// The `error` control frames of one market-data WS text frame (a JSON array), in order; empty for
/// anything else. Pure (no I/O) and total over arbitrary text.
///
/// Every text frame of a live socket passes the caller, so a frame that cannot hold an error
/// element is rejected by a substring test before any JSON is parsed: a data frame costs one scan,
/// not a second parse.
pub(super) fn decode_control_errors(text: &str) -> Vec<ControlError> {
    if !text.contains("\"error\"") {
        return Vec::new();
    }
    let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(text) else {
        return Vec::new();
    };
    arr.iter()
        .filter(|v| v.get("T").and_then(serde_json::Value::as_str) == Some("error"))
        .map(|v| ControlError {
            code: v.get("code").and_then(serde_json::Value::as_u64),
            msg: v
                .get("msg")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .chars()
                .take(MAX_ERROR_MSG_CHARS)
                .collect(),
        })
        .collect()
}
