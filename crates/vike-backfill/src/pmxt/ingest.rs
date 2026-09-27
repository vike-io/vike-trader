//! pmxt Parquet streaming ingest: HTTP download of one archive hour + bounded-memory row-group
//! decode + drive [`crate::pmxt::map::map_row`] into the `vike-data` hist store.
//!
//! Data source: the [pmxt](https://archive.pmxt.dev) Polymarket order-book archive, licensed
//! [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/). This module downloads and decodes
//! Parquet files published by **pmxt** (`r2v2.pmxt.dev`); pmxt is not affiliated with this
//! project.
//!
//! Memory bound: [`ParquetRecordBatchReaderBuilder`] iterates one row group's [`RecordBatch`] at
//! a time (never the whole file), which is why the R2 parts (130-400 MB each) are ingestable on
//! an ordinary box. [`download_hour`] is the same discipline on the network side — the response
//! body is streamed straight to a temp file via `std::io::copy`, never buffered as a `String`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use datafusion::arrow::array::{Array, Decimal128Array, StringArray};
use datafusion::arrow::record_batch::RecordBatch;

use vike_data::{DataFusionHist, HistStore};
use vike_model::{BookUpdate, QuoteTick, TradeTick};

use crate::arrowutil::{decimal_col, for_each_batch, str_col, ts_col};
use crate::error::CollectError;
use crate::pmxt::map::{MapState, Mapped, PmxtRow, l1_from_row, map_row};

/// Vendor prefix for the shared Arrow accessor error messages (see [`crate::arrowutil`]).
const CTX: &str = "pmxt";

/// pmxt's fixed decimal scales (spec-fixed rather than read from the array, per the archive's
/// published schema): `price`/`new_tick_size` are scale-4, `size` is scale-6.
const PRICE_SCALE_DIVISOR: f64 = 10_000.0;
const SIZE_SCALE_DIVISOR: f64 = 1_000_000.0;

/// Build the R2 URL for one UTC-hour Parquet part (`day_hour` = `YYYY-MM-DDTHH`).
pub fn hour_url(day_hour: &str) -> String {
    format!("https://r2v2.pmxt.dev/polymarket_orderbook_{day_hour}.parquet")
}

/// Download one hour's Parquet part into a fresh temp file under `tmp_dir`, streaming the
/// response body straight to disk (never `read_to_string` — parts run 130-400 MB). `Ok(None)`
/// on an HTTP 404 (the hour hasn't been published / doesn't exist — not an error, callers skip
/// it). Any other non-2xx status or transport failure is `Err`.
pub fn download_hour(day_hour: &str, tmp_dir: &Path) -> Result<Option<PathBuf>, CollectError> {
    let url = hour_url(day_hour);
    let opts = crate::http::GetOptions {
        user_agent: Some("vike-trader-rust (pmxt-backfill; https://github.com/vike-io)"),
        ..Default::default()
    };
    let dest = tmp_dir.join(format!("pmxt_{day_hour}.parquet"));
    // 404 → `Ok(None)` (the hour isn't published yet — callers skip it); streamed to disk (parts run
    // 130-400 MB), no global timeout. Shared [`crate::http::get_to_file`].
    crate::http::get_to_file(&url, &dest, &opts, "pmxt", true)
}

// ---- Arrow column decode ----------------------------------------------------------------------
// The typed column accessors (str_col/ts_col/decimal_col) live in `crate::arrowutil` (the shared
// Arrow twin of csvutil); the per-column decimal scales + null policies below stay here.

fn opt_str(arr: &StringArray, i: usize) -> Option<String> {
    (!arr.is_null(i)).then(|| arr.value(i).to_string())
}

fn opt_decimal(arr: &Decimal128Array, i: usize, divisor: f64) -> Option<f64> {
    (!arr.is_null(i)).then(|| arr.value(i) as f64 / divisor)
}

/// Decode one `RecordBatch` (one row group's worth of rows) into [`PmxtRow`]s.
fn rows_from_batch(b: &RecordBatch) -> Result<Vec<PmxtRow>, CollectError> {
    let event_type = str_col(b, "event_type", CTX)?;
    let asset_id = str_col(b, "asset_id", CTX)?;
    let side = str_col(b, "side", CTX)?;
    let bids = str_col(b, "bids", CTX)?;
    let asks = str_col(b, "asks", CTX)?;
    let ts = ts_col(b, "timestamp", CTX)?;
    let local_ts = ts_col(b, "timestamp_received", CTX)?;
    let price = decimal_col(b, "price", CTX)?;
    let size = decimal_col(b, "size", CTX)?;
    let new_tick_size = decimal_col(b, "new_tick_size", CTX)?;
    let best_bid = decimal_col(b, "best_bid", CTX)?;
    let best_ask = decimal_col(b, "best_ask", CTX)?;

    let mut out = Vec::with_capacity(b.num_rows());
    for i in 0..b.num_rows() {
        out.push(PmxtRow {
            event_type: event_type.value(i).to_string(),
            ts_ms: ts.value(i),
            local_ts_ms: local_ts.value(i),
            asset_id: asset_id.value(i).to_string(),
            bids: opt_str(bids, i),
            asks: opt_str(asks, i),
            price: opt_decimal(price, i, PRICE_SCALE_DIVISOR),
            size: opt_decimal(size, i, SIZE_SCALE_DIVISOR),
            side: opt_str(side, i),
            new_tick_size: opt_decimal(new_tick_size, i, PRICE_SCALE_DIVISOR),
            best_bid: opt_decimal(best_bid, i, PRICE_SCALE_DIVISOR),
            best_ask: opt_decimal(best_ask, i, PRICE_SCALE_DIVISOR),
        });
    }
    Ok(out)
}

