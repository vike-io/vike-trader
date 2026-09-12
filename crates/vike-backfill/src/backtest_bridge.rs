//! The backtest bridge: a `vike_data::HistStore` implementation that reads the LIVE Polymarket
//! L2 recorder's ClickHouse tables (`polymarket.book_events` / `polymarket.l1_quotes`)
//! directly — no local Parquet copy, no ingest step. Point `vike_backtest::hist_replay::replay_ticks`
//! at a [`ClickHousePolyHistStore`] and it backtests recorded live Polymarket L2 exactly like any
//! other `HistStore`, because the trait is all `replay_ticks` depends on.
//!
//! Why this crate: it already has the exact plumbing this needs — the `clickhouse-client` subprocess
//! shell + `FORMAT Parquet` export ([`crate::clickhouse_poly::run_export`], READ-only, `SELECT`
//! only) and the bounded row-group Arrow decode ([`crate::arrowutil`]), both proven by
//! [`crate::clickhouse_poly`] (the sibling one-shot backfill of the same `polymarket` DB (the moved tape tables)).
//! Putting the trait impl here — rather than a new backend inside `vike-data` — means zero new
//! crate/feature wiring: `vike-backfill` already carries `vike-data` (`hist-datafusion`, for the
//! Arrow/Parquet types) and `datafusion` (the Arrow re-export) unconditionally, so this module adds
//! no Cargo surface at all, just Rust. The cost is the documented one-way dependency
//! (`vike-backfill` depends on `vike-data`, so this backend can never be reached FROM `vike-data`
//! itself) — acceptable because every intended caller (a backtest harness / an operator's
//! `replay_ticks` script) is already either inside `vike-backfill`'s dependency cone or a leaf binary
//! that can depend on it directly, and the "nothing depends on vike-backfill" crate-doc note is a
//! description of today's callers, not a load-bearing layering rule (`vike-backfill` already depends
//! on `vike-data` + `vike-model` only; it is not — and does not become — a base layer anything
//! else's *library* code needs).
//!
//! Schema (the recorder's deployed tables — see
//! `docs/superpowers/specs/2026-07-24-polymarket-l2-recorder-clickhouse-design.md` and
//! `docs/superpowers/plans/2026-07-24-poly-l2-recorder.md` for the write side, which serializes rows
//! via JSONEachRow with plain-number (not stringified) depth JSON and integer, not `DateTime64`
//! string, `ts`/`local_ts` — the reverse-mapping this module implements):
//!
//! - `book_events(token_id, condition_id, ts Int64, local_ts Int64, seq UInt64,
//!   event_type Enum('book','price_change','trade','tick_size_change','status'), is_snapshot UInt8,
//!   side Enum('none','buy','sell'), price Decimal(9,4), size Decimal(18,6), best_bid Decimal(9,4),
//!   best_ask Decimal(9,4), bids String, asks String, tick_size Decimal(9,4),
//!   status LowCardinality(String))` — one table for book AND trade events; `bids`/`asks` carry
//!   JSON `[[price,size],...]` depth on `event_type='book'` rows only (`""` otherwise).
//! - `l1_quotes(token_id, condition_id, ts Int64, local_ts Int64, bid Decimal(9,4), ask Decimal(9,4),
//!   bid_size Decimal(18,6), ask_size Decimal(18,6))`.
//!
//! Every export query `CAST`s the source `Decimal`/`Enum`/`LowCardinality` columns to
//! `Float64`/`String` (mirroring [`crate::clickhouse_poly::ingest::trades_query`]'s existing
//! `CAST(side AS String)`) so the Arrow decode never has to reason about ClickHouse's Parquet
//! encoding of those types (Decimal128 with a scale, or a dictionary-encoded enum) — every
//! column lands as a plain `Float64Array`/`StringArray`/`Int64Array`, decoded through the SAME
//! [`crate::arrowutil`] typed accessors every other collector uses.
//!
//! ONLY `venue == "polymarket"` is supported (the whole point of `polymarket`); any other venue
//! returns an empty scan rather than an error — `hist_replay::replay_ticks` is documented
//! sparse-tolerant for a symbol with zero rows, so this degrades the same way an out-of-range window
//! would. Read-only: every `HistStore` append/resample method returns `DataError::Io` naming the
//! store read-only — the recorder is the only writer of `polymarket`, and this bridge must never
//! be mistaken for a second one. `scan_symbol_properties` always returns empty — `polymarket`
//! records no instrument-properties series — so the trait's defaulted `properties_as_of` correctly
//! yields `None` and `EngineParams.properties`-driven fill-grid snapping is simply inert for this
//! store (documented default behavior, not a gap).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use datafusion::arrow::record_batch::RecordBatch;

use vike_data::{DataError, ExecFillRow, ExecOrderRow, HistStore, TsRange};
use vike_model::{
    Bar, BookUpdate, BookUpdateKind, EquitySample, QuoteTick, SymbolProperties, TradeTick,
};

use crate::arrowutil::{f64_col, for_each_batch, i64_col, str_col};
use crate::clickhouse_poly::run_export;
use crate::error::CollectError;

/// The ONLY venue this store answers for real data over — see the module doc.
pub const VENUE: &str = "polymarket";
/// The recorder's deployed database name.
pub const DB: &str = "polymarket";

