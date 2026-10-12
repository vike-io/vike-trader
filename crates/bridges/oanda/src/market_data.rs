//! OANDA pricing-stream decode — pure frame decoders for the LIVE market-data lane
//! (`GET /v3/accounts/{id}/pricing/stream`), the market-data twin of [`crate::stream`]'s
//! execution-transactions decode. One JSON object per chunked-HTTP line; with
//! `Accept-Datetime-Format: UNIX` every `time` is an epoch-seconds string (same wire shape
//! [`crate::data::parse_candles`] and [`crate::stream::decode_transaction_events`] parse).
//!
//! Two frame types matter (everything else — e.g. the `PRICE`-shaped snapshot OANDA replays on
//! connect is just a `PRICE`):
//! - `PRICE`: `bids`/`asks` are TOP-OF-BOOK price ladders ordered best-first (`price` a decimal
//!   string, `liquidity` the venue's available units at that price). This is NOT an L2 depth
//!   lane: the ladder is a full, unsequenced snapshot per frame with no delta grammar and no seq
//!   id, which is why the market-data conformance harness's book invariants stay deferred for
//!   this venue (see `crates/vike-bridge-core/tests/market_data_conformance.rs`'s `DEFERRED`
//!   row). Decoded to the L1 [`QuoteTick`] only.
//! - `HEARTBEAT`: ~every 5 s. Carries no data — it is the TRANSPORT-liveness signal the feed's
//!   idle/staleness watchdog hooks onto ([`crate::market_feed`]).
//!
//! The pump owns everything impure (transport, reconnect, health, sink emission).

use serde_json::Value;
use vike_model::{Bar, QuoteTick};

/// One decoded pricing-stream line. `Other` covers malformed frames and any type this decoder
/// does not carry (a one-sided `PRICE` — no bid or no ask — also lands here: no L1 quote can be
/// derived from half a market, the same "a one-sided book derives no quote" rule the conformance
/// harness pins for derived-L1 venues).
#[derive(Debug, Clone, PartialEq)]
pub enum PricingFrame {
    /// A two-sided `PRICE` frame, decoded to top-of-book. `symbol` carries the venue instrument
    /// (`EUR_USD`); the pump relabels it with the subscription's own series and stamps
    /// `local_ts` (dual-timestamp capture) — this decoder is pure and stamps nothing.
    Quote(QuoteTick),
    /// A `HEARTBEAT` frame — transport liveness only, `ts` in epoch ms (0 when unparseable).
    Heartbeat { ts: i64 },
    /// Anything else — skipped by the pump (neither data nor liveness).
    Other,
}

/// Epoch-seconds-string `time` → epoch ms (0 when absent/unparseable) — the same idiom
/// [`crate::stream`]'s transaction decode uses.
fn ts_ms(v: &Value) -> i64 {
    v.get("time")
        .and_then(Value::as_str)
        .and_then(|t| t.parse::<f64>().ok())
        .map_or(0, |secs| (secs * 1000.0) as i64)
}

/// Best (first) level of a `bids`/`asks` ladder → `(price, liquidity)`. OANDA orders each ladder
/// best-first, so `[0]` IS top-of-book. `liquidity` is tolerated absent (0.0) — the price is the
/// quote; the size is advisory.
fn top_of(v: &Value, side: &str) -> Option<(f64, f64)> {
    let level = v.get(side)?.as_array()?.first()?;
    let px = level.get("price")?.as_str()?.parse::<f64>().ok()?;
    let size = level.get("liquidity").and_then(Value::as_f64).unwrap_or(0.0);
    Some((px, size))
}

/// PURE: one parsed pricing-stream line → its [`PricingFrame`].
pub fn decode_pricing_frame(v: &Value) -> PricingFrame {
    match v.get("type").and_then(Value::as_str) {
        Some("PRICE") => match (top_of(v, "bids"), top_of(v, "asks")) {
            (Some((bid, bid_size)), Some((ask, ask_size))) => PricingFrame::Quote(QuoteTick {
                ts: ts_ms(v),
                local_ts: 0, // the pump stamps receive time
                bid,
                ask,
                bid_size,
                ask_size,
                symbol: v.get("instrument").and_then(Value::as_str).unwrap_or_default().to_string(),
            }),
            _ => PricingFrame::Other, // one-sided market: no L1 quote derivable
        },
        Some("HEARTBEAT") => PricingFrame::Heartbeat { ts: ts_ms(v) },
        _ => PricingFrame::Other,
    }
}

