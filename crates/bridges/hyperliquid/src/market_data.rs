//! Hyperliquid market-data normalizers — one WS frame → `Bar` / quote / trade prints / L2 snapshot.
//! Pure, fixture-tested (NO socket); the decode half of [`crate::market_feed`], the HL twin of
//! bybit's/okx's `market_data` normalizers. Ports the wire shapes in
//! `docs/research/2026-07-16-hyperliquid-adapters/README.md` §7/§9.
//!
//! Each function takes the WHOLE decoded frame (`{"channel":<c>,"data":<d>}`) and self-checks the
//! `channel`, so a mis-routed frame yields `None`/empty rather than a wrong decode (same convention
//! as bybit's `parse_trades` / okx's `route_frame`). HL wire facts that shape these:
//! - **candle** (`channel:"candle"`, `data` is ONE object): `t`=open ms (a JSON NUMBER), `o/h/l/c/v`
//!   are decimal STRINGS. There is **no `confirm`/closed flag** — closed-vs-forming is decided by the
//!   feed via open-time rollover (see [`crate::market_feed`]), so this only produces the `Bar`.
//! - **bbo** (`channel:"bbo"`): `data.bbo` is `[bidLevel, askLevel]`, each `{px,sz,n}` or `null`; a
//!   one-sided top is NOT a quote (`None`).
//! - **trades** (`channel:"trades"`, `data` is an ARRAY): each `{coin,side,px,sz,time,…}`. `side` is
//!   the AGGRESSOR/taker side — **`"A"`=sell, `"B"`=buy** — mapped to `is_buyer_maker` the same way
//!   every other venue is (`"A"` ⇒ the taker SOLD ⇒ the buyer was the MAKER ⇒ `true`).
//! - **l2Book** (`channel:"l2Book"`): `data.levels` is `[bids, asks]` (`levels[0]`=bids,
//!   `levels[1]`=asks), each level `{px,sz,n}`. HL pushes a **FULL snapshot every frame — no deltas,
//!   no removals, no sequence** (§7, cross-validated 4×); `time` is the only ordering key. The feed
//!   REPLACES the book each frame, never merges.

use serde_json::Value;
use vike_model::{Bar, Level, QuoteTick, TradeTick};

/// A decimal-string field → f64 (HL sends prices/sizes as strings so the JSON and the signed bytes
/// can never diverge — §3). `None` if absent or unparseable.
fn fstr(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_str).and_then(|s| s.parse::<f64>().ok())
}

/// The frame's `channel` string, if present.
fn channel(frame: &Value) -> Option<&str> {
    frame.get("channel").and_then(Value::as_str)
}

/// One `{px,sz,n}` order-book level → `(price, size)`. `None` for a `null` side or a level missing
/// `px`/`sz` (both are decimal strings on the wire).
fn level_px_sz(lvl: &Value) -> Option<Level> {
    Some((fstr(lvl.get("px"))?, fstr(lvl.get("sz"))?))
}

/// One side of an l2Book (`[{px,sz,n},…]`) → `Vec<Level>`, skipping any malformed level. An absent
/// / non-array side yields an empty vec.
fn side_levels(side: Option<&Value>) -> Vec<Level> {
    side.and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(level_px_sz).collect())
        .unwrap_or_default()
}

/// The shared non-finite/non-positive trade guard (mirrors bybit's/okx's `is_valid_trade` — a
/// downstream volume fold would spin on `+Inf` or corrupt on a zero/garbage size).
fn is_valid_trade(price: f64, size: f64) -> bool {
    price.is_finite() && price > 0.0 && size.is_finite() && size > 0.0
}

/// One HL `candle` frame → a [`Bar`] (open-time `t` as `ts`). `None` unless it is a `candle`-channel
/// frame with a `data` object and all required price fields present. Volume defaults to `0.0` on a
/// parse miss; `funding`/`bid`/`ask`/`symbol` are `None`/empty (OHLCV bar, same as every venue).
/// Closed-vs-forming is NOT decided here (HL sends no confirm flag) — the feed does it via open-time
/// rollover.
pub fn candle_to_bar(frame: &Value) -> Option<Bar> {
    if channel(frame) != Some("candle") {
        return None;
    }
    candle_data_to_bar(frame.get("data")?)
}

/// The `data` object of a candle frame → a [`Bar`]. Split out from [`candle_to_bar`] so a REST
/// `candleSnapshot` row (identical field names) could reuse it verbatim.
pub fn candle_data_to_bar(d: &Value) -> Option<Bar> {
    Some(Bar {
        ts: d.get("t").and_then(Value::as_i64)?, // open millis — a JSON number, not a string
        open: fstr(d.get("o"))?,
        high: fstr(d.get("h"))?,
        low: fstr(d.get("l"))?,
        close: fstr(d.get("c"))?,
        volume: fstr(d.get("v")).unwrap_or(0.0),
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    })
}

