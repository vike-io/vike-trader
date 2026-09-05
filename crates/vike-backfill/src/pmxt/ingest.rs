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
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use datafusion::arrow::array::{Array, Decimal128Array, StringArray};
use datafusion::arrow::record_batch::RecordBatch;

use vike_data::{DataFusionHist, HistStore};
use vike_model::{BookUpdate, QuoteTick, TradeTick};

use crate::arrowutil::{decimal_col, for_each_batch, str_col, ts_col};
use crate::error::CollectError;
use crate::pmxt::ch_rows::{book_event_json, l1_quote_json, trade_event_json};
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

// ---- ClickHouse write path ----------------------------------------------------------------------
// The write-side twin of `ingest_file` above: same download/decode/mapper pipeline, but the mapped
// rows are INSERTed into the `polymarket` ClickHouse tables the live L2 recorder writes, instead
// of the DataFusion hist store. See `docs/superpowers/specs/2026-07-24-polymarket-l2-recorder-
// clickhouse-design.md`.

/// Row-count threshold before a batch is flushed to `clickhouse-client` — keeps memory bounded on a
/// 130-400 MB source part (a naive whole-file buffer would hold millions of JSON row strings).
const CH_INSERT_BATCH_ROWS: usize = 20_000;

/// Does `ts_ms` (a row's `local_ts_ms`, the archive's own ingest time — the field
/// `polymarket.book_events.local_ts_dt`/`l1_quotes.local_ts_dt` is materialized from) fall inside
/// `[min, max]`? Both bounds are inclusive; `None` on either side means unbounded on that side, so
/// `(None, None)` accepts every row — the no-filter case, byte-identical to before this existed.
///
/// A free function rather than inlined at the call site because the one thing worth getting
/// exactly right here is the boundary (an off-by-one either re-admits a duplicate-prone minute or
/// silently drops a genuine one), and that is worth a direct, isolated test.
fn in_window(ts_ms: i64, min: Option<i64>, max: Option<i64>) -> bool {
    min.is_none_or(|m| ts_ms >= m) && max.is_none_or(|m| ts_ms <= m)
}

