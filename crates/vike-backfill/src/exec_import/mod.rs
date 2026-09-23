//! Historical execution-trade import — the backfill counterpart to the live `JournalMaterializer`
//! (unified-journaling #2, Tier-2 feeder 2). Both write the SAME `kind=exec_fill` series, so live
//! and historical fills converge into one queryable log. This module owns the pure venue-export
//! parsers (fixture-tested, no I/O); the `exec_trade_backfill` bin drives them into the store,
//! idempotent per commit-key — the exact ingest-and-commit pattern the `eod`/`pmxt` backfills use.
//!
//! v1 ingests a venue TRADE-HISTORY EXPORT (JSON), which needs no live credentials and is CI-clean.
//! A live signed-REST source (binance `myTrades`, etc.) is an additive follow-up: implement
//! [`ExecTradeSource::fetch`] for a venue and register it, reusing the bridge's signer/transport —
//! the parser here already produces the rows either path yields.

use vike_data::ExecFillRow;

use crate::error::CollectError;

/// A pluggable execution-trade-history source. One impl per venue (live REST) — the export-file
/// path in the bin uses the parsers directly and needs no source impl.
pub trait ExecTradeSource {
    /// Provider name — used as the default store `venue` and in logs.
    fn name(&self) -> &str;
    /// Account fills for `symbol` in `[since_ms, until_ms]`, ts-ascending. Live impls sign + page
    /// the venue REST; the returned rows are the SAME `ExecFillRow`s the export parser produces.
    fn fetch(
        &self,
        symbol: &str,
        since_ms: i64,
        until_ms: i64,
    ) -> Result<Vec<ExecFillRow>, CollectError>;
}

/// Parse a Binance `myTrades` response / export (a JSON array of trade objects) into `ExecFillRow`s
/// for `(venue, symbol)`. Pure — no clock, no network. Unknown/short rows are skipped tolerantly
/// (a malformed row never aborts a bulk import); numeric strings are Binance's wire format.
///
/// Shape (Binance spot `GET /api/v3/myTrades`): `[{ "id": u64, "orderId": u64, "price": "…",
/// "qty": "…", "commission": "…", "time": ms, "isBuyer": bool, … }]`. `side` = +1 buyer / -1 seller;
/// `commission` is a positive cost (Binance reports the charged fee).
pub fn parse_binance_my_trades(
    json: &str,
    venue: &str,
    symbol: &str,
) -> Result<Vec<ExecFillRow>, CollectError> {
    let v: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| CollectError::Fetch(format!("myTrades json: {e}")))?;
    let arr = v.as_array().ok_or_else(|| CollectError::Fetch("myTrades: not an array".into()))?;
    let mut rows = Vec::with_capacity(arr.len());
    for t in arr {
        // required fields; skip a row missing any (tolerant bulk import)
        let (Some(id), Some(px), Some(qty), Some(time)) = (
            t.get("id").and_then(trade_id_str),
            t.get("price").and_then(num_str),
            t.get("qty").and_then(num_str),
            t.get("time").and_then(serde_json::Value::as_i64),
        ) else {
            continue;
        };
        let is_buyer = t.get("isBuyer").and_then(serde_json::Value::as_bool).unwrap_or(true);
        // "isMaker" is present on the real myTrades shape; absent (e.g. a minimal export) → not
        // surfaced, so liquidity_side stays "" rather than guessing.
        let liquidity_side = match t.get("isMaker").and_then(serde_json::Value::as_bool) {
            Some(true) => "maker",
            Some(false) => "taker",
            None => "",
        };
        rows.push(ExecFillRow {
            ts: time,
            trade_id: id,
            client_order_id: t.get("orderId").and_then(trade_id_str).unwrap_or_default(),
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            side: if is_buyer { 1 } else { -1 },
            qty,
            px,
            commission: t.get("commission").and_then(num_str).unwrap_or(0.0),
            // the venue trade-history export has no mark price field — not surfaced by this source.
            mark_price: None,
            liquidity_side: liquidity_side.to_string(),
            commission_asset: t
                .get("commissionAsset")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
        });
    }
    rows.sort_by_key(|r| r.ts);
    Ok(rows)
}

/// A Binance numeric field is a quoted string ("64000.0"); also accept a bare number.
fn num_str(v: &serde_json::Value) -> Option<f64> {
    v.as_str().and_then(|s| s.parse::<f64>().ok()).or_else(|| v.as_f64())
}