/// One HL `bbo` frame → a [`QuoteTick`] (`local_ts`/`symbol` left unset — the feed stamps machine
/// receive time and the series label just before emit, same "0 = not stamped" convention as the
/// bybit/okx mappers). `data.bbo` is `[bidLevel, askLevel]`; `None` unless BOTH sides are present
/// (a one-sided top is not a quote — mirrors bybit's `quote_from_book`).
pub fn bbo_to_quote(frame: &Value) -> Option<QuoteTick> {
    if channel(frame) != Some("bbo") {
        return None;
    }
    let d = frame.get("data")?;
    let bbo = d.get("bbo").and_then(Value::as_array)?;
    let (bid, bid_size) = level_px_sz(bbo.first()?)?;
    let (ask, ask_size) = level_px_sz(bbo.get(1)?)?;
    Some(QuoteTick {
        ts: d.get("time").and_then(Value::as_i64).unwrap_or(0),
        local_ts: 0,
        bid,
        ask,
        bid_size,
        ask_size,
        symbol: String::new(),
    })
}

/// One `trades` data row → a [`TradeTick`] (`local_ts` left at the not-stamped sentinel `0`; the
/// feed stamps receive time and overwrites `symbol` with the series label before emit). `None` on a
/// missing/unparseable field, a non-finite/non-positive price or size, or an unrecognized `side`.
fn row_to_trade(row: &Value) -> Option<TradeTick> {
    let price = fstr(row.get("px"))?;
    let size = fstr(row.get("sz"))?;
    if !is_valid_trade(price, size) {
        return None;
    }
    let ts = row.get("time").and_then(Value::as_i64)?;
    let is_buyer_maker = match row.get("side").and_then(Value::as_str)? {
        "A" => true, // taker SOLD (hit the ask) → the buyer was the MAKER (Binance `m` convention)
        "B" => false, // taker BOUGHT (lifted the bid)
        _ => return None,
    };
    let symbol = row.get("coin").and_then(Value::as_str).unwrap_or_default().to_string();
    Some(TradeTick { ts, local_ts: 0, price, size, is_buyer_maker, symbol })
}

/// PURE: one HL `trades` frame → zero or more [`TradeTick`]s (fixture-tested, no socket). Anything
/// that is not a `trades`-channel data push — another channel, or a frame with no/empty `data`
/// array — yields an empty vec; a malformed individual row is skipped in place rather than failing
/// the whole push (mirrors bybit's/okx's per-element tolerance).
pub fn trades_from_frame(frame: &Value) -> Vec<TradeTick> {
    if channel(frame) != Some("trades") {
        return Vec::new();
    }
    let Some(rows) = frame.get("data").and_then(Value::as_array) else {
        return Vec::new();
    };
    rows.iter().filter_map(row_to_trade).collect()
}

/// A full L2 depth snapshot decoded from one HL `l2Book` frame: `bids`/`asks` as `(price, size)`
/// levels plus the venue `time` (epoch-ms, the only ordering key HL provides). HL sends a full
/// snapshot every frame, so this always REPLACES the book — there is no delta/merge variant.
#[derive(Debug, Clone, PartialEq)]
pub struct L2Snapshot {
    pub bids: Vec<Level>,
    pub asks: Vec<Level>,
    /// Venue event-time (epoch-ms); `0` if absent.
    pub time: i64,
}