/// PURE: the still-FORMING last candle of an OANDA `candles` response, if any —
/// the conflating-lane complement of [`crate::data::parse_candles`] (which keeps only
/// `complete` candles, the lossless lane). `None` when the newest candle is already complete.
pub fn parse_forming_candle(v: &Value) -> Option<Bar> {
    let candle = v.get("candles")?.as_array()?.last()?;
    if candle.get("complete").and_then(Value::as_bool).unwrap_or(false) {
        return None;
    }
    crate::data::bar_from_candle(candle)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A wire-faithful practice-stream `PRICE` frame (UNIX datetime format).
    fn price_frame() -> Value {
        serde_json::from_str(
            r#"{
                "type": "PRICE", "time": "1478012400.500000000", "instrument": "EUR_USD",
                "bids": [{"price": "1.09000", "liquidity": 10000000},
                         {"price": "1.08990", "liquidity": 20000000}],
                "asks": [{"price": "1.09010", "liquidity": 10000000},
                         {"price": "1.09020", "liquidity": 20000000}],
                "closeoutBid": "1.08980", "closeoutAsk": "1.09030",
                "status": "tradeable", "tradeable": true
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn price_frame_decodes_top_of_book() {
        let PricingFrame::Quote(q) = decode_pricing_frame(&price_frame()) else {
            panic!("expected a quote");
        };
        assert_eq!(q.ts, 1_478_012_400_500); // epoch-seconds string → ms
        assert_eq!(q.local_ts, 0, "the pure decoder stamps no receive time");
        assert_eq!(q.bid, 1.09); // bids[0] — the ladder is best-first
        assert_eq!(q.ask, 1.0901);
        assert_eq!(q.bid_size, 10_000_000.0);
        assert_eq!(q.ask_size, 10_000_000.0);
        assert_eq!(q.symbol, "EUR_USD");
    }

    #[test]
    fn heartbeat_frame_decodes_with_ts() {
        let v = serde_json::json!({"type": "HEARTBEAT", "time": "1478012401.000000000"});
        assert_eq!(decode_pricing_frame(&v), PricingFrame::Heartbeat { ts: 1_478_012_401_000 });
    }

    #[test]
    fn one_sided_price_is_not_a_quote() {
        let mut v = price_frame();
        v["asks"] = serde_json::json!([]);
        assert_eq!(decode_pricing_frame(&v), PricingFrame::Other);
        let mut v = price_frame();
        v.as_object_mut().unwrap().remove("bids");
        assert_eq!(decode_pricing_frame(&v), PricingFrame::Other);
    }

    #[test]
    fn unknown_and_malformed_frames_are_other() {
        assert_eq!(
            decode_pricing_frame(&serde_json::json!({"type": "UNKNOWN_KIND"})),
            PricingFrame::Other
        );
        assert_eq!(decode_pricing_frame(&serde_json::json!({})), PricingFrame::Other);
        // a PRICE whose ladder levels are malformed (price not a string) derives nothing
        let v = serde_json::json!({
            "type": "PRICE", "time": "1", "instrument": "EUR_USD",
            "bids": [{"price": 1.09, "liquidity": 1}], "asks": [{"price": 1.0901, "liquidity": 1}]
        });
        assert_eq!(decode_pricing_frame(&v), PricingFrame::Other);
    }

    #[test]
    fn forming_candle_is_the_incomplete_tail_only() {
        let v: Value = serde_json::from_str(
            r#"{
                "instrument": "EUR_USD", "granularity": "H1",
                "candles": [
                    {"complete": true,  "volume": 100, "time": "1478012400.000000000",
                     "mid": {"o": "1.09000", "h": "1.09500", "l": "1.08900", "c": "1.09300"}},
                    {"complete": false, "volume": 5,   "time": "1478016000.000000000",
                     "mid": {"o": "1.09300", "h": "1.09400", "l": "1.09200", "c": "1.09350"}}
                ]
            }"#,
        )
        .unwrap();
        let forming = parse_forming_candle(&v).expect("forming candle present");
        assert_eq!(forming.ts, 1_478_016_000_000);
        assert_eq!(forming.close, 1.0935);
        assert_eq!(forming.volume, 5.0);

        // newest candle complete → no forming bar
        let v: Value = serde_json::from_str(
            r#"{"candles": [
                {"complete": true, "volume": 100, "time": "1478012400.000000000",
                 "mid": {"o": "1.09000", "h": "1.09500", "l": "1.08900", "c": "1.09300"}}
            ]}"#,
        )
        .unwrap();
        assert!(parse_forming_candle(&v).is_none());
        assert!(parse_forming_candle(&serde_json::json!({"candles": []})).is_none());
    }
}
