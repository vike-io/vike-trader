//! Pure JSONEachRow row serializers for the `polymarket` ClickHouse tables (no I/O) — the
//! write-side counterpart of [`crate::clickhouse_poly`]'s read-side row decode.
//!
//! These serialize the SAME `vike_model::{BookUpdate, TradeTick, QuoteTick}` shapes
//! [`crate::pmxt::map`] already produces into the exact row shape the live L2 recorder writes into
//! `polymarket.book_events` (16 keys) / `polymarket.l1_quotes` (8 keys) — see
//! `docs/superpowers/specs/2026-07-24-polymarket-l2-recorder-clickhouse-design.md`. Backfilled
//! pmxt history and the live recorder's own rows land in ONE series per token because both key off
//! `venue="polymarket"`/`token_id=asset_id` and the same table/column shape.
//!
//! **`ts`/`local_ts` are emitted as plain INTEGER epoch-ms, never a formatted DateTime string.**
//! ClickHouse's `JSONEachRow` reader parses a bare numeric literal fed to a `DateTime64(3)` column
//! as *seconds*, not milliseconds — an epoch-ms value passed that way overflows/misreads by 1000x.
//! Whatever integer column type `ts`/`local_ts` actually are on the deployed table, the recorder and
//! this backfill agree to send the raw millisecond integer, not a string.

use serde_json::json;

use vike_model::{BookUpdate, BookUpdateKind, QuoteTick, TradeTick};

/// Encode a depth side as the archive's own `[[price,size],...]` JSON shape, or `""` when empty —
/// `book_events.bids`/`asks` carry depth only on snapshot rows; delta/trade rows leave them blank.
fn levels_json(levels: &[(f64, f64)]) -> String {
    if levels.is_empty() {
        return String::new();
    }
    serde_json::to_string(levels).unwrap_or_default()
}

/// A `price_change` delta carries exactly ONE populated side (the mapper never fills both) — read
/// it off whichever vec is non-empty. `("none", 0.0, 0.0)` is the degenerate case (should not occur
/// for a real delta, but a JSON row is easier to reason about than a panic on a mapper bug).
fn delta_side_price_size(u: &BookUpdate) -> (&'static str, f64, f64) {
    if let Some(&(p, s)) = u.bids.first() {
        ("buy", p, s)
    } else if let Some(&(p, s)) = u.asks.first() {
        ("sell", p, s)
    } else {
        ("none", 0.0, 0.0)
    }
}

/// One `polymarket.book_events` row (16 keys) for a `BookUpdate`.
///
/// `BookUpdateKind::Snapshot` -> `event_type:"book", is_snapshot:1`, full depth as JSON in
/// `bids`/`asks`, `side:"none"`/`price:0`/`size:0`/`best_bid:0`/`best_ask:0`. `BookUpdateKind::Delta`
/// -> `event_type:"price_change", is_snapshot:0`, the single populated side/price/size, `bids`/
/// `asks:""`. The pmxt mapper (`crate::pmxt::map::map_row`) never emits `GapStart`/`Stale`/
/// `LiveResume` (the archive carries no connectivity markers of its own), but those kinds still
/// degrade to a harmless `event_type:"status"` row here rather than panicking, in case a future
/// mapper change starts emitting them.
pub fn book_event_json(token_id: &str, condition_id: &str, u: &BookUpdate) -> String {
    let (event_type, is_snapshot): (&str, u8) = match u.kind {
        BookUpdateKind::Snapshot => ("book", 1),
        BookUpdateKind::Delta => ("price_change", 0),
        BookUpdateKind::GapStart | BookUpdateKind::Stale | BookUpdateKind::LiveResume => {
            ("status", 0)
        }
    };
    let (side, price, size) = match u.kind {
        BookUpdateKind::Delta => delta_side_price_size(u),
        _ => ("none", 0.0, 0.0),
    };
    // Depth JSON is carried on snapshot rows only — a delta's `bids`/`asks` hold the single
    // populated side already surfaced via `side`/`price`/`size` above, not a depth column.
    let (bids_json, asks_json) = match u.kind {
        BookUpdateKind::Snapshot => (levels_json(&u.bids), levels_json(&u.asks)),
        _ => (String::new(), String::new()),
    };
    let row = json!({
        "token_id": token_id,
        "condition_id": condition_id,
        "ts": u.ts,
        "local_ts": u.local_ts,
        "seq": u.seq,
        "event_type": event_type,
        "is_snapshot": is_snapshot,
        "side": side,
        "price": price,
        "size": size,
        "best_bid": 0.0,
        "best_ask": 0.0,
        "bids": bids_json,
        "asks": asks_json,
        "tick_size": u.tick_size,
        "status": "",
    });
    format!("{row}\n")
}