/// Vendor prefix for the shared Arrow accessor error messages (see [`crate::arrowutil`]).
const CTX: &str = "polymarket";

// ---- pure SQL builders (no I/O; unit-tested directly) ------------------------------------------

/// `token_id` rides straight into a single-quoted SQL literal (no bind params over the CLI
/// subprocess boundary — mirrors [`crate::clickhouse_poly::slug_filter`]'s own defensive posture).
/// Standard SQL doubling escapes an embedded `'`; token ids are venue-issued ERC-1155 ids and never
/// legitimately contain one, but a caller passing arbitrary text must not be able to break out of
/// the literal.
fn escape_literal(s: &str) -> String {
    s.replace('\'', "''")
}

/// `TsRange`'s open bounds become the widest representable `BETWEEN` — `local_ts` is a bounded
/// `Int64` column so this never overflows the export.
fn range_bounds(range: TsRange) -> (i64, i64) {
    (range.start.unwrap_or(i64::MIN), range.end.unwrap_or(i64::MAX))
}

/// Export SQL for `scan_book_updates`: every `book_events` row for one token in `range`, including
/// `status` markers and (harmlessly, for the caller to skip) `trade`/`tick_size_change` rows — the
/// decode side is what filters to the kinds `BookUpdate` can represent.
fn book_events_query(db: &str, token_id: &str, range: TsRange) -> String {
    let (start, end) = range_bounds(range);
    format!(
        "SELECT ts, local_ts, CAST(seq AS Int64) AS seq, CAST(event_type AS String) AS event_type, \
         CAST(side AS String) AS side, CAST(price AS Float64) AS price, \
         CAST(size AS Float64) AS size, bids, asks, CAST(tick_size AS Float64) AS tick_size, \
         CAST(status AS String) AS status \
         FROM {db}.book_events \
         WHERE token_id = '{token}' AND local_ts BETWEEN {start} AND {end} \
         ORDER BY local_ts, seq \
         FORMAT Parquet",
        token = escape_literal(token_id),
    )
}

/// Export SQL for `scan_trades`: the `event_type='trade'` slice of `book_events` for one token.
fn trades_query(db: &str, token_id: &str, range: TsRange) -> String {
    let (start, end) = range_bounds(range);
    format!(
        "SELECT ts, local_ts, CAST(price AS Float64) AS price, CAST(size AS Float64) AS size, \
         CAST(side AS String) AS side \
         FROM {db}.book_events \
         WHERE token_id = '{token}' AND event_type = 'trade' \
         AND local_ts BETWEEN {start} AND {end} \
         ORDER BY local_ts, seq \
         FORMAT Parquet",
        token = escape_literal(token_id),
    )
}

/// Export SQL for `scan_quotes`: `l1_quotes` for one token in `range`.
fn quotes_query(db: &str, token_id: &str, range: TsRange) -> String {
    let (start, end) = range_bounds(range);
    format!(
        "SELECT ts, local_ts, CAST(bid AS Float64) AS bid, CAST(ask AS Float64) AS ask, \
         CAST(bid_size AS Float64) AS bid_size, CAST(ask_size AS Float64) AS ask_size \
         FROM {db}.l1_quotes \
         WHERE token_id = '{token}' AND local_ts BETWEEN {start} AND {end} \
         ORDER BY local_ts \
         FORMAT Parquet",
        token = escape_literal(token_id),
    )
}

// ---- pure row decode (no I/O; unit-tested against synthetic Arrow batches) ---------------------

/// Decode a `[[price,size],...]` JSON depth column (the recorder's own encoding — plain JSON
/// numbers, NOT the pmxt archive's stringified-number variant `crate::pmxt::map::parse_levels`
/// handles) into level pairs. Empty string (delta/status/trade rows) and malformed JSON both
/// degrade to an empty depth rather than a decode error — one bad row must not fail a whole scan.
fn parse_levels_json(json: &str) -> Vec<(f64, f64)> {
    if json.is_empty() {
        return Vec::new();
    }
    serde_json::from_str::<Vec<(f64, f64)>>(json).unwrap_or_default()
}

