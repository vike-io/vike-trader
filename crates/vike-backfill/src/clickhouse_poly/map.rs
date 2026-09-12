//! Pure decode of ClickHouse `FORMAT Parquet` exports (the latency box's `polymarket` DB) into
//! `vike_model::{QuoteTick, TradeTick}` — no I/O, fixture-tested.
//!
//! Source tables: `polymarket_trades` (on-chain fills — the L? trade tape) and
//! `polymarket_snapshots` (L1 top-of-book). The export SQL in [`super::ingest`] projects + aliases
//! a fixed column set so the arrow schema is predictable:
//!   - trades: `ts_ms` Int64, `token_id` Utf8, `price` Float64, `size` Float64, `side` Utf8
//!   - quotes: `ts_ms` Int64, `token_id` Utf8, `bid`/`ask`/`bid_size`/`ask_size` Float64
//!
//! Unlike the pmxt archive (decimal128 price/size columns), the ClickHouse source already stores
//! prices as `Float64` **0-1 probabilities** and sizes as `Float64` — so there is NO decimal
//! scaling here. `ts_ms` is `toUnixTimestamp64Milli(ts)` = epoch milliseconds. `side` is the
//! taker side (`BUY`/`SELL`); `is_buyer_maker` is derived `side == "SELL"`, mirroring the pmxt
//! `last_trade_price` mapping so trade-tape provenance stays consistent across the two Polymarket
//! sources.

use datafusion::arrow::array::Array;
use datafusion::arrow::record_batch::RecordBatch;

use vike_model::{QuoteTick, TradeTick};

use crate::arrowutil::{f64_col, i64_col, str_col};
use crate::error::CollectError;

/// Vendor prefix for the shared Arrow accessor error messages (see [`crate::arrowutil`]).
const CTX: &str = "clickhouse";

/// Decode one `polymarket_trades` export batch into `(token_id, TradeTick)` pairs. `symbol` is
/// left empty on each tick — the store keys the series by the `(venue, symbol)` append parameters
/// and re-injects the symbol on scan, so carrying the 78-char token id per row would only waste
/// memory across a multi-million-row day. Rows with a null token id are skipped.
pub fn trades_from_batch(b: &RecordBatch) -> Result<Vec<(String, TradeTick)>, CollectError> {
    let ts = i64_col(b, "ts_ms", CTX)?;
    let token = str_col(b, "token_id", CTX)?;
    let price = f64_col(b, "price", CTX)?;
    let size = f64_col(b, "size", CTX)?;
    let side = str_col(b, "side", CTX)?;

    let mut out = Vec::with_capacity(b.num_rows());
    for i in 0..b.num_rows() {
        if token.is_null(i) {
            continue;
        }
        out.push((
            token.value(i).to_string(),
            TradeTick {
                ts: ts.value(i),
                local_ts: 0,
                price: price.value(i),
                size: size.value(i),
                is_buyer_maker: side.value(i) == "SELL",
                symbol: String::new(),
            },
        ));
    }
    Ok(out)
}

/// Decode one `polymarket_snapshots` (L1) export batch into `(token_id, QuoteTick)` pairs. Same
/// empty-`symbol` discipline as [`trades_from_batch`].
pub fn quotes_from_batch(b: &RecordBatch) -> Result<Vec<(String, QuoteTick)>, CollectError> {
    let ts = i64_col(b, "ts_ms", CTX)?;
    let token = str_col(b, "token_id", CTX)?;
    let bid = f64_col(b, "bid", CTX)?;
    let ask = f64_col(b, "ask", CTX)?;
    let bid_size = f64_col(b, "bid_size", CTX)?;
    let ask_size = f64_col(b, "ask_size", CTX)?;

    let mut out = Vec::with_capacity(b.num_rows());
    for i in 0..b.num_rows() {
        if token.is_null(i) {
            continue;
        }
        out.push((
            token.value(i).to_string(),
            QuoteTick {
                ts: ts.value(i),
                local_ts: 0,
                bid: bid.value(i),
                ask: ask.value(i),
                bid_size: bid_size.value(i),
                ask_size: ask_size.value(i),
                symbol: String::new(),
            },
        ));
    }
    Ok(out)
}