/// One HL `l2Book` frame → a full [`L2Snapshot`] (`levels[0]`=bids, `levels[1]`=asks, each level
/// `{px,sz,n}`). `None` unless it is an `l2Book`-channel frame with a `data.levels` array. Empty /
/// malformed level entries are skipped; a missing side yields an empty vec (never merged — HL
/// pushes the whole book each frame, §7).
pub fn l2book_to_snapshot(frame: &Value) -> Option<L2Snapshot> {
    if channel(frame) != Some("l2Book") {
        return None;
    }
    let d = frame.get("data")?;
    let levels = d.get("levels").and_then(Value::as_array)?;
    Some(L2Snapshot {
        bids: side_levels(levels.first()),
        asks: side_levels(levels.get(1)),
        time: d.get("time").and_then(Value::as_i64).unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn candle_decodes_with_exact_bits() {
        // HL candle: t/T are numbers; o/h/l/c/v are decimal strings; s/i/n are metadata.
        let frame = json!({
            "channel": "candle",
            "data": {
                "t": 1_700_000_060_000_i64,
                "T": 1_700_000_119_999_i64,
                "s": "BTC",
                "i": "1m",
                "o": "27010.25",
                "c": "27080.10",
                "h": "27100.00",
                "l": "27000.00",
                "v": "8.10000000",
                "n": 42
            }
        });
        let b = candle_to_bar(&frame).expect("candle → Bar");
        assert_eq!(b.ts, 1_700_000_060_000);
        assert_eq!(b.open.to_bits(), 27010.25_f64.to_bits());
        assert_eq!(b.high.to_bits(), 27100.00_f64.to_bits());
        assert_eq!(b.low.to_bits(), 27000.00_f64.to_bits());
        assert_eq!(b.close.to_bits(), 27080.10_f64.to_bits());
        assert_eq!(b.volume.to_bits(), 8.1_f64.to_bits());
        assert!(b.funding.is_none() && b.bid.is_none() && b.ask.is_none() && b.symbol.is_none());
    }

    #[test]
    fn non_candle_channel_is_none() {
        assert!(candle_to_bar(&json!({"channel": "bbo", "data": {}})).is_none());
        // a candle frame missing a required price → None (not a partial bar)
        assert!(candle_to_bar(&json!({"channel": "candle", "data": {"t": 1, "o": "1"}})).is_none());
    }

    #[test]
    fn bbo_decodes_both_sides() {
        let frame = json!({
            "channel": "bbo",
            "data": {
                "coin": "BTC",
                "time": 1_700_000_000_123_i64,
                "bbo": [
                    {"px": "60000.1", "sz": "1.5", "n": 3},
                    {"px": "60000.2", "sz": "2.0", "n": 5}
                ]
            }
        });
        let q = bbo_to_quote(&frame).expect("bbo → quote");
        assert_eq!(q.bid, 60000.1);
        assert_eq!(q.ask, 60000.2);
        assert_eq!(q.bid_size, 1.5);
        assert_eq!(q.ask_size, 2.0);
        assert_eq!(q.ts, 1_700_000_000_123);
        assert_eq!(q.local_ts, 0, "left unstamped for the feed");
        assert!(q.symbol.is_empty(), "left unlabeled for the feed");
    }

    #[test]
    fn bbo_one_sided_is_no_quote() {
        // a `null` ask (no resting ask) is not a two-sided quote
        let frame = json!({
            "channel": "bbo",
            "data": {"coin": "BTC", "time": 1, "bbo": [{"px": "100.0", "sz": "1"}, null]}
        });
        assert!(bbo_to_quote(&frame).is_none());
    }

    #[test]
    fn trades_map_aggressor_side() {
        let frame = json!({
            "channel": "trades",
            "data": [
                {"coin": "BTC", "side": "A", "px": "60000.7", "sz": "0.4", "time": 1_700_i64,
                 "hash": "0xabc", "tid": 111},
                {"coin": "BTC", "side": "B", "px": "60001.0", "sz": "0.1", "time": 1_701_i64,
                 "hash": "0xdef", "tid": 222}
            ]
        });
        let ticks = trades_from_frame(&frame);
        assert_eq!(ticks.len(), 2);
        // side "A" = sell (taker sold) → buyer was the maker
        assert_eq!(ticks[0].price, 60000.7);
        assert_eq!(ticks[0].size, 0.4);
        assert_eq!(ticks[0].ts, 1_700);
        assert!(ticks[0].is_buyer_maker, "side A (sell) → buyer was the maker");
        assert_eq!(ticks[0].symbol, "BTC");
        // side "B" = buy (taker bought) → buyer was the taker
        assert!(!ticks[1].is_buyer_maker, "side B (buy) → buyer was the taker");
    }

    #[test]
    fn trades_skip_bad_rows_and_wrong_channel() {
        // a zero size and an unknown side are each dropped in place; the good row survives
        let frame = json!({
            "channel": "trades",
            "data": [
                {"side": "A", "px": "1.0", "sz": "0", "time": 1},        // zero size → dropped
                {"side": "X", "px": "1.0", "sz": "1", "time": 2},        // bad side → dropped
                {"coin": "ETH", "side": "B", "px": "3000.0", "sz": "2", "time": 3}
            ]
        });
        let ticks = trades_from_frame(&frame);
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0].symbol, "ETH");
        // a non-trades frame yields nothing
        assert!(trades_from_frame(&json!({"channel": "l2Book", "data": {}})).is_empty());
    }

    #[test]
    fn l2book_full_snapshot_splits_bids_and_asks() {
        let frame = json!({
            "channel": "l2Book",
            "data": {
                "coin": "BTC",
                "time": 1_700_000_000_500_i64,
                "levels": [
                    [{"px": "60000.0", "sz": "5", "n": 2}, {"px": "59999.0", "sz": "3", "n": 1}],
                    [{"px": "60001.0", "sz": "4", "n": 3}]
                ]
            }
        });
        let snap = l2book_to_snapshot(&frame).expect("l2Book → snapshot");
        assert_eq!(snap.bids, vec![(60000.0, 5.0), (59999.0, 3.0)]);
        assert_eq!(snap.asks, vec![(60001.0, 4.0)]);
        assert_eq!(snap.time, 1_700_000_000_500);
        // wrong channel → None
        assert!(l2book_to_snapshot(&json!({"channel": "bbo", "data": {}})).is_none());
    }
}