/// Decode one `book_events` export batch into [`BookUpdate`]s, inverting exactly the encoding the
/// live recorder writes (see the module doc). `trade` and `tick_size_change`
/// rows carry no `BookUpdate` of their own and are skipped (trades are read separately via
/// [`trades_from_batch`]); an unrecognized `status` label is skipped defensively rather than
/// guessed at.
fn book_updates_from_batch(b: &RecordBatch, symbol: &str) -> Result<Vec<BookUpdate>, CollectError> {
    let ts = i64_col(b, "ts", CTX)?;
    let local_ts = i64_col(b, "local_ts", CTX)?;
    let seq = i64_col(b, "seq", CTX)?;
    let event_type = str_col(b, "event_type", CTX)?;
    let side = str_col(b, "side", CTX)?;
    let price = f64_col(b, "price", CTX)?;
    let size = f64_col(b, "size", CTX)?;
    let bids = str_col(b, "bids", CTX)?;
    let asks = str_col(b, "asks", CTX)?;
    let tick_size = f64_col(b, "tick_size", CTX)?;
    let status = str_col(b, "status", CTX)?;

    let mut out = Vec::with_capacity(b.num_rows());
    for i in 0..b.num_rows() {
        let kind = match event_type.value(i) {
            "book" => BookUpdateKind::Snapshot,
            "price_change" => BookUpdateKind::Delta,
            "status" => match status.value(i) {
                "gap_start" => BookUpdateKind::GapStart,
                "stale" => BookUpdateKind::Stale,
                "live_resume" => BookUpdateKind::LiveResume,
                _ => continue, // unrecognized status label — skip rather than guess
            },
            // "trade" -> scan_trades' job; "tick_size_change" carries no BookUpdate of its own
            // (mirrors the pmxt mapper: it only updates tracked per-asset state elsewhere).
            _ => continue,
        };
        let (row_bids, row_asks) = match kind {
            BookUpdateKind::Snapshot => {
                (parse_levels_json(bids.value(i)), parse_levels_json(asks.value(i)))
            }
            BookUpdateKind::Delta => match side.value(i) {
                "buy" => (vec![(price.value(i), size.value(i))], Vec::new()),
                "sell" => (Vec::new(), vec![(price.value(i), size.value(i))]),
                _ => (Vec::new(), Vec::new()), // "none" / unexpected — a degenerate empty delta
            },
            // GapStart / Stale / LiveResume carry no levels.
            _ => (Vec::new(), Vec::new()),
        };
        out.push(BookUpdate {
            ts: ts.value(i),
            local_ts: local_ts.value(i),
            seq: seq.value(i).max(0) as u64,
            kind,
            tick_size: tick_size.value(i),
            bids: row_bids,
            asks: row_asks,
            symbol: symbol.to_string(),
        });
    }
    Ok(out)
}

/// Decode one `book_events` (`event_type='trade'`-filtered) export batch into [`TradeTick`]s.
/// `side` is the TAKER side the recorder wrote (`is_buyer_maker` true -> `"sell"`) — inverted here
/// the same way [`crate::clickhouse_poly::map`] already does for the sibling `polymarket`
/// trade tape.
fn trades_from_batch(b: &RecordBatch, symbol: &str) -> Result<Vec<TradeTick>, CollectError> {
    let ts = i64_col(b, "ts", CTX)?;
    let local_ts = i64_col(b, "local_ts", CTX)?;
    let price = f64_col(b, "price", CTX)?;
    let size = f64_col(b, "size", CTX)?;
    let side = str_col(b, "side", CTX)?;

    let mut out = Vec::with_capacity(b.num_rows());
    for i in 0..b.num_rows() {
        out.push(TradeTick {
            ts: ts.value(i),
            local_ts: local_ts.value(i),
            price: price.value(i),
            size: size.value(i),
            is_buyer_maker: side.value(i) == "sell",
            symbol: symbol.to_string(),
        });
    }
    Ok(out)
}

/// Decode one `l1_quotes` export batch into [`QuoteTick`]s.
fn quotes_from_batch(b: &RecordBatch, symbol: &str) -> Result<Vec<QuoteTick>, CollectError> {
    let ts = i64_col(b, "ts", CTX)?;
    let local_ts = i64_col(b, "local_ts", CTX)?;
    let bid = f64_col(b, "bid", CTX)?;
    let ask = f64_col(b, "ask", CTX)?;
    let bid_size = f64_col(b, "bid_size", CTX)?;
    let ask_size = f64_col(b, "ask_size", CTX)?;

    let mut out = Vec::with_capacity(b.num_rows());
    for i in 0..b.num_rows() {
        out.push(QuoteTick {
            ts: ts.value(i),
            local_ts: local_ts.value(i),
            bid: bid.value(i),
            ask: ask.value(i),
            bid_size: bid_size.value(i),
            ask_size: ask_size.value(i),
            symbol: symbol.to_string(),
        });
    }
    Ok(out)
}

// ---- the HistStore impl -------------------------------------------------------------------------

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// A fresh, collision-free export destination under `scratch_root` — reused across concurrent scans
/// (each call gets its own file; nothing is shared), removed after decode regardless of outcome.
///
/// ⚠ **No shared SUBDIRECTORY, deliberately, and the FILENAME is what carries that.** This used to
/// create `<temp>/vike_ch_backtest_bridge/` and write into it — a fixed path shared by every user of
/// the machine. The first user to run it owns the directory, and its default mode then denies
/// everyone else; `create_dir_all` still returns `Ok` on the already-existing directory, so the
/// failure surfaces much later as a bare `Permission denied` on the parquet file, which reads like a
/// disk problem rather than an ownership one. That is what happened on the CI box: CI's `the CI user`
/// created it, and every later run as `the operator` failed `sweep_runs_every_point_over_this_store`.
///
/// The repair was to put the uniquifier in the FILE name rather than in a directory, and that half
/// is unchanged — `vike_ch_backtest_bridge_<tag>_<pid>_<n>_<nanos>.parquet` keeps the grouping the
/// directory used to provide, and a glob still finds every stray file.
///
/// ⚠ **What DID change is the parent**, and only the parent. It was the OS temp directory, chosen
/// then because a per-user or per-pid subdirectory "costs an env read (i.e. a `Library` row in the
/// settings registry)". That reasoning was right about the cost and wrong about the alternative:
/// this function does not read anything now, it takes the root as a PARAMETER — which is what the
/// registry rule actually asks for — and its callers pass
/// `vike_model::state_path::project_tmp_dir_from`'s answer. The OS temp directory had to go for a
/// reason neither user-ownership nor tidiness: inside the container this project is moving to, it is
/// not the host's, does not survive a restart, and cannot be mounted beside the project folder. See
/// [`vike_model::state_path::PROJECT_TMP_DIR`].
///
/// The root is created if it is not there yet — `tmp/` is not a marker, and a fresh project has
/// none. A failure to create it is left to `run_export` to report against the destination path,
/// which is where every other staging failure already surfaces.
fn tmp_export_path(scratch_root: &Path, tag: &str) -> PathBuf {
    let n = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let _ = std::fs::create_dir_all(scratch_root);
    scratch_root
        .join(format!("vike_ch_backtest_bridge_{tag}_{}_{n}_{nanos}.parquet", std::process::id()))
}

