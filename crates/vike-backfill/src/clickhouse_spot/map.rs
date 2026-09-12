//! Pure decode of the `data_history.spot_1s` `FORMAT Parquet` export into `vike_model::QuoteTick`
//! — no I/O, fixture-tested.
//!
//! The export SQL in [`super::ingest`] projects a fixed two-column schema so the arrow types are
//! predictable: `ts_ms` Int64 (`toUnixTimestamp(sec) * 1000` — the source column is a second-
//! resolution `DateTime`) and `px` Float64.
//!
//! `bid == ask == px`, sizes zero: see the module doc of [`super`] for why a scalar price series
//! is carried as a degenerate quote rather than a bar. `symbol` is left EMPTY on each tick — the
//! store keys the series by the `(venue, symbol)` append parameters and re-injects the symbol on
//! scan, the same discipline `clickhouse_poly::map` uses.

use datafusion::arrow::array::Array;
use datafusion::arrow::record_batch::RecordBatch;

use vike_model::QuoteTick;

use crate::arrowutil::{f64_col, i64_col};
use crate::error::CollectError;

/// Vendor prefix for the shared Arrow accessor error messages (see [`crate::arrowutil`]).
const CTX: &str = "clickhouse-spot";

/// Decode one `spot_1s` export batch into [`QuoteTick`]s. Rows with a null `px` are skipped, as
/// are NaN and non-positive prices — `trailing_sigma` takes `ln(p1/p0)`, so a zero, negative or
/// NaN sample is not a price at all and must never reach the estimator.
pub fn spot_from_batch(b: &RecordBatch) -> Result<Vec<QuoteTick>, CollectError> {
    let ts = i64_col(b, "ts_ms", CTX)?;
    let px = f64_col(b, "px", CTX)?;

    let mut out = Vec::with_capacity(b.num_rows());
    for i in 0..b.num_rows() {
        if px.is_null(i) || ts.is_null(i) {
            continue;
        }
        let p = px.value(i);
        if p.is_nan() || p <= 0.0 {
            continue;
        }
        out.push(QuoteTick {
            ts: ts.value(i),
            local_ts: 0,
            bid: p,
            ask: p,
            bid_size: 0.0,
            ask_size: 0.0,
            symbol: String::new(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use datafusion::arrow::array::{Float64Array, Int64Array};
    use datafusion::arrow::datatypes::{DataType, Field, Schema};
    use std::sync::Arc;

    fn batch(ts: Vec<i64>, px: Vec<Option<f64>>) -> RecordBatch {
        let schema = Schema::new(vec![
            Field::new("ts_ms", DataType::Int64, false),
            Field::new("px", DataType::Float64, true),
        ]);
        RecordBatch::try_new(
            Arc::new(schema),
            vec![Arc::new(Int64Array::from(ts)), Arc::new(Float64Array::from(px))],
        )
        .unwrap()
    }

    #[test]
    fn decodes_a_scalar_price_into_a_degenerate_quote() {
        let b = batch(
            vec![1_781_092_500_000, 1_781_092_501_000],
            vec![Some(104_233.5), Some(104_234.0)],
        );
        let rows = spot_from_batch(&b).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].ts, 1_781_092_500_000);
        assert_eq!(rows[0].bid, 104_233.5);
        assert_eq!(rows[0].ask, 104_233.5);
        assert_eq!(rows[0].bid_size, 0.0);
        assert_eq!(rows[0].ask_size, 0.0);
        assert_eq!(rows[0].symbol, "", "symbol stays empty; the append params key the series");
        // the lossless round trip the module doc claims: mid of (p, p) is EXACTLY p
        assert_eq!(rows[0].mid(), 104_233.5);
        assert_eq!(rows[1].mid(), 104_234.0);
    }

    #[test]
    fn null_and_non_positive_prices_are_dropped() {
        // `trailing_sigma` takes ln(p1/p0) — a 0 / negative / null sample must never reach it.
        let b = batch(vec![1, 2, 3, 4], vec![Some(100.0), None, Some(0.0), Some(-1.0)]);
        let rows = spot_from_batch(&b).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].ts, 1);
    }

    #[test]
    fn wrong_column_type_is_a_clean_error() {
        let schema = Schema::new(vec![
            Field::new("ts_ms", DataType::Int64, false),
            Field::new("px", DataType::Int64, false),
        ]);
        let b = RecordBatch::try_new(
            Arc::new(schema),
            vec![Arc::new(Int64Array::from(vec![1_i64])), Arc::new(Int64Array::from(vec![1_i64]))],
        )
        .unwrap();
        let err = spot_from_batch(&b).unwrap_err();
        assert!(format!("{err}").contains("px"), "error names the offending column");
    }
}