/// One `polymarket.book_events` row (16 keys) for a `TradeTick` — trades share the SAME table as
/// book events (`event_type:"trade"`), not a separate one. `side` follows the recorder's convention:
/// `is_buyer_maker` true means the resting order was a buy, i.e. the taker SOLD.
pub fn trade_event_json(token_id: &str, condition_id: &str, t: &TradeTick) -> String {
    let row = json!({
        "token_id": token_id,
        "condition_id": condition_id,
        "ts": t.ts,
        "local_ts": t.local_ts,
        "seq": 0u64,
        "event_type": "trade",
        "is_snapshot": 0u8,
        "side": if t.is_buyer_maker { "sell" } else { "buy" },
        "price": t.price,
        "size": t.size,
        "best_bid": 0.0,
        "best_ask": 0.0,
        "bids": "",
        "asks": "",
        "tick_size": 0.0,
        "status": "",
    });
    format!("{row}\n")
}

/// One `polymarket.l1_quotes` row (8 keys) for a `QuoteTick` — the L1 lane
/// (`crate::pmxt::map::l1_from_row`) lifted off the archive's own `price_change.best_bid/best_ask`
/// columns.
pub fn l1_quote_json(token_id: &str, condition_id: &str, q: &QuoteTick) -> String {
    let row = json!({
        "token_id": token_id,
        "condition_id": condition_id,
        "ts": q.ts,
        "local_ts": q.local_ts,
        "bid": q.bid,
        "ask": q.ask,
        "bid_size": q.bid_size,
        "ask_size": q.ask_size,
    });
    format!("{row}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact `book_events` key set (16), independent of key ORDER (JSON objects are unordered).
    const BOOK_EVENTS_COLS: &[&str] = &[
        "token_id",
        "condition_id",
        "ts",
        "local_ts",
        "seq",
        "event_type",
        "is_snapshot",
        "side",
        "price",
        "size",
        "best_bid",
        "best_ask",
        "bids",
        "asks",
        "tick_size",
        "status",
    ];
    const L1_QUOTES_COLS: &[&str] =
        &["token_id", "condition_id", "ts", "local_ts", "bid", "ask", "bid_size", "ask_size"];

    fn assert_keys(v: &serde_json::Value, want: &[&str]) {
        let obj = v.as_object().expect("row is a JSON object");
        let mut got: Vec<&str> = obj.keys().map(String::as_str).collect();
        got.sort_unstable();
        let mut want: Vec<&str> = want.to_vec();
        want.sort_unstable();
        assert_eq!(got, want);
    }

    fn snapshot_update() -> BookUpdate {
        BookUpdate {
            ts: 1_700_000_000_000,
            local_ts: 1_700_000_000_003,
            seq: 1,
            kind: BookUpdateKind::Snapshot,
            tick_size: 0.01,
            bids: vec![(0.50, 100.0)],
            asks: vec![(0.51, 80.0)],
            symbol: "T".into(),
        }
    }

    #[test]
    fn snapshot_row_has_16_keys_json_depth_and_integer_ts() {
        let u = snapshot_update();
        let s = book_event_json("T", "0xcond", &u);
        assert!(s.ends_with('\n'), "newline-terminated for JSONEachRow stdin");
        let v: serde_json::Value = serde_json::from_str(s.trim()).unwrap();
        assert_keys(&v, BOOK_EVENTS_COLS);
        assert_eq!(v["event_type"], "book");
        assert_eq!(v["is_snapshot"], 1);
        assert_eq!(v["side"], "none");
        assert_eq!(v["price"], 0.0);
        assert_eq!(v["size"], 0.0);
        assert_eq!(v["bids"], "[[0.5,100.0]]");
        assert_eq!(v["asks"], "[[0.51,80.0]]");
        // ts/local_ts are INTEGER, never a DateTime string — a JSON number, not a quoted string.
        assert!(v["ts"].is_number(), "ts must be a JSON integer, not a string: {v}");
        assert!(v["local_ts"].is_number(), "local_ts must be a JSON integer, not a string: {v}");
        assert_eq!(v["ts"], 1_700_000_000_000i64);
        assert_eq!(v["local_ts"], 1_700_000_000_003i64);
        assert_eq!(v["token_id"], "T");
        assert_eq!(v["condition_id"], "0xcond");
        assert_eq!(v["tick_size"], 0.01);
        assert_eq!(v["status"], "");
    }

    #[test]
    fn delta_row_carries_the_populated_side_and_blanks_depth() {
        let buy = BookUpdate {
            ts: 1,
            local_ts: 2,
            seq: 5,
            kind: BookUpdateKind::Delta,
            tick_size: 0.01,
            bids: vec![(0.42, 7.0)],
            asks: vec![],
            symbol: "T".into(),
        };
        let v: serde_json::Value =
            serde_json::from_str(book_event_json("T", "0xc", &buy).trim()).unwrap();
        assert_keys(&v, BOOK_EVENTS_COLS);
        assert_eq!(v["event_type"], "price_change");
        assert_eq!(v["is_snapshot"], 0);
        assert_eq!(v["side"], "buy");
        assert_eq!(v["price"], 0.42);
        assert_eq!(v["size"], 7.0);
        assert_eq!(v["bids"], "");
        assert_eq!(v["asks"], "");

        let sell = BookUpdate {
            ts: 1,
            local_ts: 2,
            seq: 6,
            kind: BookUpdateKind::Delta,
            tick_size: 0.01,
            bids: vec![],
            asks: vec![(0.60, 0.0)],
            symbol: "T".into(),
        };
        let v: serde_json::Value =
            serde_json::from_str(book_event_json("T", "0xc", &sell).trim()).unwrap();
        assert_eq!(v["side"], "sell");
        assert_eq!(v["price"], 0.60);
        assert_eq!(v["size"], 0.0);
    }

    #[test]
    fn trade_row_has_16_keys_and_taker_side_convention() {
        let t = TradeTick {
            ts: 1_700_000_000_100,
            local_ts: 1_700_000_000_101,
            price: 0.95,
            size: 3.0,
            is_buyer_maker: true,
            symbol: "T".into(),
        };
        let v: serde_json::Value =
            serde_json::from_str(trade_event_json("T", "0xc", &t).trim()).unwrap();
        assert_keys(&v, BOOK_EVENTS_COLS);
        assert_eq!(v["event_type"], "trade");
        assert_eq!(v["is_snapshot"], 0);
        assert_eq!(v["side"], "sell", "is_buyer_maker=true -> resting buy -> taker SOLD");
        assert_eq!(v["price"], 0.95);
        assert_eq!(v["size"], 3.0);
        assert_eq!(v["seq"], 0);
        assert_eq!(v["bids"], "");
        assert_eq!(v["asks"], "");
        assert_eq!(v["tick_size"], 0.0);
        assert!(v["ts"].is_number());
        assert!(v["local_ts"].is_number());

        let taker_buy = TradeTick { is_buyer_maker: false, ..t };
        let v2: serde_json::Value =
            serde_json::from_str(trade_event_json("T", "0xc", &taker_buy).trim()).unwrap();
        assert_eq!(v2["side"], "buy");
    }

    #[test]
    fn l1_quote_row_has_8_keys_and_integer_ts() {
        let q = QuoteTick {
            ts: 1_700_000_000_200,
            local_ts: 1_700_000_000_201,
            bid: 0.44,
            ask: 0.47,
            bid_size: 0.0,
            ask_size: 0.0,
            symbol: "T".into(),
        };
        let s = l1_quote_json("T", "0xcond", &q);
        assert!(s.ends_with('\n'));
        let v: serde_json::Value = serde_json::from_str(s.trim()).unwrap();
        assert_keys(&v, L1_QUOTES_COLS);
        assert_eq!(v["token_id"], "T");
        assert_eq!(v["condition_id"], "0xcond");
        assert_eq!(v["bid"], 0.44);
        assert_eq!(v["ask"], 0.47);
        assert_eq!(v["bid_size"], 0.0);
        assert_eq!(v["ask_size"], 0.0);
        assert!(v["ts"].is_number());
        assert!(v["local_ts"].is_number());
    }
}