/// Read-only `HistStore` over the live Polymarket L2 recorder's ClickHouse tables. Auth is
/// entirely delegated to the `clickhouse-client` CLI's own config (no credentials in code or the
/// workspace `.env`) — the exact posture [`crate::clickhouse_poly`] already documents.
pub struct ClickHousePolyHistStore {
    ch_bin: String,
    db: String,
    scratch_root: PathBuf,
}

impl ClickHousePolyHistStore {
    /// `ch_bin` is usually `"clickhouse-client"` (resolved off `$PATH`); `db` is usually [`DB`].
    /// Both are explicit rather than assumed so a test box with a differently-named binary or a
    /// staging database can still point this at the right place.
    ///
    /// `scratch_root` is `<project>/tmp` — [`vike_model::state_path::project_tmp_dir_from`]'s
    /// answer, which every calling BINARY already has from `vike_backfill::cli::scratch_root`. It is
    /// a parameter rather than something this type resolves because a library must not reach for
    /// global state its caller can neither see nor override
    /// (`crates/vike-ops/tests/settings_registry.rs`'s `LIBRARY_PIN` is the rule;
    /// `crates/vike-research/src/bin/research.rs`'s `default_lightgbm` is the shape — a DELETED
    /// binary, gone with the research crate, kept as the worked example rather than as somewhere to
    /// look; `crates/vike-ops/tests/citation_gate.rs`'s `DEAD_PATH_EXCEPTIONS` carries it).
    ///
    /// ⚠ **There is deliberately no `Default` any more.** It used to supply `clickhouse-client` and
    /// [`DB`], and adding a third field would have meant it also supplying a scratch root — the one
    /// argument that has no defensible default, since guessing it is exactly the defect this
    /// parameter exists to remove. Its single caller now spells all three.
    pub fn new(
        ch_bin: impl Into<String>,
        db: impl Into<String>,
        scratch_root: impl Into<PathBuf>,
    ) -> Self {
        Self { ch_bin: ch_bin.into(), db: db.into(), scratch_root: scratch_root.into() }
    }

    fn read_only<T>() -> Result<T, DataError> {
        Err(DataError::Io(
            "ClickHousePolyHistStore is read-only over the live polymarket recorder tables \
             (writes only ever happen through the recorder itself)"
                .to_string(),
        ))
    }

    /// Run one export query to a fresh temp file, decode it a row group at a time, and remove the
    /// temp file regardless of outcome (bounded memory — never the whole export in one `Vec` copy
    /// beyond the decoded rows themselves).
    fn export_and_decode<T>(
        &self,
        tag: &str,
        sql: &str,
        decode: impl Fn(&RecordBatch) -> Result<Vec<T>, CollectError>,
    ) -> Result<Vec<T>, DataError> {
        let dest = tmp_export_path(&self.scratch_root, tag);
        let result: Result<Vec<T>, CollectError> = (|| {
            run_export(&self.ch_bin, sql, &dest)?;
            let mut out = Vec::new();
            for_each_batch(&dest, |b| {
                out.extend(decode(b)?);
                Ok(())
            })?;
            Ok(out)
        })();
        let _ = std::fs::remove_file(&dest);
        result.map_err(|e| DataError::Query(e.to_string()))
    }
}

impl HistStore for ClickHousePolyHistStore {
    // The catalog pair (`list_series`/`inventory`) is NOT overridden: this bridge fronts a
    // ClickHouse database through fixed per-kind queries and has no enumeration verb to forward,
    // so the trait's refusing default — "this store cannot enumerate its inventory" — is its true
    // answer. The OLD default was an empty `Ok`, which answered "empty store" on the DATABASE's
    // behalf — the database may well hold a catalog; this bridge cannot ask it.

    /// `polymarket` records no bar series — always empty, never an error (the R6 bar-seed step
    /// in `hist_replay::replay_ticks` is optional and degrades cleanly to "no seeded context").
    fn load_bars(
        &self,
        _venue: &str,
        _symbol: &str,
        _interval: &str,
        _range: TsRange,
    ) -> Result<Vec<Bar>, DataError> {
        Ok(Vec::new())
    }

