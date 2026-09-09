//! ClickHouse → hist-store bridge for the 1-second spot reference series: shell
//! `clickhouse-client --query "... FORMAT Parquet"` for one UTC day of `data_history.spot_1s`,
//! decode the export a row group at a time, and append through `vike-data`'s `append_quotes`.
//!
//! Same operational contract as [`crate::clickhouse_poly::ingest`] — the CLI is already
//! authenticated from `~/.clickhouse-client/config.xml`, so no credential ever appears in this
//! code or the workspace `.env`; the export process is reused verbatim through
//! [`crate::clickhouse_poly::run_export`] rather than duplicated. **Read-only on the source**:
//! every query is a `SELECT`.
//!
//! Idempotency: each `(symbol, day)` append carries a `clickhouse:spot:{symbol}:{day}` commit
//! key, so re-running a day is a no-op.

use std::path::Path;

use vike_data::{DataFusionHist, HistStore};
use vike_model::QuoteTick;

use crate::arrowutil::for_each_batch;
use crate::clickhouse_spot::map::spot_from_batch;
use crate::error::CollectError;

/// The source database on the latency box's ClickHouse.
pub const DB: &str = "data_history";
/// The source table — a `ReplacingMergeTree(_v)`; see [`spot_query`] on why `FINAL` is mandatory.
pub const TABLE: &str = "spot_1s";
/// Default value of the source table's `market` column for the BTC reference series.
pub const MARKET: &str = "spot";
/// Default hist-store venue. Named after the source table's own `market` column rather than a
/// guessed exchange: `data_history.spot_1s` carries `(symbol, market)` and no venue attribution,
/// so claiming e.g. `binance` here would be an assertion the source does not make. Override with
/// the bin's `--venue` when the operator knows better.
pub const VENUE: &str = "spot";

/// Escape a single-quoted ClickHouse string literal by doubling any embedded quote. These values
/// come from operator argv (a symbol / market name), not untrusted input, but the whole statement
/// is passed as ONE `--query` argument, so an unescaped quote would break the SQL rather than
/// inject — doubling keeps a legal literal either way.
fn lit(s: &str) -> String {
    s.replace('\'', "''")
}

/// SQL for one UTC day `[day, next_day)` of the 1-second spot series, projected to the fixed
/// export schema (see [`super::map`]).
///
/// **`FINAL` is load-bearing, not decorative.** `spot_1s` is a `ReplacingMergeTree(_v)`: until
/// background merges collapse the parts, a plain `SELECT` returns superseded duplicate rows for
/// the same `(symbol, market, sec)`. Duplicated seconds would survive into the store, and
/// `trailing_sigma`'s per-second bucketing would then see a different last-write-wins price than
/// the venue published. [`spot_query_is_final_and_readonly`] pins the keyword.
///
/// The RAW SPARSE series is exported deliberately — no `WITH FILL` / `INTERPOLATE`. See the
/// module doc of [`super`].
///
/// **`toInt64` is load-bearing too.** `sec` is a `DateTime`, so `toUnixTimestamp` returns
/// `UInt32` and `* 1000` widens it to `UInt64` — which Parquet faithfully records as an UNSIGNED
/// 64-bit column, and [`super::map::spot_from_batch`]'s `ts_ms` reader (like every other
/// collector's) accepts `Int64` only. Without the cast every single day fails decode with
/// `missing/!int64 column ts_ms`. `toUnixTimestamp64Milli` — what the sibling
/// [`crate::clickhouse_poly`] queries use — is not available here because it needs a `DateTime64`.
pub fn spot_query(symbol: &str, market: &str, day: &str, next_day: &str) -> String {
    format!(
        "SELECT toInt64(toUnixTimestamp(sec)) * 1000 AS ts_ms, px \
         FROM {DB}.{TABLE} FINAL \
         WHERE symbol = '{sym}' AND market = '{mkt}' \
         AND sec >= '{day} 00:00:00' AND sec < '{next_day} 00:00:00' \
         ORDER BY sec \
         FORMAT Parquet",
        sym = lit(symbol),
        mkt = lit(market),
    )
}

/// Decode a spot Parquet export and append it under `clickhouse:spot:{symbol}:{day}`.
/// Returns quotes written (0 when the day was already ingested).
pub fn ingest_spot_file(
    store: &DataFusionHist,
    path: &Path,
    venue: &str,
    symbol: &str,
    day: &str,
) -> Result<usize, CollectError> {
    let mut ticks: Vec<QuoteTick> = Vec::new();
    for_each_batch(path, |batch| {
        ticks.extend(spot_from_batch(batch)?);
        Ok(())
    })?;
    if ticks.is_empty() {
        return Ok(0);
    }
    Ok(store.append_quotes(
        venue,
        symbol,
        &ticks,
        Some(&format!("clickhouse:spot:{symbol}:{day}")),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spot_query_is_final_and_readonly() {
        let q = spot_query("BTCUSDT", "spot", "2026-06-10", "2026-06-11");
        assert!(q.starts_with("SELECT "), "SELECT-only");
        let up = q.to_uppercase();
        for forbidden in ["INSERT", "DROP", "ALTER", "CREATE", "TRUNCATE"] {
            assert!(!up.contains(forbidden), "{forbidden} must never appear");
        }
        // THE trap: ReplacingMergeTree(_v) without FINAL returns superseded duplicate seconds.
        assert!(q.contains("FROM data_history.spot_1s FINAL"), "FINAL is mandatory: {q}");
        assert!(q.contains("symbol = 'BTCUSDT' AND market = 'spot'"));
        assert!(q.contains("sec >= '2026-06-10 00:00:00' AND sec < '2026-06-11 00:00:00'"));
        // `toInt64` is not cosmetic: without it the projection is UInt64 and every day fails
        // decode with `missing/!int64 column ts_ms` (verified live against the latency box).
        assert!(q.contains("toInt64(toUnixTimestamp(sec)) * 1000 AS ts_ms"), "seconds → epoch ms");
        assert!(q.trim_end().ends_with("FORMAT Parquet"));
    }

    #[test]
    fn spot_query_exports_the_raw_sparse_series() {
        // The ~11 % missing seconds are closed by `fair_value::trailing_sigma`, NOT here — see the
        // module doc. A `WITH FILL`/`INTERPOLATE` clause creeping in would double-interpolate.
        let q = spot_query("BTCUSDT", "spot", "2026-06-10", "2026-06-11");
        assert!(!q.to_uppercase().contains("WITH FILL"));
        assert!(!q.to_uppercase().contains("INTERPOLATE"));
    }

    #[test]
    fn string_literals_are_escaped() {
        let q = spot_query("BTC'USDT", "spot", "2026-06-10", "2026-06-11");
        assert!(q.contains("symbol = 'BTC''USDT'"), "{q}");
    }
}