/// A Binance id is an integer; render it to the `String` `ExecFillRow` carries.
///
/// An EMPTY rendered id is `None`, not `Some("")`. `ExecFillRow::trade_id` stays a `String` (the
/// stored schema must keep reading Parquet written before `vike_model::events::TradeId` existed), so
/// this is the WRITE-side door: a row whose `id` is `""` would persist an id that can never dedup
/// and would then be dropped by every reader that turns the row back into a `FillEvent`
/// (`vike_report::journal_read::exec_fill_to_event`). Refusing it here means it is never written.
/// For the required `id` field the caller's `let else` skips the whole row — the same tolerant
/// treatment a row missing `id` already gets; for the optional `orderId` it is indistinguishable
/// from absent, which is what `unwrap_or_default()` already assumed.
fn trade_id_str(v: &serde_json::Value) -> Option<String> {
    v.as_u64()
        .map(|n| n.to_string())
        .or_else(|| v.as_str().map(str::to_string))
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"[
        {"symbol":"BTCUSDT","id":28457,"orderId":100234,"price":"64000.0","qty":"0.5",
         "commission":"0.001","commissionAsset":"BNB","time":1499865549590,"isBuyer":true,"isMaker":false},
        {"symbol":"BTCUSDT","id":28458,"orderId":100235,"price":"64100.0","qty":"0.25",
         "commission":"0.0005","commissionAsset":"BNB","time":1499865549000,"isBuyer":false,"isMaker":true},
        {"garbage":"missing fields"}
    ]"#;

    #[test]
    fn parses_and_sorts_binance_my_trades_skipping_malformed() {
        let rows = parse_binance_my_trades(SAMPLE, "binance", "BTCUSDT").unwrap();
        assert_eq!(rows.len(), 2, "the malformed row is skipped, the two valid ones kept");
        // sorted ascending by ts → the seller (earlier time) is first
        assert_eq!(rows[0].trade_id, "28458");
        assert_eq!(rows[0].side, -1, "isBuyer:false → sell");
        assert_eq!(rows[0].ts, 1499865549000);
        assert_eq!(rows[0].liquidity_side, "maker", "isMaker:true → maker");
        assert_eq!(rows[0].commission_asset, "BNB");
        assert_eq!(rows[1].trade_id, "28457");
        assert_eq!(rows[1].side, 1);
        assert!((rows[1].px - 64000.0).abs() < 1e-9);
        assert!((rows[1].qty - 0.5).abs() < 1e-9);
        assert_eq!(rows[1].client_order_id, "100234");
        assert_eq!(rows[1].venue, "binance");
        assert_eq!(rows[1].liquidity_side, "taker", "isMaker:false → taker");
        assert_eq!(rows[1].commission_asset, "BNB");
    }

    #[test]
    fn missing_is_maker_and_commission_asset_default_to_empty() {
        // A minimal export lacking "isMaker"/"commissionAsset" must not guess — both default to "".
        let sample = r#"[{"id":1,"orderId":2,"price":"1.0","qty":"1.0","commission":"0.0",
            "time":1000,"isBuyer":true}]"#;
        let rows = parse_binance_my_trades(sample, "binance", "BTCUSDT").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].liquidity_side, "");
        assert_eq!(rows[0].commission_asset, "");
    }

    /// An EXPORT row whose `id` renders empty is skipped exactly like a row missing `id`: the
    /// stored `trade_id` is the fill dedup key, and a persisted `""` can never dedup (it is what
    /// `vike_model::events::TradeId` exists to forbid). The good row in the same array survives —
    /// one unusable row must never abort a bulk import.
    #[test]
    fn an_empty_trade_id_row_is_skipped_and_does_not_abort_the_import() {
        let sample = r#"[
            {"id":"","orderId":2,"price":"1.0","qty":"1.0","commission":"0.0","time":1000,"isBuyer":true},
            {"id":7,"orderId":"","price":"2.0","qty":"1.0","commission":"0.0","time":2000,"isBuyer":true}
        ]"#;
        let rows = parse_binance_my_trades(sample, "binance", "BTCUSDT").unwrap();
        assert_eq!(rows.len(), 1, "the empty-id row is skipped, the valid one kept");
        assert_eq!(rows[0].trade_id, "7");
        // an empty OPTIONAL `orderId` is indistinguishable from absent — still the "" default
        assert_eq!(rows[0].client_order_id, "");
    }

    #[test]
    fn non_array_is_an_error_empty_array_is_empty() {
        assert!(parse_binance_my_trades("{}", "binance", "BTCUSDT").is_err());
        assert!(parse_binance_my_trades("[]", "binance", "BTCUSDT").unwrap().is_empty());
    }
}