    fn scan_quotes(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<QuoteTick>, DataError> {
        if venue != VENUE {
            return Ok(Vec::new());
        }
        let sql = quotes_query(&self.db, symbol, range);
        self.export_and_decode("quotes", &sql, |b| quotes_from_batch(b, symbol))
    }

    fn scan_trades(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<TradeTick>, DataError> {
        if venue != VENUE {
            return Ok(Vec::new());
        }
        let sql = trades_query(&self.db, symbol, range);
        self.export_and_decode("trades", &sql, |b| trades_from_batch(b, symbol))
    }

    fn scan_book_updates(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<BookUpdate>, DataError> {
        if venue != VENUE {
            return Ok(Vec::new());
        }
        let sql = book_events_query(&self.db, symbol, range);
        self.export_and_decode("book_events", &sql, |b| book_updates_from_batch(b, symbol))
    }

    /// `polymarket` records no instrument-properties series — always empty, so the trait's
    /// defaulted `properties_as_of` correctly yields `None` (fill-grid snapping is inert here).
    fn scan_symbol_properties(
        &self,
        _venue: &str,
        _symbol: &str,
        _range: TsRange,
    ) -> Result<Vec<(i64, SymbolProperties)>, DataError> {
        Ok(Vec::new())
    }

    /// No account equity/exec-log series live in `polymarket` either — same empty-read posture.
    fn scan_equity(
        &self,
        _venue: &str,
        _symbol: &str,
        _range: TsRange,
    ) -> Result<Vec<EquitySample>, DataError> {
        Ok(Vec::new())
    }
    fn scan_exec_fills(&self, _venue: &str, _symbol: &str) -> Result<Vec<ExecFillRow>, DataError> {
        Ok(Vec::new())
    }
    fn scan_exec_orders(
        &self,
        _venue: &str,
        _symbol: &str,
    ) -> Result<Vec<ExecOrderRow>, DataError> {
        Ok(Vec::new())
    }

    // ---- writes: this store is read-only ---------------------------------------------------

    fn append_bars(
        &self,
        _venue: &str,
        _symbol: &str,
        _interval: &str,
        _bars: &[Bar],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Self::read_only()
    }
    fn append_quotes(
        &self,
        _venue: &str,
        _symbol: &str,
        _ticks: &[QuoteTick],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Self::read_only()
    }
    fn append_trades(
        &self,
        _venue: &str,
        _symbol: &str,
        _ticks: &[TradeTick],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Self::read_only()
    }
    fn append_book_updates(
        &self,
        _venue: &str,
        _symbol: &str,
        _updates: &[BookUpdate],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Self::read_only()
    }
    fn append_symbol_properties(
        &self,
        _venue: &str,
        _symbol: &str,
        _rows: &[(i64, SymbolProperties)],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Self::read_only()
    }
    fn append_equity(
        &self,
        _venue: &str,
        _symbol: &str,
        _rows: &[EquitySample],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Self::read_only()
    }
    fn append_exec_fills(
        &self,
        _venue: &str,
        _symbol: &str,
        _rows: &[ExecFillRow],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Self::read_only()
    }
    fn append_exec_orders(
        &self,
        _venue: &str,
        _symbol: &str,
        _rows: &[ExecOrderRow],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Self::read_only()
    }
    fn resample_quotes_to_bars(
        &self,
        _venue: &str,
        _symbol: &str,
        _interval: &str,
        _range: TsRange,
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Self::read_only()
    }
    fn resample_trades_to_bars(
        &self,
        _venue: &str,
        _symbol: &str,
        _interval: &str,
        _range: TsRange,
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Self::read_only()
    }

    // funding / chain stay on the trait's own empty/no-op defaults (`polymarket` holds neither).
}

#[cfg(test)]
mod tests {
    use super::*;
    use datafusion::arrow::array::{Float64Array, Int64Array, StringArray};
    use datafusion::arrow::datatypes::{DataType, Field, Schema};
    use std::sync::Arc;

    /// A bridge for the tests that never reach ClickHouse — every scan below is either a
    /// non-polymarket venue (answered empty before any query is built) or a write (refused).
    ///
    /// This replaces a `Default` impl that supplied the same two values. `Default` went when the
    /// scratch root became a third field: the two strings had obvious defaults and the root does
    /// not, and an impl that quietly picked one would be the exact guess the parameter exists to
    /// remove. The relative path here is deliberate and never created — a test that started
    /// staging would stage where it was run, loudly, rather than into somebody's temp directory.
    fn bridge() -> ClickHousePolyHistStore {
        ClickHousePolyHistStore::new("clickhouse-client", DB, "unused-scratch-root")
    }

    // ---- SQL builders ------------------------------------------------------------------------

    #[test]
    fn book_events_query_is_readonly_and_range_scoped() {
        let q = book_events_query(DB, "TOK1", TsRange::of(100, 200));
        assert!(q.starts_with("SELECT "), "SELECT-only");
        assert!(!q.to_uppercase().contains("INSERT") && !q.to_uppercase().contains("DROP"));
        assert!(q.contains("FROM polymarket.book_events"));
        assert!(q.contains("token_id = 'TOK1'"));
        assert!(q.contains("local_ts BETWEEN 100 AND 200"));
        assert!(q.trim_end().ends_with("FORMAT Parquet"));
        assert!(q.contains("CAST(event_type AS String)"));
        assert!(q.contains("CAST(side AS String)"));
        assert!(q.contains("CAST(status AS String)"));
    }

    #[test]
    fn trades_query_filters_event_type() {
        let q = trades_query(DB, "TOK1", TsRange::of(1, 2));
        assert!(q.contains("event_type = 'trade'"));
        assert!(q.contains("FROM polymarket.book_events"));
    }

    #[test]
    fn quotes_query_reads_l1_quotes_table() {
        let q = quotes_query(DB, "TOK1", TsRange::all());
        assert!(q.contains("FROM polymarket.l1_quotes"));
        // unbounded TsRange -> the widest representable BETWEEN, never a missing clause.
        assert!(q.contains(&format!("BETWEEN {} AND {}", i64::MIN, i64::MAX)));
    }

    #[test]
    fn token_literal_is_escaped() {
        let q = book_events_query(DB, "a'b", TsRange::all());
        assert!(q.contains("token_id = 'a''b'"), "{q}");
    }

    // ---- level JSON decode --------------------------------------------------------------------

    #[test]
    fn parse_levels_json_handles_numbers_empty_and_garbage() {
        assert_eq!(parse_levels_json(""), Vec::<(f64, f64)>::new());
        assert_eq!(
            parse_levels_json("[[0.5,100.0],[0.49,50.0]]"),
            vec![(0.5, 100.0), (0.49, 50.0)]
        );
        assert_eq!(parse_levels_json("not json"), Vec::<(f64, f64)>::new());
    }

    // ---- book_events -> BookUpdate ------------------------------------------------------------

    /// `(ts, local_ts, seq, event_type, side, price, size, bids, asks, tick_size, status)` — the
    /// fixture shape for [`book_events_batch`], named so clippy's type-complexity lint stays quiet
    /// without an `#[allow]`.
    type BookEventFixtureRow<'a> =
        (i64, i64, i64, &'a str, &'a str, f64, f64, &'a str, &'a str, f64, &'a str);

    fn book_events_batch(rows: &[BookEventFixtureRow<'_>]) -> RecordBatch {
        let schema = Schema::new(vec![
            Field::new("ts", DataType::Int64, false),
            Field::new("local_ts", DataType::Int64, false),
            Field::new("seq", DataType::Int64, false),
            Field::new("event_type", DataType::Utf8, false),
            Field::new("side", DataType::Utf8, false),
            Field::new("price", DataType::Float64, false),
            Field::new("size", DataType::Float64, false),
            Field::new("bids", DataType::Utf8, false),
            Field::new("asks", DataType::Utf8, false),
            Field::new("tick_size", DataType::Float64, false),
            Field::new("status", DataType::Utf8, false),
        ]);
        RecordBatch::try_new(
            Arc::new(schema),
            vec![
                Arc::new(Int64Array::from(rows.iter().map(|r| r.0).collect::<Vec<_>>())),
                Arc::new(Int64Array::from(rows.iter().map(|r| r.1).collect::<Vec<_>>())),
                Arc::new(Int64Array::from(rows.iter().map(|r| r.2).collect::<Vec<_>>())),
                Arc::new(StringArray::from(rows.iter().map(|r| r.3).collect::<Vec<_>>())),
                Arc::new(StringArray::from(rows.iter().map(|r| r.4).collect::<Vec<_>>())),
                Arc::new(Float64Array::from(rows.iter().map(|r| r.5).collect::<Vec<_>>())),
                Arc::new(Float64Array::from(rows.iter().map(|r| r.6).collect::<Vec<_>>())),
                Arc::new(StringArray::from(rows.iter().map(|r| r.7).collect::<Vec<_>>())),
                Arc::new(StringArray::from(rows.iter().map(|r| r.8).collect::<Vec<_>>())),
                Arc::new(Float64Array::from(rows.iter().map(|r| r.9).collect::<Vec<_>>())),
                Arc::new(StringArray::from(rows.iter().map(|r| r.10).collect::<Vec<_>>())),
            ],
        )
        .unwrap()
    }

    #[test]
    fn snapshot_row_decodes_full_depth() {
        let b = book_events_batch(&[(
            1_700_000_000_000,
            1_700_000_000_003,
            1,
            "book",
            "none",
            0.0,
            0.0,
            "[[0.5,100.0]]",
            "[[0.51,80.0]]",
            0.01,
            "",
        )]);
        let out = book_updates_from_batch(&b, "TOK").unwrap();
        assert_eq!(out.len(), 1);
        let u = &out[0];
        assert_eq!(u.kind, BookUpdateKind::Snapshot);
        assert_eq!(u.ts, 1_700_000_000_000);
        assert_eq!(u.local_ts, 1_700_000_000_003);
        assert_eq!(u.seq, 1);
        assert_eq!(u.bids, vec![(0.5, 100.0)]);
        assert_eq!(u.asks, vec![(0.51, 80.0)]);
        assert_eq!(u.tick_size, 0.01);
        assert_eq!(u.symbol, "TOK");
    }

    #[test]
    fn delta_row_decodes_the_populated_side() {
        let b = book_events_batch(&[
            (1, 2, 5, "price_change", "buy", 0.42, 7.0, "", "", 0.01, ""),
            (1, 2, 6, "price_change", "sell", 0.60, 0.0, "", "", 0.01, ""),
        ]);
        let out = book_updates_from_batch(&b, "TOK").unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].kind, BookUpdateKind::Delta);
        assert_eq!(out[0].bids, vec![(0.42, 7.0)]);
        assert!(out[0].asks.is_empty());
        assert_eq!(out[1].kind, BookUpdateKind::Delta);
        assert!(out[1].bids.is_empty());
        assert_eq!(out[1].asks, vec![(0.60, 0.0)]);
    }

    #[test]
    fn status_rows_map_to_the_right_kind_with_empty_levels() {
        let b = book_events_batch(&[
            (9, 10, 0, "status", "none", 0.0, 0.0, "", "", 0.0, "gap_start"),
            (9, 11, 0, "status", "none", 0.0, 0.0, "", "", 0.0, "stale"),
            (9, 12, 0, "status", "none", 0.0, 0.0, "", "", 0.0, "live_resume"),
        ]);
        let out = book_updates_from_batch(&b, "TOK").unwrap();
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].kind, BookUpdateKind::GapStart);
        assert_eq!(out[1].kind, BookUpdateKind::Stale);
        assert_eq!(out[2].kind, BookUpdateKind::LiveResume);
        assert!(out.iter().all(|u| u.bids.is_empty() && u.asks.is_empty()));
    }

    #[test]
    fn trade_and_tick_size_change_rows_are_skipped() {
        let b = book_events_batch(&[
            (1, 2, 0, "trade", "sell", 0.5, 1.0, "", "", 0.0, ""),
            (1, 2, 0, "tick_size_change", "none", 0.0, 0.0, "", "", 0.02, ""),
        ]);
        let out = book_updates_from_batch(&b, "TOK").unwrap();
        assert!(out.is_empty(), "trade/tick_size_change carry no BookUpdate: {out:?}");
    }

    // ---- book_events (event_type=trade) -> TradeTick -----------------------------------------

    fn trades_batch(rows: &[(i64, i64, f64, f64, &str)]) -> RecordBatch {
        let schema = Schema::new(vec![
            Field::new("ts", DataType::Int64, false),
            Field::new("local_ts", DataType::Int64, false),
            Field::new("price", DataType::Float64, false),
            Field::new("size", DataType::Float64, false),
            Field::new("side", DataType::Utf8, false),
        ]);
        RecordBatch::try_new(
            Arc::new(schema),
            vec![
                Arc::new(Int64Array::from(rows.iter().map(|r| r.0).collect::<Vec<_>>())),
                Arc::new(Int64Array::from(rows.iter().map(|r| r.1).collect::<Vec<_>>())),
                Arc::new(Float64Array::from(rows.iter().map(|r| r.2).collect::<Vec<_>>())),
                Arc::new(Float64Array::from(rows.iter().map(|r| r.3).collect::<Vec<_>>())),
                Arc::new(StringArray::from(rows.iter().map(|r| r.4).collect::<Vec<_>>())),
            ],
        )
        .unwrap()
    }

    #[test]
    fn trades_from_batch_inverts_the_taker_side_convention() {
        let b = trades_batch(&[
            (1_700_000_000_100, 1_700_000_000_101, 0.95, 3.0, "sell"),
            (1_700_000_000_200, 1_700_000_000_201, 0.10, 1.0, "buy"),
        ]);
        let out = trades_from_batch(&b, "TOK").unwrap();
        assert_eq!(out.len(), 2);
        assert!(out[0].is_buyer_maker, "side=sell -> taker sold -> is_buyer_maker=true");
        assert_eq!(out[0].price, 0.95);
        assert_eq!(out[0].size, 3.0);
        assert_eq!(out[0].symbol, "TOK");
        assert!(!out[1].is_buyer_maker, "side=buy -> taker bought -> is_buyer_maker=false");
    }

    // ---- l1_quotes -> QuoteTick ----------------------------------------------------------------

    #[test]
    fn quotes_from_batch_decodes_l1() {
        let schema = Schema::new(vec![
            Field::new("ts", DataType::Int64, false),
            Field::new("local_ts", DataType::Int64, false),
            Field::new("bid", DataType::Float64, false),
            Field::new("ask", DataType::Float64, false),
            Field::new("bid_size", DataType::Float64, false),
            Field::new("ask_size", DataType::Float64, false),
        ]);
        let b = RecordBatch::try_new(
            Arc::new(schema),
            vec![
                Arc::new(Int64Array::from(vec![1_700_000_000_200_i64])),
                Arc::new(Int64Array::from(vec![1_700_000_000_201_i64])),
                Arc::new(Float64Array::from(vec![0.44_f64])),
                Arc::new(Float64Array::from(vec![0.47_f64])),
                Arc::new(Float64Array::from(vec![10.0_f64])),
                Arc::new(Float64Array::from(vec![8.0_f64])),
            ],
        )
        .unwrap();
        let out = quotes_from_batch(&b, "TOK").unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].bid, 0.44);
        assert_eq!(out[0].ask, 0.47);
        assert_eq!(out[0].bid_size, 10.0);
        assert_eq!(out[0].ask_size, 8.0);
        assert_eq!(out[0].symbol, "TOK");
        assert_eq!(out[0].mid(), (0.44 + 0.47) / 2.0);
    }

    // ---- HistStore surface (no live ClickHouse — venue mismatch + read-only writes) -----------

    #[test]
    fn non_polymarket_venue_scans_are_empty_not_errors() {
        let store = bridge();
        assert_eq!(store.scan_quotes("binance", "BTCUSDT", TsRange::all()).unwrap(), vec![]);
        assert_eq!(store.scan_trades("binance", "BTCUSDT", TsRange::all()).unwrap(), vec![]);
        assert!(store.scan_book_updates("binance", "BTCUSDT", TsRange::all()).unwrap().is_empty());
    }

    #[test]
    fn writes_are_rejected() {
        let store = bridge();
        assert!(store.append_bars("polymarket", "T", "1m", &[], None).is_err());
        assert!(store.append_quotes("polymarket", "T", &[], None).is_err());
        assert!(store.append_trades("polymarket", "T", &[], None).is_err());
        assert!(store.append_book_updates("polymarket", "T", &[], None).is_err());
        assert!(
            store.resample_quotes_to_bars("polymarket", "T", "1m", TsRange::all(), None).is_err()
        );
        assert!(
            store.resample_trades_to_bars("polymarket", "T", "1m", TsRange::all(), None).is_err()
        );
    }

    #[test]
    fn no_properties_no_funding_no_chain_series() {
        let store = bridge();
        assert_eq!(
            store.scan_symbol_properties("polymarket", "T", TsRange::all()).unwrap(),
            vec![]
        );
        assert_eq!(store.properties_as_of("polymarket", "T", 1_000).unwrap(), None);
        assert_eq!(store.scan_funding("polymarket", "T", TsRange::all()).unwrap(), vec![]);
        assert_eq!(store.scan_chain("polymarket", "T", TsRange::all()).unwrap(), vec![]);
    }

    /// `new` stores all three arguments where the scans read them — the pass-through this type's
    /// whole configurability rests on, and the successor to the `Default`-value assertion that
    /// stood here before the scratch root became a parameter.
    #[test]
    fn new_carries_the_client_the_db_and_the_scratch_root_through() {
        let store = ClickHousePolyHistStore::new("ch-alt", "staging_db", "/srv/proj/tmp");
        assert_eq!(store.ch_bin, "ch-alt");
        assert_eq!(store.db, "staging_db");
        assert_eq!(store.scratch_root, PathBuf::from("/srv/proj/tmp"));
    }

    /// …and the scratch root really is what the staged export hangs off, rather than a field the
    /// exporter ignores. Only the PARENT is asserted: the unique FILE name is the repair for a
    /// the CI box permission failure (see [`tmp_export_path`]) and is deliberately not pinned verbatim.
    #[test]
    fn a_staged_export_lands_under_the_scratch_root_it_was_given() {
        let root = std::env::temp_dir().join(format!(
            "vike-bridge-scratch-{}-{}",
            std::process::id(),
            TMP_SEQ.load(Ordering::Relaxed)
        ));
        let dest = tmp_export_path(&root, "quotes");
        assert_eq!(dest.parent(), Some(root.as_path()), "the parent is the root, not the OS temp");
        let name = dest.file_name().expect("a file name").to_string_lossy().into_owned();
        assert!(name.starts_with("vike_ch_backtest_bridge_quotes_"), "got {name}");
        assert!(name.ends_with(".parquet"), "got {name}");
        assert!(root.is_dir(), "the root is created on first use — `tmp/` is not a marker");
        std::fs::remove_dir_all(&root).ok();
    }

    /// STALE-PREMISE REGRESSION: `poly_ch_backtest` used to REFUSE any `[sweep]` profile, claiming
    /// the harness sweep path was keyed to the concrete `DataFusionHist` and had never been relaxed
    /// to the trait object. It takes `Arc<dyn HistStore + Send + Sync>` — the same handle
    /// `run_backtest` takes — and this store satisfies it, which is what this test pins.
    ///
    /// It needs NO ClickHouse: the store is pointed at a deliberately nonexistent client binary, so
    /// every grid point still travels the whole `run_sweep` -> `run_backtest` -> `replay_ticks` ->
    /// `scan_quotes` path and comes back as that row's `error` (a per-point failure is recorded,
    /// never fatal). Asserting the binary NAME appears in each row's error is what proves the point
    /// really reached THIS store, rather than failing earlier on profile/strategy resolution.
    #[cfg(feature = "poly-ch-backtest")]
    #[test]
    fn sweep_runs_every_point_over_this_store() {
        use vike_backtest::harness::{self, BacktestProfile, RankMetric};

        const MISSING_BIN: &str = "vike-no-such-clickhouse-client";

        let store: Arc<dyn HistStore + Send + Sync> =
            Arc::new(ClickHousePolyHistStore::new(MISSING_BIN, DB, "unused-scratch-root"));
        let profile = BacktestProfile::from_toml_str(
            r#"
name = "sweep-premise"
[data]
kind = "tick"
from = "0"
to = "1000"
[[data.series]]
venue = "polymarket"
symbol = "TOK1"
kind = "tick"
[engine]
cash = 1000.0
[strategy]
name = "buy_hold"
[strategy.params]
size = 1.0
[sweep]
size = [1.0, 2.0, 3.0]
"#,
        )
        .expect("the sweep profile parses");
        assert!(profile.is_sweep(), "the fixture must exercise the sweep path");

        let report = harness::run_sweep(&profile, store, RankMetric::Sharpe)
            .expect("a per-point failure is a row error, never a whole-sweep failure");
        assert_eq!(report.rows.len(), 3, "one row per grid point");
        for row in &report.rows {
            let err = row.error.as_deref().unwrap_or_default();
            assert!(
                err.contains(MISSING_BIN),
                "every point must have reached THIS store's clickhouse-client shell, got {err:?}"
            );
        }
    }
}