/// Stream-decode one downloaded pmxt Parquet file (one row group at a time — bounded memory),
/// drive every row through [`map_row`] with ONE [`MapState`] for the whole file, and append the
/// resulting per-asset book/trade batches into `store` under `commit_key`s scoped to
/// `(kind, asset, hour)` (idempotent re-runs of the same hour are a no-op). `tokens` — when
/// `Some` — narrows ingest to those asset ids; rows for any other asset are skipped before
/// hitting the mapper (so filtered-out assets never populate `MapState`, and the append cost is
/// paid only for the tokens the caller actually wants). Returns `(book events written, trades
/// written)`.
pub fn ingest_file(
    store: &DataFusionHist,
    path: &Path,
    hour: &str,
    tokens: Option<&HashSet<String>>,
) -> Result<(usize, usize), CollectError> {
    let mut state = MapState::default();
    #[allow(clippy::type_complexity)]
    let mut per_asset: HashMap<String, (Vec<BookUpdate>, Vec<TradeTick>, Vec<QuoteTick>)> =
        HashMap::new();

    // Shared bounded-memory row-group reader ([`crate::arrowutil::for_each_batch`]); pmxt keeps its
    // stateful `MapState` loop over each batch.
    for_each_batch(path, |batch| {
        for row in rows_from_batch(batch)? {
            if let Some(allow) = tokens
                && !allow.contains(&row.asset_id)
            {
                continue;
            }
            // The venue's own top of book on this row — the ghost-level repair the L2 replay
            // needs (see `l1_from_row`). Taken BEFORE `map_row` consumes the row.
            let l1 = l1_from_row(&row);
            match map_row(&mut state, &row) {
                Mapped::Book(update) => {
                    per_asset.entry(row.asset_id.clone()).or_default().0.push(update);
                }
                Mapped::Trade(trade) => {
                    per_asset.entry(row.asset_id.clone()).or_default().1.push(trade);
                }
                Mapped::None => {}
            }
            if let Some(q) = l1 {
                per_asset.entry(row.asset_id).or_default().2.push(q);
            }
        }
        Ok(())
    })?;

    let mut total_books = 0usize;
    let mut total_trades = 0usize;
    for (asset, (books, trades, quotes)) in per_asset {
        if !books.is_empty() {
            total_books += store.append_book_updates(
                "polymarket",
                &asset,
                &books,
                Some(&format!("pmxt:book:{asset}:{hour}")),
            )?;
        }
        if !trades.is_empty() {
            total_trades += store.append_trades(
                "polymarket",
                &asset,
                &trades,
                Some(&format!("pmxt:trade:{asset}:{hour}")),
            )?;
        }
        if !quotes.is_empty() {
            store.append_quotes(
                "polymarket",
                &asset,
                &quotes,
                Some(&format!("pmxt:quote:{asset}:{hour}")),
            )?;
        }
    }
    Ok((total_books, total_trades))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hour_url_matches_r2_naming() {
        assert_eq!(
            hour_url("2026-04-13T22"),
            "https://r2v2.pmxt.dev/polymarket_orderbook_2026-04-13T22.parquet"
        );
    }

    /// Locks the per-column decimal scales in CI (the network smoke is `#[ignore]`d). pmxt columns:
    /// price/new_tick_size = decimal128(9,4) → ÷1e4; size = decimal128(18,6) → ÷1e6. A wrong divisor
    /// would silently corrupt every price/size by orders of magnitude.
    #[test]
    fn decimal_scales_decode_price_and_size() {
        // raw i128 values as arrow stores them (unscaled); `opt_decimal` divides by the passed scale.
        let arr = Decimal128Array::from(vec![Some(1390_i128), None, Some(5_208_325_i128)]);
        // price grid: raw 1390 @ ÷1e4 → 0.1390 (matches the verified real-file sample)
        assert!((opt_decimal(&arr, 0, PRICE_SCALE_DIVISOR).unwrap() - 0.139).abs() < 1e-12);
        assert_eq!(opt_decimal(&arr, 1, PRICE_SCALE_DIVISOR), None, "null → None");
        // size grid: raw 5_208_325 @ ÷1e6 → 5.208325
        assert!((opt_decimal(&arr, 2, SIZE_SCALE_DIVISOR).unwrap() - 5.208325).abs() < 1e-12);
        assert_eq!(PRICE_SCALE_DIVISOR, 10_000.0);
        assert_eq!(SIZE_SCALE_DIVISOR, 1_000_000.0);
    }
}