/// Run `ch_bin --query "INSERT INTO {table} FORMAT JSONEachRow"` with `rows` (newline-terminated
/// JSONEachRow objects, see `ch_rows`) piped to stdin. Auth is delegated entirely to the CLI's own
/// config — the same pattern as `crate::clickhouse_poly::ingest::run_export` (the read-side twin);
/// no host/user/password in this code or the workspace `.env`. `SELECT`-only elsewhere in this
/// crate; this is the one place that issues an `INSERT`.
fn ch_insert_batch(ch_bin: &str, table: &str, rows: &str) -> Result<(), CollectError> {
    let mut child = Command::new(ch_bin)
        .arg("--query")
        .arg(format!("INSERT INTO {table} FORMAT JSONEachRow"))
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| CollectError::Fetch(format!("spawn {ch_bin}: {e}")))?;
    child
        .stdin
        .take()
        .expect("stdin piped")
        .write_all(rows.as_bytes())
        .map_err(|e| CollectError::Fetch(format!("write to {ch_bin} stdin: {e}")))?;
    let out =
        child.wait_with_output().map_err(|e| CollectError::Fetch(format!("wait {ch_bin}: {e}")))?;
    if !out.status.success() {
        return Err(CollectError::Fetch(format!(
            "{ch_bin} exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

/// The ClickHouse-write counterpart of [`ingest_file`]: stream-decode one downloaded pmxt Parquet
/// file (bounded memory, one row group at a time — identical decode path to `ingest_file`) and
/// INSERT the mapped rows into the SAME `polymarket.book_events` / `polymarket.l1_quotes`
/// tables the live L2 recorder writes, via `ch_bin` (`clickhouse-client` by default). Because the
/// venue/symbol keying is identical (`venue="polymarket"` implicit in the table, `token_id` =
/// `asset_id`), backfilled pmxt history and the live recorder's own rows land in ONE series per
/// token. `condition_id` is always emitted `""` — pmxt rows are keyed by `asset_id` alone and carry
/// no condition_id of their own (the recorder's own rows fill it in independently for the tokens it
/// discovers via Gamma).
///
/// ⚠ **NOT IDEMPOTENT.** Unlike [`ingest_file`]'s `HistStore` path (a per-`(kind, asset, hour)`
/// commit key makes a re-run of the same hour a no-op), `book_events`/`l1_quotes` are plain
/// append-only tables here — no dedup key is applied on this write path, so re-running the same
/// hour duplicates every row. Callers driving a real backfill range must track which hours were
/// already loaded themselves (e.g. never re-run a completed `[--from, --to]` range) rather than
/// relying on this function to no-op a re-run. `min_local_ts_ms`/`max_local_ts_ms` narrow WHICH
/// rows of an hour get written (see below) — that bounds the BLAST RADIUS of a duplicate write,
/// it does not make one idempotent: re-running the identical window still duplicates every row in
/// it.
///
/// `min_local_ts_ms`/`max_local_ts_ms` (both inclusive, both optional — `(None, None)` is the
/// no-filter case, unchanged from before this parameter pair existed) narrow the write to rows
/// whose `local_ts_ms` falls inside the window, via [`in_window`]. **A filtered-out row is
/// skipped BEFORE `l1_from_row`/`map_row` see it — not decoded further, not folded into
/// `MapState` at all** — which is deliberate, not an oversight: it means a narrow window behaves
/// exactly as if the archive file HAD been trimmed to that window before this function ever saw
/// it (each asset's tick-size/seq/ts-clamp state starts fresh at the first row inside the
/// window, precisely like the start of a file), rather than carrying clamp state accumulated from
/// rows the caller explicitly asked to exclude. This exists so an operator backfilling a
/// PARTIALLY-degraded hour (recorder healthy for part of it, collapsed for the rest) can target
/// only the minutes actually missing, instead of re-inserting a whole hour on top of data that
/// is already there — `--clickhouse` has no per-row dedup, so that re-insertion would otherwise
/// double-count every already-covered minute.
///
/// Returns `(book events written, trades written, L1 quotes written)`.
pub fn ingest_file_clickhouse(
    ch_bin: &str,
    path: &Path,
    tokens: Option<&HashSet<String>>,
    min_local_ts_ms: Option<i64>,
    max_local_ts_ms: Option<i64>,
) -> Result<(usize, usize, usize), CollectError> {
    let mut state = MapState::default();
    let mut book_buf = String::new();
    let mut book_buf_rows = 0usize;
    let mut quote_buf = String::new();
    let mut quote_buf_rows = 0usize;
    let (mut n_books, mut n_trades, mut n_quotes) = (0usize, 0usize, 0usize);

    for_each_batch(path, |batch| {
        for row in rows_from_batch(batch)? {
            if let Some(allow) = tokens
                && !allow.contains(&row.asset_id)
            {
                continue;
            }
            if !in_window(row.local_ts_ms, min_local_ts_ms, max_local_ts_ms) {
                continue;
            }
            // The venue's own top of book on this row — taken BEFORE `map_row` consumes it (same
            // ordering as `ingest_file`).
            let l1 = l1_from_row(&row);
            match map_row(&mut state, &row) {
                Mapped::Book(update) => {
                    book_buf.push_str(&book_event_json(&row.asset_id, "", &update));
                    book_buf_rows += 1;
                    n_books += 1;
                }
                Mapped::Trade(trade) => {
                    // Trades land in the SAME `book_events` table (`event_type:"trade"`), not a
                    // separate one — see `ch_rows::trade_event_json`.
                    book_buf.push_str(&trade_event_json(&row.asset_id, "", &trade));
                    book_buf_rows += 1;
                    n_trades += 1;
                }
                Mapped::None => {}
            }
            if let Some(q) = l1 {
                quote_buf.push_str(&l1_quote_json(&row.asset_id, "", &q));
                quote_buf_rows += 1;
                n_quotes += 1;
            }
            if book_buf_rows >= CH_INSERT_BATCH_ROWS {
                ch_insert_batch(ch_bin, "polymarket.book_events", &book_buf)?;
                book_buf.clear();
                book_buf_rows = 0;
            }
            if quote_buf_rows >= CH_INSERT_BATCH_ROWS {
                ch_insert_batch(ch_bin, "polymarket.l1_quotes", &quote_buf)?;
                quote_buf.clear();
                quote_buf_rows = 0;
            }
        }
        Ok(())
    })?;

    if book_buf_rows > 0 {
        ch_insert_batch(ch_bin, "polymarket.book_events", &book_buf)?;
    }
    if quote_buf_rows > 0 {
        ch_insert_batch(ch_bin, "polymarket.l1_quotes", &quote_buf)?;
    }
    Ok((n_books, n_trades, n_quotes))
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

    // ---- in_window boundary --------------------------------------------------------------------
    // The whole point of the filter is the BOUNDARY: an off-by-one either re-admits a row from a
    // minute the caller was trying to exclude (the 2026-08-30 incident: a duplicate-prone edge
    // that should have been dropped) or silently drops a genuine in-window row (a caller who
    // asked for exactly [start, end] and got a hole at the edges instead). Every case below is
    // named after which failure mode it would have caught.

    #[test]
    fn in_window_no_bounds_admits_everything() {
        assert!(in_window(i64::MIN, None, None));
        assert!(in_window(0, None, None));
        assert!(in_window(i64::MAX, None, None));
    }

    #[test]
    fn in_window_min_bound_is_inclusive_at_the_edge() {
        assert!(in_window(1000, Some(1000), None), "exactly at min must be INCLUDED, not dropped");
        assert!(!in_window(999, Some(1000), None), "one ms below min must be EXCLUDED");
        assert!(in_window(1_000_000, Some(1000), None), "well above min is included");
    }

    #[test]
    fn in_window_max_bound_is_inclusive_at_the_edge() {
        assert!(in_window(2000, None, Some(2000)), "exactly at max must be INCLUDED, not dropped");
        assert!(!in_window(2001, None, Some(2000)), "one ms above max must be EXCLUDED");
        assert!(in_window(-1_000_000, None, Some(2000)), "well below max is included");
    }

    #[test]
    fn in_window_both_bounds_only_admit_the_closed_interval() {
        let (min, max) = (Some(1000), Some(2000));
        assert!(!in_window(999, min, max), "just below the window");
        assert!(in_window(1000, min, max), "left edge, inclusive");
        assert!(in_window(1500, min, max), "interior");
        assert!(in_window(2000, min, max), "right edge, inclusive");
        assert!(!in_window(2001, min, max), "just above the window");
    }

    #[test]
    fn in_window_an_inverted_window_admits_nothing() {
        // The bin refuses `--min-local-ts` after `--max-local-ts` before ever calling this, but
        // the predicate itself must not silently admit anything if that guard is ever bypassed.
        assert!(!in_window(1500, Some(2000), Some(1000)));
    }

    // ---- ClickHouse write path ---------------------------------------------------------------

    /// CI-safe (no real ClickHouse needed): a nonexistent `ch_bin` fails to spawn, and that failure
    /// surfaces as a clean `CollectError::Fetch`, not a panic.
    #[test]
    fn ch_insert_batch_reports_spawn_failure_for_a_missing_binary() {
        let err = ch_insert_batch(
            "this-binary-does-not-exist-vike-pmxt-ch-test",
            "polymarket.book_events",
            "",
        )
        .unwrap_err();
        assert!(matches!(err, CollectError::Fetch(_)));
    }

    /// Real subprocess smoke — needs a live `clickhouse-client` on PATH AND a reachable ClickHouse
    /// server with the `polymarket.book_events` schema already applied. Never run in CI; run
    /// manually: `cargo test -p vike-backfill --lib pmxt::ingest::tests::ch_insert_batch_smoke --
    /// --ignored --nocapture`.
    #[test]
    #[ignore]
    fn ch_insert_batch_smoke() {
        use vike_model::BookUpdateKind;
        let update = BookUpdate {
            ts: 1,
            local_ts: 2,
            seq: 0,
            kind: BookUpdateKind::Snapshot,
            tick_size: 0.01,
            bids: vec![],
            asks: vec![],
            symbol: "SMOKE_TOKEN".into(),
        };
        let row = book_event_json("SMOKE_TOKEN", "", &update);
        ch_insert_batch("clickhouse-client", "polymarket.book_events", &row)
            .expect("real clickhouse-client insert");
    }

    // ---- ingest_file_clickhouse window filter, end to end ---------------------------------------
    // `in_window` above proves the boundary predicate in isolation; this proves the ACTUAL
    // function that a real backfill calls filters correctly through the real decode pipeline —
    // no shortcut, no mock of `rows_from_batch`/`for_each_batch`. Needs no network and no real
    // ClickHouse: a generated shell script stands in for `ch_bin`, consumes the piped JSONEachRow
    // batch and exits 0, so `ingest_file_clickhouse`'s own RETURN COUNTS are the assertion surface
    // rather than anything captured from the subprocess.
    //
    // ⚠ `#[cfg(unix)]` because the stub is a `#!/bin/sh` script marked executable through
    // `PermissionsExt::from_mode` — neither exists on Windows, and the `windows-cross` feature lane
    // cross-compiles this crate `--all-targets` for `x86_64-pc-windows-gnu`, so an ungated version
    // fails the merge gate at COMPILE time (it did: E0433 `cannot find 'unix' in 'os'` + E0599
    // `from_mode`). Nothing is lost by gating: every workspace test runs on the Linux boxes (root
    // CLAUDE.md — "No Windows TEST runs anywhere"), so this only ever needed to COMPILE there.
    #[cfg(unix)]
    mod ch_window_filter {
        use super::*;

        use datafusion::arrow::array::TimestampMillisecondArray;
        use datafusion::arrow::datatypes::{DataType, Field, Schema, TimeUnit};
        use datafusion::parquet::arrow::ArrowWriter;
        use std::os::unix::fs::PermissionsExt;
        use std::sync::Arc;

        /// A stand-in `ch_bin`: a shell script that ignores every argv (`ch_insert_batch` always
        /// calls it as `ch_bin --query "<sql>"`, and a real coreutils tool like `cat` chokes trying
        /// to parse `--query` as one of its OWN options), drains stdin, and exits 0. Written fresh
        /// per test into `dir` rather than shared, so parallel test threads never race on one file.
        fn stub_ch_bin(dir: &Path) -> PathBuf {
            let path = dir.join("stub_ch_bin.sh");
            std::fs::write(&path, "#!/bin/sh\ncat >/dev/null\nexit 0\n")
                .expect("write stub ch_bin");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("chmod +x stub ch_bin");
            path
        }

        /// One synthetic `price_change` row: `(asset_id, timestamp_received_ms)`. `price_change` is
        /// the event type that produces BOTH a book delta AND (via `best_bid`/`best_ask`) an L1
        /// quote per row, so one fixture row exercises both write targets at once. `timestamp`
        /// (the OTHER, clamped column) is set equal to `timestamp_received` for simplicity — this
        /// fixture is testing the WINDOW FILTER, not the clamp, which `map.rs`'s own tests already
        /// cover in isolation.
        fn write_pmxt_fixture(dir: &Path, rows: &[(&str, i64)]) -> PathBuf {
            let schema = Arc::new(Schema::new(vec![
                Field::new("event_type", DataType::Utf8, false),
                Field::new("asset_id", DataType::Utf8, false),
                Field::new("side", DataType::Utf8, false),
                Field::new("bids", DataType::Utf8, true),
                Field::new("asks", DataType::Utf8, true),
                Field::new("timestamp", DataType::Timestamp(TimeUnit::Millisecond, None), false),
                Field::new(
                    "timestamp_received",
                    DataType::Timestamp(TimeUnit::Millisecond, None),
                    false,
                ),
                Field::new("price", DataType::Decimal128(9, 4), false),
                Field::new("size", DataType::Decimal128(18, 6), false),
                Field::new("new_tick_size", DataType::Decimal128(9, 4), true),
                Field::new("best_bid", DataType::Decimal128(9, 4), false),
                Field::new("best_ask", DataType::Decimal128(9, 4), false),
            ]));
            let n = rows.len();
            let ts = TimestampMillisecondArray::from(rows.iter().map(|r| r.1).collect::<Vec<_>>());
            let price =
                Decimal128Array::from(vec![4_200_i128; n]).with_precision_and_scale(9, 4).unwrap();
            let size = Decimal128Array::from(vec![1_000_000_i128; n])
                .with_precision_and_scale(18, 6)
                .unwrap();
            let best_bid =
                Decimal128Array::from(vec![4_100_i128; n]).with_precision_and_scale(9, 4).unwrap();
            let best_ask =
                Decimal128Array::from(vec![4_300_i128; n]).with_precision_and_scale(9, 4).unwrap();
            let batch = RecordBatch::try_new(
                schema.clone(),
                vec![
                    Arc::new(StringArray::from(vec!["price_change"; n])),
                    Arc::new(StringArray::from(rows.iter().map(|r| r.0).collect::<Vec<_>>())),
                    Arc::new(StringArray::from(vec!["BUY"; n])),
                    Arc::new(StringArray::from(vec![None::<&str>; n])),
                    Arc::new(StringArray::from(vec![None::<&str>; n])),
                    Arc::new(ts.clone()),
                    Arc::new(ts),
                    Arc::new(price) as Arc<dyn Array>,
                    Arc::new(size) as Arc<dyn Array>,
                    Arc::new(
                        Decimal128Array::from(vec![None::<i128>; n])
                            .with_precision_and_scale(9, 4)
                            .unwrap(),
                    ) as Arc<dyn Array>,
                    Arc::new(best_bid) as Arc<dyn Array>,
                    Arc::new(best_ask) as Arc<dyn Array>,
                ],
            )
            .expect("build fixture RecordBatch");
            let path = dir.join("fixture.parquet");
            let file = std::fs::File::create(&path).expect("create fixture parquet file");
            let mut writer = ArrowWriter::try_new(file, schema, None).expect("open ArrowWriter");
            writer.write(&batch).expect("write fixture batch");
            writer.close().expect("close fixture writer");
            path
        }

        /// Three rows at local_ts_ms 1000/2000/3000 (one per asset, so each is independently
        /// countable) — the middle one is the ONLY one inside `[1500, 2500]`. Guards against the
        /// bug this whole filter was written to prevent: an off-by-one that either drops the middle
        /// row or admits a neighbor.
        #[test]
        fn ingest_file_clickhouse_narrow_window_admits_only_the_middle_row() {
            let dir = tempfile::tempdir().expect("tempdir");
            let ch_bin = stub_ch_bin(dir.path());
            let path = write_pmxt_fixture(dir.path(), &[("A", 1000), ("B", 2000), ("C", 3000)]);
            let (books, trades, quotes) = ingest_file_clickhouse(
                &ch_bin.to_string_lossy(),
                &path,
                None,
                Some(1500),
                Some(2500),
            )
            .expect("ingest_file_clickhouse with the stub ch_bin");
            assert_eq!(
                (books, trades, quotes),
                (1, 0, 1),
                "only the local_ts=2000 row is in-window"
            );
        }

        #[test]
        fn ingest_file_clickhouse_no_window_admits_every_row() {
            let dir = tempfile::tempdir().expect("tempdir");
            let ch_bin = stub_ch_bin(dir.path());
            let path = write_pmxt_fixture(dir.path(), &[("A", 1000), ("B", 2000), ("C", 3000)]);
            let (books, _trades, quotes) =
                ingest_file_clickhouse(&ch_bin.to_string_lossy(), &path, None, None, None)
                    .expect("ingest_file_clickhouse with the stub ch_bin");
            assert_eq!(
                (books, quotes),
                (3, 3),
                "(None, None) is the pre-existing no-filter behavior"
            );
        }

        #[test]
        fn ingest_file_clickhouse_window_boundary_is_inclusive() {
            let dir = tempfile::tempdir().expect("tempdir");
            let ch_bin = stub_ch_bin(dir.path());
            let path = write_pmxt_fixture(dir.path(), &[("A", 1000), ("B", 2000), ("C", 3000)]);
            // min == max == the middle row's own local_ts: must still admit it (not exclude on the
            // grounds that the window is a single instant).
            let (books, _trades, quotes) = ingest_file_clickhouse(
                &ch_bin.to_string_lossy(),
                &path,
                None,
                Some(2000),
                Some(2000),
            )
            .expect("ingest_file_clickhouse with the stub ch_bin");
            assert_eq!(
                (books, quotes),
                (1, 1),
                "a single-instant window still admits its exact row"
            );
        }

        #[test]
        fn ingest_file_clickhouse_min_only_bounds_the_left_edge() {
            let dir = tempfile::tempdir().expect("tempdir");
            let ch_bin = stub_ch_bin(dir.path());
            let path = write_pmxt_fixture(dir.path(), &[("A", 1000), ("B", 2000), ("C", 3000)]);
            let (books, _trades, quotes) =
                ingest_file_clickhouse(&ch_bin.to_string_lossy(), &path, None, Some(2000), None)
                    .expect("ingest_file_clickhouse with the stub ch_bin");
            assert_eq!((books, quotes), (2, 2), "B (==min) and C (>min) pass; A (<min) is dropped");
        }

        #[test]
        fn ingest_file_clickhouse_max_only_bounds_the_right_edge() {
            let dir = tempfile::tempdir().expect("tempdir");
            let ch_bin = stub_ch_bin(dir.path());
            let path = write_pmxt_fixture(dir.path(), &[("A", 1000), ("B", 2000), ("C", 3000)]);
            let (books, _trades, quotes) =
                ingest_file_clickhouse(&ch_bin.to_string_lossy(), &path, None, None, Some(2000))
                    .expect("ingest_file_clickhouse with the stub ch_bin");
            assert_eq!((books, quotes), (2, 2), "A (<max) and B (==max) pass; C (>max) is dropped");
        }

        /// The `tokens` allowlist and the ts window are two INDEPENDENT filters — both must pass, not
        /// just one. Guards against a wrong `&&`/`||` at the call site.
        #[test]
        fn ingest_file_clickhouse_tokens_filter_and_window_filter_both_apply() {
            let dir = tempfile::tempdir().expect("tempdir");
            let ch_bin = stub_ch_bin(dir.path());
            let path = write_pmxt_fixture(dir.path(), &[("A", 2000), ("B", 2000)]);
            let tokens: HashSet<String> = ["A".to_string()].into_iter().collect();
            let (books, _trades, quotes) = ingest_file_clickhouse(
                &ch_bin.to_string_lossy(),
                &path,
                Some(&tokens),
                Some(1500),
                Some(2500),
            )
            .expect("ingest_file_clickhouse with the stub ch_bin");
            assert_eq!(
                (books, quotes),
                (1, 1),
                "B is in-window but not in the tokens allowlist, and must still be dropped"
            );
        }
    }
}