/// Decode a [`super::ingest::book_tokens_query`] export batch into a flat token-id list (order not
/// significant — the caller, [`super::ingest::ingest_book_day`], scans each one independently via
/// `ClickHousePolyHistStore::scan_book_updates`). Null ids are skipped defensively — `arrayJoin`
/// over a non-null `Array(String)` column shouldn't emit one, but a decode helper should never
/// assume the DB can't surprise it.
pub fn tokens_from_batch(b: &RecordBatch) -> Result<Vec<String>, CollectError> {
    let token = str_col(b, "token_id", CTX)?;
    let mut out = Vec::with_capacity(b.num_rows());
    for i in 0..b.num_rows() {
        if !token.is_null(i) {
            out.push(token.value(i).to_string());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use datafusion::arrow::array::{Float64Array, Int64Array, StringArray};
    use datafusion::arrow::datatypes::{DataType, Field, Schema};
    use std::sync::Arc;

    #[test]
    fn trades_batch_decodes_price_size_and_side() {
        let schema = Schema::new(vec![
            Field::new("ts_ms", DataType::Int64, false),
            Field::new("token_id", DataType::Utf8, false),
            Field::new("price", DataType::Float64, false),
            Field::new("size", DataType::Float64, false),
            Field::new("side", DataType::Utf8, false),
        ]);
        let batch = RecordBatch::try_new(
            Arc::new(schema),
            vec![
                Arc::new(Int64Array::from(vec![1_784_667_069_000_i64, 1_784_667_070_000])),
                Arc::new(StringArray::from(vec!["TOKA", "TOKB"])),
                Arc::new(Float64Array::from(vec![0.5_f64, 0.97])),
                Arc::new(Float64Array::from(vec![10.0_f64, 2.0])),
                Arc::new(StringArray::from(vec!["BUY", "SELL"])),
            ],
        )
        .unwrap();

        let rows = trades_from_batch(&batch).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "TOKA");
        assert_eq!(rows[0].1.ts, 1_784_667_069_000);
        assert_eq!(rows[0].1.price, 0.5);
        assert_eq!(rows[0].1.size, 10.0);
        assert!(!rows[0].1.is_buyer_maker, "BUY taker → buyer was NOT the maker");
        assert_eq!(rows[0].1.symbol, "", "symbol stays empty; append param keys the series");
        assert!(rows[1].1.is_buyer_maker, "SELL taker → buyer was the maker");
    }

    #[test]
    fn quotes_batch_decodes_l1_levels() {
        let schema = Schema::new(vec![
            Field::new("ts_ms", DataType::Int64, false),
            Field::new("token_id", DataType::Utf8, false),
            Field::new("bid", DataType::Float64, false),
            Field::new("ask", DataType::Float64, false),
            Field::new("bid_size", DataType::Float64, false),
            Field::new("ask_size", DataType::Float64, false),
        ]);
        let batch = RecordBatch::try_new(
            Arc::new(schema),
            vec![
                Arc::new(Int64Array::from(vec![1_784_667_069_000_i64])),
                Arc::new(StringArray::from(vec!["TOKA"])),
                Arc::new(Float64Array::from(vec![0.49_f64])),
                Arc::new(Float64Array::from(vec![0.51_f64])),
                Arc::new(Float64Array::from(vec![100.0_f64])),
                Arc::new(Float64Array::from(vec![80.0_f64])),
            ],
        )
        .unwrap();

        let rows = quotes_from_batch(&batch).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "TOKA");
        assert_eq!(rows[0].1.bid, 0.49);
        assert_eq!(rows[0].1.ask, 0.51);
        assert_eq!(rows[0].1.bid_size, 100.0);
        assert_eq!(rows[0].1.ask_size, 80.0);
        assert_eq!(rows[0].1.mid(), 0.5, "L1 mid of a 0-1 probability book");
    }

    #[test]
    fn wrong_column_type_is_a_clean_error() {
        // price supplied as Int64 where Float64 is required → a descriptive Err, not a panic.
        let schema = Schema::new(vec![
            Field::new("ts_ms", DataType::Int64, false),
            Field::new("token_id", DataType::Utf8, false),
            Field::new("price", DataType::Int64, false),
            Field::new("size", DataType::Float64, false),
            Field::new("side", DataType::Utf8, false),
        ]);
        let batch = RecordBatch::try_new(
            Arc::new(schema),
            vec![
                Arc::new(Int64Array::from(vec![1_i64])),
                Arc::new(StringArray::from(vec!["T"])),
                Arc::new(Int64Array::from(vec![1_i64])),
                Arc::new(Float64Array::from(vec![1.0_f64])),
                Arc::new(StringArray::from(vec!["BUY"])),
            ],
        )
        .unwrap();
        let err = trades_from_batch(&batch).unwrap_err();
        assert!(format!("{err}").contains("price"), "error names the offending column");
    }

    #[test]
    fn tokens_batch_decodes_flat_ids_and_skips_nulls() {
        let schema = Schema::new(vec![Field::new("token_id", DataType::Utf8, true)]);
        let batch = RecordBatch::try_new(
            Arc::new(schema),
            vec![Arc::new(StringArray::from(vec![Some("TOKA"), None, Some("TOKB")]))],
        )
        .unwrap();
        let out = tokens_from_batch(&batch).unwrap();
        assert_eq!(out, vec!["TOKA".to_string(), "TOKB".to_string()]);
    }
}
