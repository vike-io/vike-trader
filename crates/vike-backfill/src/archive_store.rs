//! `ArchiveParquetHistStore` — a `vike_data::HistStore` that reads `data.vike.io` Polymarket
//! archive Parquet files **IN PLACE**, so a tick-replay backtest needs no import step at all.
//!
//! Why this exists: getting one day of BTC-5m archive data into the DataFusion hist store today
//! costs a download PLUS a full decode→re-encode "import" (~1m45s/day single-core, measured —
//! mostly Parquet-encode + zstd + per-series commits, the rest mapping/decode). The data is
//! *already* Parquet and the store is *already* Parquet — the import is a format round-trip that
//! mostly re-encodes what it just decoded. This store skips that round-trip: it opens the archive
//! file(s) directly and answers `HistStore` reads straight off them, at query time. Precedent:
//! [`crate::backtest_bridge::ClickHousePolyHistStore`] does the same thing over ClickHouse (no
//! local copy, no ingest) — this is the local-Parquet-file analogue.
//!
//! **Schema note — this is the "family" day-file layout, not the flat `venue=.../date=...`
//! archive layout [`crate::vike_archive`] documents.** The real files this was built and measured
//! against (`/var/lib/vike/dl/btc5m_YYYY-MM-DD.parquet` on the CI box) are ONE file per UTC day
//! holding the FULL 16-column `book_events` row shape (book/price_change/status/trade all mixed,
//! `token_id`-sorted) rather than three separate `book_events`/`trades`/`l1_quotes` streams. Two
//! real, live-verified divergences from `vike_archive.rs`'s documented flat-layout schema (found
//! independently by the `ingest-profile`/`ingest-parallel` measurement work on the same files, and
//! re-verified here directly against the real file via `clickhouse-local`'s standalone `DESCRIBE
//! TABLE`/`SELECT` — no server, no ClickHouse table needed for a local file):
//!
//! 1. `event_type`/`side` decode as plain Arrow **`Utf8`** here, not the `Binary`
//!    ClickHouse-enum export the flat layout uses. [`StrOrBinCol`] decodes either physical type
//!    transparently so this module tolerates whichever a given file actually carries.
//! 2. There is no separate `l1_quotes`/`trades` file — `trade` rows are just `event_type='trade'`
//!    rows of the SAME schema, with `price`/`size` as `Decimal128(9,4)`/`Decimal128(18,6)` (like
//!    every other row), not the flat layout's separate-stream `Float64` columns.
//!
//! The decode logic below is therefore a DELIBERATE near-duplicate of
//! `vike_archive::book_updates_from_batch`/`parse_levels_json` (same scale divisors, same
//! event_type/status/side conventions) rather than a shared extraction: `vike_archive.rs` is under
//! concurrent edit by two other in-flight branches (`ingest-parallel`/`local-file`) at the time
//! this was written, and this module only needs read-only access to a handful of its PUBLIC items
//! (`VENUE`, `select_row_groups`) — reused directly below, not copied. The duplication is
//! unit-tested to match the documented conventions row-for-row (see `tests` below); pushing this
//! back into one shared helper is a natural follow-up once the concurrent edits land.
//!
//! **Row-group pruning — the whole performance point.** [`select_row_groups_by_ts`] (this module,
//! new) intersects with `vike_archive::select_row_groups`'s existing `token_id`-based pruning: a
//! single-token, single-day query touches only the row groups whose `token_id` MIN/MAX statistics
//! bracket the requested token AND whose `ts` MIN/MAX statistics overlap the requested range — for
//! the `token_id`-sorted family files this workspace has today, that is typically a couple of row
//! groups out of hundreds (measured in
//! `.superpowers/sdd/2026-07-28-poly-mm-latency-batch/archive-store-report.md`), not the whole
//! file. Row-group statistics only bound WHICH groups can contain a match, so every decoded row is
//! still filtered again by exact `token_id`/`ts` — a kept group can (and typically does) carry
//! other tokens' rows mixed in.
//!
//! **Range semantics — scoped by `ts` (the venue/frame clock), not `local_ts`.**
//! `vike_backtest::hist_replay::replay_ticks` calls `scan_book_updates`/`scan_trades` with a
//! `TsRange` it expects scoped the same way `DataFusionHist` scopes it (`col("ts")` — see
//! `vike-data`'s `datafusion_hist/query.rs`); `local_ts` is carried through untouched on every
//! returned row (feed-arrival-order tie-break is `hist_replay`'s own job, downstream of the scan).
//! This is the one place this store's convention differs, deliberately, from
//! `ClickHousePolyHistStore` (which scopes its ClickHouse `BETWEEN` on `local_ts`) — noted here as
//! an observation for whoever eventually reconciles the two, not something this module changes.
//!
//! **Verbs answered vs refused** (same "do not fake data" posture as `ClickHousePolyHistStore`):
//! `scan_book_updates`, `scan_trades` and `scan_quotes` are all real. `scan_quotes` is **derived
//! L1** — folded from this store's own book depth, emitting one `QuoteTick` per update whose top of
//! book moved, exactly as the live Polymarket feed derives L1 from its L2 book.
//!
//! ⚠ It did NOT always do that. `scan_quotes` originally returned `Ok(vec![])` unconditionally,
//! reasoning that a `book_events` ROW carries `best_bid`/`best_ask` but no
//! `best_bid_size`/`best_ask_size`, so a `QuoteTick` could not be built without fabricating a size.
//! The premise held; the conclusion did not. The `bids`/`asks` JSON LADDER carries price AND size
//! per level, so top-of-book yields both honestly — it just cannot be read off a single row,
//! because a `price_change` row holds only the CHANGED levels. It has to be folded through an
//! [`L2Book`], which is what the verb now does.
//!
//! The empty return was actively harmful rather than merely incomplete, because `HistStore` makes
//! `Ok(vec![])` mean "no quotes in this range" — a claim of fact. A tick replay therefore ran with
//! the entire quote lane silently missing and reported healthy-looking numbers: the trailing
//! scalper scored **-2.74%** over 580 markets where the same markets WITH quotes score **-27.64%**
//! (8,288 trades vs 14,910). Ten times better than reality, with nothing to indicate a problem.
//! Treat this as the module's standing warning: in a `HistStore`, "I cannot answer this" must never
//! be spelled the same way as "the answer is none".
//!
//! `load_bars`/`scan_symbol_properties`/`scan_equity`/`scan_exec_fills`/`scan_exec_orders` are
//! always empty (this archive genuinely holds none of those series); every
//! `append_*`/`resample_*_to_bars` call is rejected — this store is READ-ONLY over files an
//! operator downloaded once, never a second writer of them.

use std::collections::HashSet;
use std::fs::File;
use std::path::{Path, PathBuf};

use datafusion::arrow::array::{
    BinaryArray, Decimal128Array, Float64Array, Int64Array, StringArray, UInt64Array,
};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use datafusion::parquet::file::metadata::ParquetMetaData;
use datafusion::parquet::file::statistics::Statistics;

use vike_data::{DataError, ExecFillRow, ExecOrderRow, HistStore, TsRange};
use vike_model::{
    Bar, BookUpdate, BookUpdateKind, EquitySample, L2Book, QuoteTick, SymbolProperties, TradeTick,
};

use crate::vike_archive::{VENUE, select_row_groups};

/// Vendor prefix for error messages (mirrors `crate::vike_archive`/`crate::arrowutil`'s `CTX`
/// convention).
const CTX: &str = "archive-store";

/// `book_events`' fixed decimal scales — identical to `vike_archive::PRICE_SCALE_DIVISOR`/
/// `SIZE_SCALE_DIVISOR` (duplicated per this module's doc: the constant itself is not `pub` there).
/// `price`/`best_bid`/`best_ask`/`tick_size` are scale-4, `size` is scale-6.
const PRICE_SCALE_DIVISOR: f64 = 10_000.0;
const SIZE_SCALE_DIVISOR: f64 = 1_000_000.0;

/// Which of the archive's three per-date streams a Parquet file carries, decided from its SCHEMA by
/// [`ArchiveParquetHistStore::classify`]. Distinct from `vike_archive::Stream`, which names the
/// same three streams on the INGEST side — there the caller declares the kind via `--kind`, here
/// the file is sniffed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamKind {
    /// `l1_quotes`: bid/ask/bid_size/ask_size — the recorder's own captured top of book.
    Quotes,
    /// `trades`: price/size/side as Float64 — the taker tape.
    Trades,
    /// `book_events`: event_type/bids/asks/tick_size — full L2 depth (and, in the family layout,
    /// trade rows too, as `event_type = 'trade'`).
    Book,
}

// ---- Arrow column decode (pure; unit-tested against synthetic batches) --------------------------

fn str_col<'a>(b: &'a RecordBatch, name: &str) -> Result<&'a StringArray, DataError> {
    b.column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<StringArray>())
        .ok_or_else(|| DataError::Query(format!("{CTX}: missing/!string column {name}")))
}

fn i64_col<'a>(b: &'a RecordBatch, name: &str) -> Result<&'a Int64Array, DataError> {
    b.column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<Int64Array>())
        .ok_or_else(|| DataError::Query(format!("{CTX}: missing/!int64 column {name}")))
}

/// The archive's `trades` stream carries `price`/`size` as plain `Float64`, unlike the
/// `book_events` shape's `Decimal128` — see [`ArchiveParquetHistStore::classify`].
fn f64_col<'a>(b: &'a RecordBatch, name: &str) -> Result<&'a Float64Array, DataError> {
    b.column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<Float64Array>())
        .ok_or_else(|| DataError::Query(format!("{CTX}: missing/!float64 column {name}")))
}

fn u64_col<'a>(b: &'a RecordBatch, name: &str) -> Result<&'a UInt64Array, DataError> {
    b.column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<UInt64Array>())
        .ok_or_else(|| DataError::Query(format!("{CTX}: missing/!uint64 column {name}")))
}

fn decimal_col<'a>(b: &'a RecordBatch, name: &str) -> Result<&'a Decimal128Array, DataError> {
    b.column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<Decimal128Array>())
        .ok_or_else(|| DataError::Query(format!("{CTX}: missing/!decimal128 column {name}")))
}

fn dec(arr: &Decimal128Array, i: usize, divisor: f64) -> f64 {
    arr.value(i) as f64 / divisor
}

/// `event_type`/`side` decode as plain `Utf8` on the real family day-files this module targets,
/// but as raw `Binary` on the flat `data.vike.io/archive` layout `vike_archive.rs` documents (the
/// same divergence that crate's own `StrOrBinCol` tolerates) — this enum is this module's
/// independent (duplicated) equivalent, so a file of either physical shape decodes without error.
enum StrOrBinCol<'a> {
    Utf8(&'a StringArray),
    Bin(&'a BinaryArray),
}

impl StrOrBinCol<'_> {
    /// UTF-8(-lossy for the `Binary` case) decode of one row (malformed bytes degrade to `""`
    /// rather than panicking — one bad row must not fail a whole scan).
    fn value(&self, i: usize) -> &str {
        match self {
            StrOrBinCol::Utf8(a) => a.value(i),
            StrOrBinCol::Bin(a) => std::str::from_utf8(a.value(i)).unwrap_or(""),
        }
    }
}

fn str_or_bin_col<'a>(b: &'a RecordBatch, name: &str) -> Result<StrOrBinCol<'a>, DataError> {
    let col = b
        .column_by_name(name)
        .ok_or_else(|| DataError::Query(format!("{CTX}: missing column {name}")))?;
    if let Some(a) = col.as_any().downcast_ref::<StringArray>() {
        return Ok(StrOrBinCol::Utf8(a));
    }
    if let Some(a) = col.as_any().downcast_ref::<BinaryArray>() {
        return Ok(StrOrBinCol::Bin(a));
    }
    Err(DataError::Query(format!("{CTX}: column {name} is neither string nor binary")))
}

/// Decode a `[[price,size],...]` JSON depth column (plain numbers). Empty string (delta/status/
/// trade rows) and malformed JSON both degrade to an empty depth rather than a decode error — one
/// bad row must not fail a whole scan. Identical to `vike_archive::parse_levels_json` (duplicated
/// per this module's doc).
fn parse_levels_json(json: &str) -> Vec<(f64, f64)> {
    if json.is_empty() {
        return Vec::new();
    }
    serde_json::from_str::<Vec<(f64, f64)>>(json).unwrap_or_default()
}

/// Decode one `book_events`-shaped batch into [`BookUpdate`]s for exactly `symbol`, filtered to
/// `range` (scoped by `ts`, per this module's doc). `trade`/`tick_size_change` rows and an
/// unrecognized `status` label are skipped (`trade` rows are [`trades_from_batch`]'s job).
fn book_updates_from_batch(
    b: &RecordBatch,
    symbol: &str,
    range: TsRange,
) -> Result<Vec<BookUpdate>, DataError> {
    let token_id = str_col(b, "token_id")?;
    let ts = i64_col(b, "ts")?;
    let local_ts = i64_col(b, "local_ts")?;
    let seq = u64_col(b, "seq")?;
    let event_type = str_or_bin_col(b, "event_type")?;
    let side = str_or_bin_col(b, "side")?;
    let price = decimal_col(b, "price")?;
    let size = decimal_col(b, "size")?;
    let bids = str_col(b, "bids")?;
    let asks = str_col(b, "asks")?;
    let tick_size = decimal_col(b, "tick_size")?;
    let status = str_col(b, "status")?;

    let start = range.start.unwrap_or(i64::MIN);
    let end = range.end.unwrap_or(i64::MAX);

    let mut out = Vec::new();
    for i in 0..b.num_rows() {
        if token_id.value(i) != symbol {
            continue;
        }
        let row_ts = ts.value(i);
        if row_ts < start || row_ts > end {
            continue;
        }
        let kind = match event_type.value(i) {
            "book" => BookUpdateKind::Snapshot,
            "price_change" => BookUpdateKind::Delta,
            "status" => match status.value(i) {
                "gap_start" => BookUpdateKind::GapStart,
                "stale" => BookUpdateKind::Stale,
                "live_resume" => BookUpdateKind::LiveResume,
                _ => continue, // unrecognized status label — skip rather than guess
            },
            // "trade" -> `trades_from_batch`'s job; "tick_size_change" carries no `BookUpdate`.
            _ => continue,
        };
        let (row_bids, row_asks) = match kind {
            BookUpdateKind::Snapshot => {
                (parse_levels_json(bids.value(i)), parse_levels_json(asks.value(i)))
            }
            BookUpdateKind::Delta => {
                let level = (dec(price, i, PRICE_SCALE_DIVISOR), dec(size, i, SIZE_SCALE_DIVISOR));
                match side.value(i) {
                    "buy" => (vec![level], Vec::new()),
                    "sell" => (Vec::new(), vec![level]),
                    _ => (Vec::new(), Vec::new()), // "none" / unexpected — a degenerate empty delta
                }
            }
            _ => (Vec::new(), Vec::new()), // GapStart / Stale / LiveResume carry no levels
        };
        out.push(BookUpdate {
            ts: row_ts,
            local_ts: local_ts.value(i),
            seq: seq.value(i),
            kind,
            tick_size: dec(tick_size, i, PRICE_SCALE_DIVISOR),
            bids: row_bids,
            asks: row_asks,
            symbol: symbol.to_string(),
        });
    }
    Ok(out)
}

/// Decode one `book_events`-shaped batch's `event_type='trade'` rows into [`TradeTick`]s for
/// exactly `symbol`, filtered to `range`. `side` is the TAKER side: `"sell"` -> the taker sold ->
/// `is_buyer_maker = true` (the same convention `vike_archive::trades_from_batch` and
/// `backtest_bridge::trades_from_batch` both use for the flat/ClickHouse layouts).
/// Decode one `l1_quotes` batch into [`QuoteTick`]s for `symbol` in `range`.
///
/// This is the archive's OWN recorded top-of-book — the `poly-l2-recorder` writes L1 and L2 as
/// separate streams, and every family/flat date partition ships `l1_quotes.parquet` beside
/// `book_events.parquet`. Same scale divisors as every other column here (`bid`/`ask` scale-4,
/// sizes scale-6), matching `vike_archive::quotes_from_batch`, which decodes the identical schema
/// on the ingest side.
fn quotes_from_l1_batch(
    b: &RecordBatch,
    symbol: &str,
    range: TsRange,
) -> Result<Vec<QuoteTick>, DataError> {
    let token_id = str_col(b, "token_id")?;
    let ts = i64_col(b, "ts")?;
    let local_ts = i64_col(b, "local_ts")?;
    let bid = decimal_col(b, "bid")?;
    let ask = decimal_col(b, "ask")?;
    let bid_size = decimal_col(b, "bid_size")?;
    let ask_size = decimal_col(b, "ask_size")?;

    let start = range.start.unwrap_or(i64::MIN);
    let end = range.end.unwrap_or(i64::MAX);

    let mut out = Vec::new();
    for i in 0..b.num_rows() {
        if token_id.value(i) != symbol {
            continue;
        }
        let row_ts = ts.value(i);
        if row_ts < start || row_ts > end {
            continue;
        }
        out.push(QuoteTick {
            ts: row_ts,
            local_ts: local_ts.value(i),
            bid: dec(bid, i, PRICE_SCALE_DIVISOR),
            ask: dec(ask, i, PRICE_SCALE_DIVISOR),
            bid_size: dec(bid_size, i, SIZE_SCALE_DIVISOR),
            ask_size: dec(ask_size, i, SIZE_SCALE_DIVISOR),
            symbol: symbol.to_string(),
        });
    }
    Ok(out)
}

fn trades_from_batch(
    b: &RecordBatch,
    symbol: &str,
    range: TsRange,
) -> Result<Vec<TradeTick>, DataError> {
    let token_id = str_col(b, "token_id")?;
    let ts = i64_col(b, "ts")?;
    let local_ts = i64_col(b, "local_ts")?;
    let event_type = str_or_bin_col(b, "event_type")?;
    let side = str_or_bin_col(b, "side")?;
    let price = decimal_col(b, "price")?;
    let size = decimal_col(b, "size")?;

    let start = range.start.unwrap_or(i64::MIN);
    let end = range.end.unwrap_or(i64::MAX);

    let mut out = Vec::new();
    for i in 0..b.num_rows() {
        if token_id.value(i) != symbol || event_type.value(i) != "trade" {
            continue;
        }
        let row_ts = ts.value(i);
        if row_ts < start || row_ts > end {
            continue;
        }
        out.push(TradeTick {
            ts: row_ts,
            local_ts: local_ts.value(i),
            price: dec(price, i, PRICE_SCALE_DIVISOR),
            size: dec(size, i, SIZE_SCALE_DIVISOR),
            is_buyer_maker: side.value(i) == "sell",
            symbol: symbol.to_string(),
        });
    }
    Ok(out)
}

// ---- row-group pruning (pure over already-loaded metadata; unit-tested with synthetic files) ----

/// Which row-group indices of `md` can possibly overlap `range`, using the `col_name` column's
/// per-row-group MIN/MAX statistics (must be a physical `INT64` column — true of both `ts` and
/// `local_ts` here). Falls back to "include everything" whenever pruning data is unavailable (no
/// such column, no statistics on a group's chunk, or the column is not `Int64`-typed) — pruning is
/// an optimization, never a filter that can silently lose rows. Mirrors
/// `vike_archive::select_row_groups`'s own defensive shape (byte-range containment there; typed
/// numeric overlap here), the `ts`-axis sibling of that `token_id`-axis pruning.
fn select_row_groups_by_ts(md: &ParquetMetaData, col_name: &str, range: TsRange) -> Vec<usize> {
    let total = md.num_row_groups();
    let start = range.start.unwrap_or(i64::MIN);
    let end = range.end.unwrap_or(i64::MAX);
    let Some(col_idx) =
        md.file_metadata().schema_descr().columns().iter().position(|c| c.name() == col_name)
    else {
        return (0..total).collect(); // no such column — can't prune, include everything
    };
    (0..total)
        .filter(|&i| {
            let rg = md.row_group(i);
            let Some(stats) = rg.column(col_idx).statistics() else {
                return true; // no stats on this group's chunk — include defensively
            };
            let Statistics::Int64(vs) = stats else {
                return true; // not an Int64-typed statistics — can't reason about it, include
            };
            match (vs.min_opt(), vs.max_opt()) {
                (Some(&min), Some(&max)) => min <= end && max >= start, // range overlap
                _ => true,
            }
        })
        .collect()
}

/// The row groups that could possibly hold `symbol`'s rows within `range`: the intersection of
/// `vike_archive::select_row_groups`'s `token_id` pruning (reused, read-only) and this module's own
/// `ts`-column pruning. Order is preserved ascending (both inputs are ascending; a `HashSet`
/// membership test over the smaller side keeps this linear).
fn select_row_groups_for(md: &ParquetMetaData, symbol: &str, range: TsRange) -> Vec<usize> {
    let mut tokens = HashSet::with_capacity(1);
    tokens.insert(symbol.to_string());
    let by_token = select_row_groups(md, Some(&tokens));
    let by_ts = select_row_groups_by_ts(md, "ts", range);
    let by_ts_set: HashSet<usize> = by_ts.into_iter().collect();
    by_token.into_iter().filter(|i| by_ts_set.contains(i)).collect()
}

// ---- the pruning-plan diagnostic (the MEASURE surface) -------------------------------------------

/// One file's row-group-pruning plan for a `(symbol, range)` query — how many of a file's row
/// groups would actually be read vs the whole file, and their compressed byte cost. Mirrors
/// `vike_archive::PrunePlan`'s shape (this module's local, file-path-carrying twin).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePrunePlan {
    pub path: PathBuf,
    pub total_row_groups: usize,
    pub selected_row_groups: usize,
    pub total_compressed_bytes: i64,
    pub selected_compressed_bytes: i64,
}

// ---- the HistStore impl ---------------------------------------------------------------------------

/// Read-only `HistStore` over one or more LOCAL `data.vike.io` archive Parquet files (the family
/// day-file layout — see the module doc), read directly with no import step. `Send + Sync` (a
/// `Vec<PathBuf>` only) so it works as `Arc<dyn HistStore + Send + Sync>`, exactly like
/// `run_backtest`/`hist_replay::replay_ticks` take.
pub struct ArchiveParquetHistStore {
    /// Files this store reads from, sorted by path — for the `{prefix}_{YYYY-MM-DD}.parquet`
    /// family naming convention, a lexicographic sort is also a chronological one, so multi-day
    /// scan results merge in a sane (if not load-bearing — every scan re-sorts its own output by
    /// `ts` before returning) order.
    files: Vec<PathBuf>,
    /// `files` split by SCHEMA into the archive's three streams. Done ONCE at construction rather
    /// than per scan, and three-way rather than two-way: a date partition ships `book_events`,
    /// `l1_quotes` AND `trades`, and each has a genuinely different shape. Feeding the wrong one to
    /// a decoder is a hard error, not an empty result — a two-way split (quotes vs everything else)
    /// put `trades.parquet` in the book set and every scan died on `missing column event_type`.
    /// Classification reads footers only — no row data.
    quote_files: Vec<PathBuf>,
    trade_files: Vec<PathBuf>,
    book_files: Vec<PathBuf>,
}

impl ArchiveParquetHistStore {
    /// Construct from an explicit, caller-supplied set of local Parquet files (any order).
    pub fn from_files(files: impl IntoIterator<Item = PathBuf>) -> Self {
        let mut files: Vec<PathBuf> = files.into_iter().collect();
        files.sort();
        let mut quote_files = Vec::new();
        let mut trade_files = Vec::new();
        let mut book_files = Vec::new();
        for p in &files {
            match Self::classify(p) {
                StreamKind::Quotes => quote_files.push(p.clone()),
                StreamKind::Trades => trade_files.push(p.clone()),
                StreamKind::Book => book_files.push(p.clone()),
            }
        }
        Self { files, quote_files, trade_files, book_files }
    }

    /// Construct from every `*.parquet` file directly under `dir` (non-recursive). A caller who
    /// wants a narrower set (e.g. a shell glob like `btc5m_2026-07-*.parquet`) should build that
    /// list themselves and use [`Self::from_files`] instead — this constructor is the "just point
    /// me at the whole drop directory" convenience.
    pub fn from_dir(dir: impl AsRef<Path>) -> Result<Self, DataError> {
        let dir = dir.as_ref();
        let entries = std::fs::read_dir(dir)
            .map_err(|e| DataError::Io(format!("{CTX}: read_dir {}: {e}", dir.display())))?;
        let mut files = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| {
                DataError::Io(format!("{CTX}: read_dir entry {}: {e}", dir.display()))
            })?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("parquet") {
                files.push(path);
            }
        }
        Ok(Self::from_files(files))
    }

    /// The files this store was constructed over, in scan order.
    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }

    fn read_only<T>() -> Result<T, DataError> {
        Err(DataError::Io(format!(
            "{CTX}: read-only over local archive Parquet files (no writer — an operator \
             re-downloads to refresh them)"
        )))
    }

    fn open_builder(path: &Path) -> Result<ParquetRecordBatchReaderBuilder<File>, DataError> {
        let file = File::open(path)
            .map_err(|e| DataError::Io(format!("{CTX}: open {}: {e}", path.display())))?;
        ParquetRecordBatchReaderBuilder::try_new(file)
            .map_err(|e| DataError::Query(format!("{CTX}: parquet open {}: {e}", path.display())))
    }

    /// Which of the archive's three streams is this file?
    ///
    /// Detected from the SCHEMA, never the filename. An operator renames these constantly — the
    /// file this store was built against is `btc5m_2026-07-26.parquet`, not `book_events.parquet` —
    /// so filename inference would be wrong exactly when it matters. `vike_archive_backfill`'s
    /// `--file` doc makes the same argument for refusing to guess a stream from a name.
    ///
    /// The three shapes, all sharing `token_id`/`condition_id`/`ts`/`local_ts`:
    ///   - `l1_quotes`   — `bid`, `ask`, `bid_size`, `ask_size`
    ///   - `trades`      — `price`, `size`, `side` (Float64), NO `event_type`
    ///   - `book_events` — `event_type`, `bids`, `asks`, `tick_size` (Decimal prices)
    ///
    /// Unrecognised (or unreadable) falls through to `Book`, so the book decoder reports the real
    /// schema error rather than this silently dropping the file.
    fn classify(path: &Path) -> StreamKind {
        let Ok(b) = Self::open_builder(path) else {
            return StreamKind::Book;
        };
        let s = b.schema();
        let has = |c: &str| s.field_with_name(c).is_ok();
        if has("bid") && has("ask") && has("bid_size") && has("ask_size") {
            StreamKind::Quotes
        } else if has("price") && has("size") && has("side") && !has("event_type") {
            StreamKind::Trades
        } else {
            StreamKind::Book
        }
    }

    /// Decode the archive's own `trades` stream (Float64 `price`/`size`, unlike the book file's
    /// Decimal128) — the same schema `vike_archive::trades_from_batch` ingests, including its
    /// `side == "sell"` ⇒ `is_buyer_maker` taker-side inversion.
    fn scan_recorded_trades(
        &self,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<TradeTick>, DataError> {
        let start = range.start.unwrap_or(i64::MIN);
        let end = range.end.unwrap_or(i64::MAX);
        let mut out = Vec::new();
        for path in &self.trade_files {
            let builder = Self::open_builder(path)?;
            let selected = select_row_groups_for(builder.metadata(), symbol, range);
            if selected.is_empty() {
                continue;
            }
            let reader = builder.with_row_groups(selected).build().map_err(|e| {
                DataError::Query(format!("{CTX}: build reader {}: {e}", path.display()))
            })?;
            for batch in reader {
                let b = batch.map_err(|e| {
                    DataError::Query(format!("{CTX}: read batch {}: {e}", path.display()))
                })?;
                let token_id = str_col(&b, "token_id")?;
                let ts = i64_col(&b, "ts")?;
                let local_ts = i64_col(&b, "local_ts")?;
                let price = f64_col(&b, "price")?;
                let size = f64_col(&b, "size")?;
                let side = str_or_bin_col(&b, "side")?;
                for i in 0..b.num_rows() {
                    if token_id.value(i) != symbol {
                        continue;
                    }
                    let row_ts = ts.value(i);
                    if row_ts < start || row_ts > end {
                        continue;
                    }
                    out.push(TradeTick {
                        ts: row_ts,
                        local_ts: local_ts.value(i),
                        price: price.value(i),
                        size: size.value(i),
                        is_buyer_maker: side.value(i) == "sell",
                        symbol: symbol.to_string(),
                    });
                }
            }
        }
        out.sort_by_key(|t| t.ts);
        Ok(out)
    }

    /// Decode `l1_quotes` rows for `symbol` in `range` straight out of the archive's own stream —
    /// the recorder's captured L1, not a reconstruction.
    fn scan_recorded_quotes(
        &self,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<QuoteTick>, DataError> {
        let mut out = Vec::new();
        for path in &self.quote_files {
            let builder = Self::open_builder(path)?;
            let selected = select_row_groups_for(builder.metadata(), symbol, range);
            if selected.is_empty() {
                continue;
            }
            let reader = builder.with_row_groups(selected).build().map_err(|e| {
                DataError::Query(format!("{CTX}: build reader {}: {e}", path.display()))
            })?;
            for batch in reader {
                let batch = batch.map_err(|e| {
                    DataError::Query(format!("{CTX}: read batch {}: {e}", path.display()))
                })?;
                out.extend(quotes_from_l1_batch(&batch, symbol, range)?);
            }
        }
        out.sort_by_key(|q| q.ts);
        Ok(out)
    }

    /// The row-group-pruning plan for `(symbol, range)` across every file this store holds — the
    /// `--dry-run` "how much would actually be read" report the MEASURE work in
    /// `.superpowers/sdd/2026-07-28-poly-mm-latency-batch/archive-store-report.md` is built on.
    /// Opens each file's footer only (no row data) — cheap regardless of how many files this store
    /// holds.
    pub fn plan(&self, symbol: &str, range: TsRange) -> Result<Vec<FilePrunePlan>, DataError> {
        let mut out = Vec::with_capacity(self.book_files.len());
        for path in &self.book_files {
            let builder = Self::open_builder(path)?;
            let md = builder.metadata();
            let total_row_groups = md.num_row_groups();
            let selected = select_row_groups_for(md, symbol, range);
            let total_compressed_bytes: i64 =
                (0..total_row_groups).map(|i| md.row_group(i).compressed_size()).sum();
            let selected_compressed_bytes: i64 =
                selected.iter().map(|&i| md.row_group(i).compressed_size()).sum();
            out.push(FilePrunePlan {
                path: path.clone(),
                total_row_groups,
                selected_row_groups: selected.len(),
                total_compressed_bytes,
                selected_compressed_bytes,
            });
        }
        Ok(out)
    }

    fn scan_book_updates_impl(
        &self,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<BookUpdate>, DataError> {
        let mut out = Vec::new();
        for path in &self.book_files {
            let builder = Self::open_builder(path)?;
            let selected = select_row_groups_for(builder.metadata(), symbol, range);
            if selected.is_empty() {
                continue;
            }
            let reader = builder.with_row_groups(selected).build().map_err(|e| {
                DataError::Query(format!("{CTX}: build reader {}: {e}", path.display()))
            })?;
            for batch in reader {
                let batch = batch.map_err(|e| {
                    DataError::Query(format!("{CTX}: read batch {}: {e}", path.display()))
                })?;
                out.extend(book_updates_from_batch(&batch, symbol, range)?);
            }
        }
        // (ts, seq) — the same order `DataFusionHist::scan_book_updates` documents (BookCodec's
        // sort key); a stable sort preserves within-event level order from decode.
        out.sort_by(|a, b| a.ts.cmp(&b.ts).then(a.seq.cmp(&b.seq)));
        Ok(out)
    }

    fn scan_trades_impl(&self, symbol: &str, range: TsRange) -> Result<Vec<TradeTick>, DataError> {
        // PREFER the archive's own `trades` stream when this store holds it, exactly as
        // `scan_quotes` prefers `l1_quotes`. Falling through to the book file's own
        // `event_type = 'trade'` rows is the narrower case of holding only that file — the two
        // carry the same tape (verified: 1,159 trades for the same token/window either way).
        if !self.trade_files.is_empty() {
            return self.scan_recorded_trades(symbol, range);
        }
        let mut out = Vec::new();
        for path in &self.book_files {
            let builder = Self::open_builder(path)?;
            let selected = select_row_groups_for(builder.metadata(), symbol, range);
            if selected.is_empty() {
                continue;
            }
            let reader = builder.with_row_groups(selected).build().map_err(|e| {
                DataError::Query(format!("{CTX}: build reader {}: {e}", path.display()))
            })?;
            for batch in reader {
                let batch = batch.map_err(|e| {
                    DataError::Query(format!("{CTX}: read batch {}: {e}", path.display()))
                })?;
                out.extend(trades_from_batch(&batch, symbol, range)?);
            }
        }
        out.sort_by_key(|t| t.ts);
        Ok(out)
    }
}

impl HistStore for ArchiveParquetHistStore {
    // The catalog pair (`list_series`/`inventory`) is NOT overridden: this store reads the fixed
    // set of archive files handed to its constructor and has no manifest to fold a catalog from,
    // so the trait's refusing default — "this store cannot enumerate its inventory" — is its true
    // answer. The OLD default was an empty `Ok`, which had this leaf claiming "empty store" while
    // holding a full trade/quote/book archive; its replay callers read the lanes directly and
    // never asked, which is the only reason the fabrication cost nothing here.

    /// This archive holds no bar series — always empty, never an error (the R6 bar-seed step in
    /// `hist_replay::replay_ticks` is optional and degrades cleanly to "no seeded context").
    fn load_bars(
        &self,
        _venue: &str,
        _symbol: &str,
        _interval: &str,
        _range: TsRange,
    ) -> Result<Vec<Bar>, DataError> {
        Ok(Vec::new())
    }

    /// **Derived L1** — folded from this store's own [`Self::scan_book_updates`] depth, exactly as
    /// the live Polymarket feed derives L1 from its L2 book.
    ///
    /// This verb previously returned `Ok(vec![])` unconditionally, on the reasoning that a
    /// `book_events` ROW carries `best_bid`/`best_ask` but no `best_bid_size`/`best_ask_size`, so a
    /// `QuoteTick` could not be built without fabricating a size. The premise was right and the
    /// conclusion was wrong: the `bids`/`asks` JSON LADDER carries price AND size per level, so
    /// top-of-book yields both without fabricating anything — it just cannot be read off a single
    /// row, because a `price_change` row holds only the CHANGED levels. It has to be folded.
    ///
    /// Returning empty was actively harmful, not merely incomplete. `HistStore`'s contract makes
    /// `Ok(vec![])` mean "this symbol had no quotes in this range" — a fact — so a tick replay
    /// silently ran with the entire quote lane missing. Measured cost of that: the trailing scalper
    /// scored **-2.74%** over 580 markets against **-27.64%** from the same markets with quotes
    /// present (8,288 trades vs 14,910). A backtest read 10x better than reality and looked
    /// perfectly healthy doing it.
    ///
    /// Emission rule: one `QuoteTick` per book update whose TOP OF BOOK changed (price or size, on
    /// either side), timestamped with that update's `ts`/`local_ts`. Unchanged-L1 updates (a deeper
    /// level moving) emit nothing, which is what makes this a quote lane rather than a copy of the
    /// book lane. Status-marker kinds carry no levels and cannot change L1, so they are inert.
    ///
    /// ⚠ This is NOT bit-identical to the separate `l1_quotes` stream the flat archive layout ships
    /// and `vike_archive::quotes_from_batch` decodes — that stream is the recorder's own L1 capture,
    /// with its own timestamps and its own dedup. Derived L1 is the best this file can answer, and
    /// it is a different (defensible) input, not a reproduction. A run comparing this store against
    /// one whose quotes came from `l1_quotes` or from ClickHouse must expect them to differ.
    fn scan_quotes(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<QuoteTick>, DataError> {
        if venue != VENUE {
            return Ok(Vec::new());
        }
        // PREFER the archive's own recorded L1. Every date partition — flat or family — ships
        // `l1_quotes.parquet` beside `book_events.parquet`, because the `poly-l2-recorder` writes
        // L1 and L2 as separate streams. When that file is among this store's files, use it: it is
        // what the venue actually published, and it is BIT-IDENTICAL to what
        // `vike_archive_backfill --kind quote` ingests, so a read-in-place run and an imported run
        // agree exactly (verified: 12,687 quotes and 1,159 trades for the same token/window, both
        // paths, and all 28 backtest rows identical).
        //
        // Deriving from book depth is the FALLBACK for the narrower case of holding only the book
        // file. It is a faithful reconstruction after the anchor fixes below, but a reconstruction:
        // it cannot emit before the first snapshot anchors, so it starts later than the recorder's
        // own capture and differs from it by a fraction of a percent.
        if !self.quote_files.is_empty() {
            return self.scan_recorded_quotes(symbol, range);
        }
        let updates = self.scan_book_updates_impl(symbol, range)?;
        let mut out: Vec<QuoteTick> = Vec::new();
        let mut last: Option<(f64, f64, f64, f64)> = None;
        // The book is built LAZILY from the first update carrying a real `tick_size`, never
        // `L2Book::new(0.0)`. The book stores tick INDICES and converts back with
        // `price_of(tick) = tick * tick_size`, and the constructor silently clamps a non-positive
        // tick_size to 1.0 — which would round every 0.xx probability to 0 or 1 and hand the
        // strategy a book of garbage prices instead of an obvious failure.
        let mut book: Option<L2Book> = None;
        // Emission is ANCHOR-GATED: nothing is emitted from a book that has never folded a
        // `Snapshot`. A range scan of a delta stream starts mid-flight with an EMPTY book, so until
        // an anchor arrives the fold only knows the levels that happen to have been touched — its
        // "top of book" is the best of a partial ladder, not the real one. Measured on a real
        // market open (`derived_l1_divergence_report`): the fold reported 0.42/0.65, then
        // 0.42/0.56, 0.49/0.56, 0.49/0.55 while the truth was 0.50/0.51, and became exact at the
        // very millisecond the first snapshot landed — 4 wrong quotes, then event-for-event
        // agreement for the remaining 12,687. `vike_model::BookUpdateKind::Snapshot`'s own doc says
        // it: "Replay seeks start here."
        let mut anchored = false;
        // Did this range contain any book updates at all? Distinguishes "nothing here" from
        // "something here but never anchored", which the fallback below needs to tell apart.
        let mut saw_levels = false;
        for u in &updates {
            let bk = match book {
                Some(ref mut b) => b,
                None if u.tick_size > 0.0 => book.insert(L2Book::new(u.tick_size)),
                // No book yet, and this update cannot establish the price scale — skip it.
                None => continue,
            };
            match u.kind {
                BookUpdateKind::Snapshot => {
                    bk.apply_snapshot(u.seq, &u.bids, &u.asks);
                    anchored = true;
                    saw_levels = true;
                }
                BookUpdateKind::Delta => {
                    bk.apply_delta(u.seq, &u.bids, &u.asks);
                    saw_levels = true;
                }
                // A stream-health marker carries no levels, but it DOES invalidate the book:
                // `GapStart` means "transport lost from `ts` — data until the next `Snapshot` is
                // MISSING", and `Stale` means the data stopped flowing. Folding on through either
                // one keeps applying deltas to a book that is missing whatever was lost, so the
                // emitted L1 is confidently wrong until an anchor repairs it — the same disease as
                // the cold start above, with a mid-stream trigger.
                //
                // Measured (`derived_l1_divergence_report`, token 9119933407…): `gap_start` at
                // ts=1785026504199, the first L1 disagreement 1.2s later at ts=1785026505435,
                // `live_resume` at ts=1785026505552. Un-anchoring on the gap suppresses exactly
                // that window.
                //
                // `LiveResume` deliberately does NOT re-anchor: its own doc says "the re-seed
                // `Snapshot` follows", so the stream is live again but the BOOK is not yet valid.
                // Only a `Snapshot` re-anchors.
                BookUpdateKind::GapStart | BookUpdateKind::Stale => {
                    anchored = false;
                    continue;
                }
                _ => continue,
            }
            if !anchored {
                continue; // still cold — see the anchor-gate note above
            }
            let (b, bs) = bk.best_bid().unwrap_or((0.0, 0.0)); // Level = (price, qty)
            let (a, as_) = bk.best_ask().unwrap_or((0.0, 0.0));
            // One-sided or empty books emit nothing: a `QuoteTick` with a 0.0 leg is not a quote,
            // and a strategy reading it as one would price against a phantom side.
            if b <= 0.0 || a <= 0.0 {
                continue;
            }
            let now = (b, a, bs, as_);
            if last == Some(now) {
                continue; // L1 unchanged — a deeper level moved
            }
            last = Some(now);
            out.push(QuoteTick {
                ts: u.ts,
                local_ts: u.local_ts,
                bid: b,
                ask: a,
                bid_size: bs,
                ask_size: as_,
                symbol: symbol.to_string(),
            });
        }
        // A range with real book activity that NEVER anchored yields no quotes at all — and a
        // silently-empty quote lane is the exact failure this verb was fixed for, so it must be
        // audible rather than inferred from a flat backtest. Not an error: a slice genuinely
        // containing no snapshot cannot produce trustworthy L1, and inventing some would be worse.
        // In practice this should not fire — the feed anchors roughly every 190ms.
        if saw_levels && !anchored {
            tracing::warn!(
                symbol,
                book_updates = updates.len(),
                "archive derived L1: range has book updates but NO snapshot to anchor on — \
                 emitting no quotes for it (widen the range to include an anchor)"
            );
        }
        Ok(out)
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
        self.scan_trades_impl(symbol, range)
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
        self.scan_book_updates_impl(symbol, range)
    }

    /// This archive records no instrument-properties series — always empty, so the trait's
    /// defaulted `properties_as_of` correctly yields `None`.
    fn scan_symbol_properties(
        &self,
        _venue: &str,
        _symbol: &str,
        _range: TsRange,
    ) -> Result<Vec<(i64, SymbolProperties)>, DataError> {
        Ok(Vec::new())
    }

    /// No account equity/exec-log series live in a market-data archive either.
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

    // funding / chain stay on the trait's own empty/no-op defaults (this archive holds neither).
}

#[cfg(test)]
mod tests {
    use super::*;
    use datafusion::arrow::array::ArrayRef;
    use datafusion::arrow::datatypes::{DataType, Field, Schema};
    use datafusion::parquet::arrow::ArrowWriter;
    use datafusion::parquet::file::properties::WriterProperties;
    use std::sync::Arc;

    /// `(token_id, ts, local_ts, seq, event_type, side, price, size, bids, asks, tick_size,
    /// status)` — the family layout's 12-decoded-field row shape (the other 4 real columns,
    /// `condition_id`/`is_snapshot`/`best_bid`/`best_ask`, are never decoded by this module, same
    /// as `vike_archive.rs`).
    type Row<'a> =
        (&'a str, i64, i64, u64, &'a str, &'a str, f64, f64, &'a str, &'a str, f64, &'a str);

    /// Writes `rows` as a family-layout Parquet file (`event_type`/`side` as plain `Utf8` — the
    /// real physical type this module targets, per its doc), optionally forcing a max row-group
    /// row count so a fixture can exercise pruning across many small row groups.
    fn write_family_parquet(rows: &[Row<'_>], max_rows_per_group: Option<usize>) -> Vec<u8> {
        let schema = Arc::new(Schema::new(vec![
            Field::new("token_id", DataType::Utf8, false),
            Field::new("ts", DataType::Int64, false),
            Field::new("local_ts", DataType::Int64, false),
            Field::new("seq", DataType::UInt64, false),
            Field::new("event_type", DataType::Utf8, false),
            Field::new("side", DataType::Utf8, false),
            Field::new("price", DataType::Decimal128(9, 4), false),
            Field::new("size", DataType::Decimal128(18, 6), false),
            Field::new("bids", DataType::Utf8, false),
            Field::new("asks", DataType::Utf8, false),
            Field::new("tick_size", DataType::Decimal128(9, 4), false),
            Field::new("status", DataType::Utf8, false),
        ]));
        let price = Decimal128Array::from(
            rows.iter().map(|r| (r.6 * PRICE_SCALE_DIVISOR).round() as i128).collect::<Vec<_>>(),
        )
        .with_precision_and_scale(9, 4)
        .unwrap();
        let size = Decimal128Array::from(
            rows.iter().map(|r| (r.7 * SIZE_SCALE_DIVISOR).round() as i128).collect::<Vec<_>>(),
        )
        .with_precision_and_scale(18, 6)
        .unwrap();
        let tick_size = Decimal128Array::from(
            rows.iter().map(|r| (r.10 * PRICE_SCALE_DIVISOR).round() as i128).collect::<Vec<_>>(),
        )
        .with_precision_and_scale(9, 4)
        .unwrap();
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(StringArray::from(rows.iter().map(|r| r.0).collect::<Vec<_>>()))
                    as ArrayRef,
                Arc::new(Int64Array::from(rows.iter().map(|r| r.1).collect::<Vec<_>>())),
                Arc::new(Int64Array::from(rows.iter().map(|r| r.2).collect::<Vec<_>>())),
                Arc::new(UInt64Array::from(rows.iter().map(|r| r.3).collect::<Vec<_>>())),
                Arc::new(StringArray::from(rows.iter().map(|r| r.4).collect::<Vec<_>>())),
                Arc::new(StringArray::from(rows.iter().map(|r| r.5).collect::<Vec<_>>())),
                Arc::new(price) as ArrayRef,
                Arc::new(size) as ArrayRef,
                Arc::new(StringArray::from(rows.iter().map(|r| r.8).collect::<Vec<_>>())),
                Arc::new(StringArray::from(rows.iter().map(|r| r.9).collect::<Vec<_>>())),
                Arc::new(tick_size) as ArrayRef,
                Arc::new(StringArray::from(rows.iter().map(|r| r.11).collect::<Vec<_>>())),
            ],
        )
        .unwrap();
        let props = max_rows_per_group
            .map(|n| WriterProperties::builder().set_max_row_group_row_count(Some(n)).build());
        let mut buf = Vec::new();
        {
            let mut writer = ArrowWriter::try_new(&mut buf, schema, props).unwrap();
            writer.write(&batch).unwrap();
            writer.close().unwrap();
        }
        buf
    }

    fn write_temp_parquet(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    // ---- pure decode (structural equivalence with the documented import conventions) -----------
    //
    // Same expected values as `vike_archive.rs`'s own `book_events_batch`/`trades_batch` tests
    // (0.5/100.0 bid, 0.42/7.0 buy-delta, gap_start/stale/live_resume mapping, "sell" taker ->
    // is_buyer_maker=true) — this is the "decoded values match what the ingest path produces for
    // the same input" property: both decoders apply the SAME scale divisors and the SAME
    // event_type/status/side conventions the module doc documents, so agreement here is not a
    // coincidence.

    fn one_row_batch(row: Row<'_>) -> RecordBatch {
        let path_bytes = write_family_parquet(&[row], None);
        let mut reader = ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::from(path_bytes))
            .unwrap()
            .build()
            .unwrap();
        reader.next().unwrap().unwrap()
    }

    #[test]
    fn snapshot_row_decodes_full_depth() {
        let b = one_row_batch((
            "TOKA",
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
        ));
        let out = book_updates_from_batch(&b, "TOKA", TsRange::all()).unwrap();
        assert_eq!(out.len(), 1);
        let u = &out[0];
        assert_eq!(u.kind, BookUpdateKind::Snapshot);
        assert_eq!(u.ts, 1_700_000_000_000);
        assert_eq!(u.local_ts, 1_700_000_000_003);
        assert_eq!(u.seq, 1);
        assert_eq!(u.bids, vec![(0.5, 100.0)]);
        assert_eq!(u.asks, vec![(0.51, 80.0)]);
        assert!((u.tick_size - 0.01).abs() < 1e-12);
        assert_eq!(u.symbol, "TOKA");
    }

    #[test]
    fn delta_row_decodes_the_populated_side_from_decimal_columns() {
        let rows: [Row<'_>; 2] = [
            ("TOKA", 1, 2, 5, "price_change", "buy", 0.42, 7.0, "", "", 0.01, ""),
            ("TOKA", 1, 2, 6, "price_change", "sell", 0.60, 0.0, "", "", 0.01, ""),
        ];
        let bytes = write_family_parquet(&rows, None);
        let reader = ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::from(bytes))
            .unwrap()
            .build()
            .unwrap();
        let mut out = Vec::new();
        for batch in reader {
            out.extend(book_updates_from_batch(&batch.unwrap(), "TOKA", TsRange::all()).unwrap());
        }
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].kind, BookUpdateKind::Delta);
        assert!((out[0].bids[0].0 - 0.42).abs() < 1e-9);
        assert!((out[0].bids[0].1 - 7.0).abs() < 1e-9);
        assert!(out[0].asks.is_empty());
        assert!(out[1].bids.is_empty());
        assert!((out[1].asks[0].0 - 0.60).abs() < 1e-9);
    }

    #[test]
    fn status_rows_map_to_the_right_kind_with_empty_levels() {
        let rows: [Row<'_>; 3] = [
            ("TOKA", 9, 10, 0, "status", "none", 0.0, 0.0, "", "", 0.0, "gap_start"),
            ("TOKA", 9, 11, 0, "status", "none", 0.0, 0.0, "", "", 0.0, "stale"),
            ("TOKA", 9, 12, 0, "status", "none", 0.0, 0.0, "", "", 0.0, "live_resume"),
        ];
        let bytes = write_family_parquet(&rows, None);
        let reader = ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::from(bytes))
            .unwrap()
            .build()
            .unwrap();
        let mut out = Vec::new();
        for batch in reader {
            out.extend(book_updates_from_batch(&batch.unwrap(), "TOKA", TsRange::all()).unwrap());
        }
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].kind, BookUpdateKind::GapStart);
        assert_eq!(out[1].kind, BookUpdateKind::Stale);
        assert_eq!(out[2].kind, BookUpdateKind::LiveResume);
        assert!(out.iter().all(|u| u.bids.is_empty() && u.asks.is_empty()));
    }

    #[test]
    fn trades_from_batch_inverts_the_taker_side_convention() {
        let rows: [Row<'_>; 2] = [
            (
                "TOKA",
                1_700_000_000_100,
                1_700_000_000_101,
                0,
                "trade",
                "sell",
                0.95,
                3.0,
                "",
                "",
                0.0,
                "",
            ),
            (
                "TOKA",
                1_700_000_000_200,
                1_700_000_000_201,
                0,
                "trade",
                "buy",
                0.10,
                1.0,
                "",
                "",
                0.0,
                "",
            ),
        ];
        let bytes = write_family_parquet(&rows, None);
        let reader = ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::from(bytes))
            .unwrap()
            .build()
            .unwrap();
        let mut out = Vec::new();
        for batch in reader {
            out.extend(trades_from_batch(&batch.unwrap(), "TOKA", TsRange::all()).unwrap());
        }
        assert_eq!(out.len(), 2);
        assert!(out[0].is_buyer_maker, "side=sell -> taker sold -> is_buyer_maker=true");
        assert!((out[0].price - 0.95).abs() < 1e-9);
        assert!((out[0].size - 3.0).abs() < 1e-9);
        assert!(!out[1].is_buyer_maker, "side=buy -> taker bought -> is_buyer_maker=false");
    }

    #[test]
    fn trade_rows_are_excluded_from_book_updates_and_vice_versa() {
        let rows: [Row<'_>; 2] = [
            ("TOKA", 1, 1, 0, "trade", "sell", 0.5, 1.0, "", "", 0.0, ""),
            ("TOKA", 2, 2, 1, "book", "none", 0.0, 0.0, "[]", "[]", 0.01, ""),
        ];
        let bytes = write_family_parquet(&rows, None);
        let reader = || {
            ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::from(bytes.clone()))
                .unwrap()
                .build()
                .unwrap()
        };
        let mut books = Vec::new();
        for batch in reader() {
            books.extend(book_updates_from_batch(&batch.unwrap(), "TOKA", TsRange::all()).unwrap());
        }
        assert_eq!(books.len(), 1, "only the book row");
        assert_eq!(books[0].kind, BookUpdateKind::Snapshot);

        let mut trades = Vec::new();
        for batch in reader() {
            trades.extend(trades_from_batch(&batch.unwrap(), "TOKA", TsRange::all()).unwrap());
        }
        assert_eq!(trades.len(), 1, "only the trade row");
    }

    // ---- end-to-end: ArchiveParquetHistStore over a real multi-row-group Parquet file -----------

    #[test]
    fn scan_book_updates_returns_only_the_requested_token_and_range() {
        let rows: Vec<Row<'_>> = vec![
            (
                "TOK_A",
                100,
                100,
                1,
                "book",
                "none",
                0.0,
                0.0,
                "[[0.5,10.0]]",
                "[[0.51,10.0]]",
                0.01,
                "",
            ),
            ("TOK_A", 200, 200, 2, "price_change", "buy", 0.6, 5.0, "", "", 0.01, ""),
            ("TOK_A", 999_999, 999_999, 3, "price_change", "buy", 0.7, 5.0, "", "", 0.01, ""),
            (
                "TOK_B",
                150,
                150,
                1,
                "book",
                "none",
                0.0,
                0.0,
                "[[0.4,20.0]]",
                "[[0.41,20.0]]",
                0.01,
                "",
            ),
        ];
        // One row group per row -> forces the store to prune across several groups, not just
        // decode a single one.
        let bytes = write_family_parquet(&rows, Some(1));
        let dir = tempfile::tempdir().unwrap();
        let path = write_temp_parquet(dir.path(), "day.parquet", &bytes);
        let store = ArchiveParquetHistStore::from_files([path]);

        let out = store.scan_book_updates(VENUE, "TOK_A", TsRange::of(0, 500)).unwrap();
        assert_eq!(
            out.len(),
            2,
            "TOK_A rows within [0,500] only — not TOK_B, not the ts=999999 row"
        );
        assert_eq!(out[0].ts, 100);
        assert_eq!(out[1].ts, 200);
        assert_eq!(out[1].bids, vec![(0.6, 5.0)]);
    }

    #[test]
    fn scan_book_updates_wrong_venue_is_empty_not_an_error() {
        let rows: Vec<Row<'_>> =
            vec![("TOK_A", 1, 1, 1, "book", "none", 0.0, 0.0, "[]", "[]", 0.01, "")];
        let bytes = write_family_parquet(&rows, None);
        let dir = tempfile::tempdir().unwrap();
        let path = write_temp_parquet(dir.path(), "day.parquet", &bytes);
        let store = ArchiveParquetHistStore::from_files([path]);
        assert!(store.scan_book_updates("binance", "TOK_A", TsRange::all()).unwrap().is_empty());
        assert!(store.scan_trades("binance", "TOK_A", TsRange::all()).unwrap().is_empty());
    }

    #[test]
    fn unknown_token_scans_empty_and_plans_zero_selected_row_groups() {
        let rows: Vec<Row<'_>> = vec![
            ("TOK_A", 1, 1, 1, "book", "none", 0.0, 0.0, "[[0.5,1.0]]", "[]", 0.01, ""),
            ("TOK_B", 2, 2, 1, "book", "none", 0.0, 0.0, "[[0.4,1.0]]", "[]", 0.01, ""),
        ];
        let bytes = write_family_parquet(&rows, Some(1));
        let dir = tempfile::tempdir().unwrap();
        let path = write_temp_parquet(dir.path(), "day.parquet", &bytes);
        let store = ArchiveParquetHistStore::from_files([path]);

        assert!(store.scan_book_updates(VENUE, "TOK_NOPE", TsRange::all()).unwrap().is_empty());
        assert!(store.scan_trades(VENUE, "TOK_NOPE", TsRange::all()).unwrap().is_empty());
        let plan = store.plan("TOK_NOPE", TsRange::all()).unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].selected_row_groups, 0, "an absent token selects no row groups");
        assert_eq!(plan[0].total_row_groups, 2);
    }

    #[test]
    fn plan_prunes_to_the_matching_token_row_group_only() {
        // Distinct, SORTED token ids -> one row group per id (max_row_group_size=1): each group's
        // min==max==that id, an unambiguous test of the pruning path (mirrors
        // `vike_archive::select_row_groups_prunes_to_the_matching_groups_only`, but exercised
        // through the store's own `plan`, over a REAL family-schema file).
        let rows: Vec<Row<'_>> = ["TOK_A", "TOK_B", "TOK_C", "TOK_D"]
            .iter()
            .enumerate()
            .map(|(i, tok)| {
                (*tok, i as i64, i as i64, i as u64, "book", "none", 0.0, 0.0, "[]", "[]", 0.01, "")
            })
            .collect();
        let bytes = write_family_parquet(&rows, Some(1));
        let dir = tempfile::tempdir().unwrap();
        let path = write_temp_parquet(dir.path(), "day.parquet", &bytes);
        let store = ArchiveParquetHistStore::from_files([path]);

        let plan = store.plan("TOK_B", TsRange::all()).unwrap();
        assert_eq!(plan[0].total_row_groups, 4);
        assert_eq!(plan[0].selected_row_groups, 1, "only TOK_B's own row group");
        assert!(plan[0].selected_compressed_bytes < plan[0].total_compressed_bytes);
    }

    #[test]
    fn plan_prunes_by_ts_range_too() {
        // One token, several ts values, one row group per row: a narrow ts window should select
        // only the row groups whose ts range overlaps it, independent of token pruning (every row
        // is the SAME token here, so token-axis pruning selects everything — this isolates the
        // ts-axis pruning this module adds on top of `vike_archive::select_row_groups`).
        let rows: Vec<Row<'_>> = (0..5)
            .map(|i| {
                let ts = i * 1000;
                ("TOK_A", ts, ts, i as u64, "book", "none", 0.0, 0.0, "[]", "[]", 0.01, "")
            })
            .collect();
        let bytes = write_family_parquet(&rows, Some(1));
        let dir = tempfile::tempdir().unwrap();
        let path = write_temp_parquet(dir.path(), "day.parquet", &bytes);
        let store = ArchiveParquetHistStore::from_files([path]);

        let plan = store.plan("TOK_A", TsRange::of(2000, 2000)).unwrap();
        assert_eq!(plan[0].total_row_groups, 5);
        assert_eq!(plan[0].selected_row_groups, 1, "only the ts=2000 row group overlaps");

        let out = store.scan_book_updates(VENUE, "TOK_A", TsRange::of(2000, 2000)).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].ts, 2000);
    }

    #[test]
    fn pruned_scan_matches_an_unpruned_full_scan_over_the_same_file() {
        // Equivalence check: decoding through the row-group-pruned `ArchiveParquetHistStore` path
        // must return EXACTLY the rows an unpruned, whole-file decode of the same bytes would —
        // pruning must never drop or alter a real match.
        let rows: Vec<Row<'_>> = (0..12)
            .map(|i| {
                let tok = if i % 3 == 0 {
                    "TOK_A"
                } else if i % 3 == 1 {
                    "TOK_B"
                } else {
                    "TOK_C"
                };
                let ts = i as i64 * 10;
                (
                    tok,
                    ts,
                    ts,
                    i as u64,
                    "price_change",
                    "buy",
                    0.1 * (i as f64),
                    1.0,
                    "",
                    "",
                    0.01,
                    "",
                )
            })
            .collect();
        let bytes = write_family_parquet(&rows, Some(2));
        let dir = tempfile::tempdir().unwrap();
        let path = write_temp_parquet(dir.path(), "day.parquet", &bytes);
        let store = ArchiveParquetHistStore::from_files([path]);

        // Unpruned reference: decode every row group, filter in Rust exactly like the pure decode
        // fn does, over the SAME bytes.
        let reader = ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::from(bytes))
            .unwrap()
            .build()
            .unwrap();
        let mut reference = Vec::new();
        for batch in reader {
            reference
                .extend(book_updates_from_batch(&batch.unwrap(), "TOK_B", TsRange::all()).unwrap());
        }
        reference.sort_by(|a: &BookUpdate, b: &BookUpdate| a.ts.cmp(&b.ts).then(a.seq.cmp(&b.seq)));

        let pruned = store.scan_book_updates(VENUE, "TOK_B", TsRange::all()).unwrap();
        assert_eq!(format!("{pruned:?}"), format!("{reference:?}"));
        assert_eq!(pruned.len(), 4, "12 rows / 3 tokens round-robin -> 4 TOK_B rows");
    }

    // ---- construction ------------------------------------------------------------------------

    #[test]
    fn from_dir_picks_up_every_parquet_file_sorted_and_merges_across_files() {
        let dir = tempfile::tempdir().unwrap();
        let day1: Vec<Row<'_>> =
            vec![("TOK_A", 100, 100, 1, "book", "none", 0.0, 0.0, "[[0.5,1.0]]", "[]", 0.01, "")];
        let day2: Vec<Row<'_>> =
            vec![("TOK_A", 200, 200, 1, "price_change", "buy", 0.6, 2.0, "", "", 0.01, "")];
        write_temp_parquet(
            dir.path(),
            "btc5m_2026-07-27.parquet",
            &write_family_parquet(&day1, None),
        );
        write_temp_parquet(
            dir.path(),
            "btc5m_2026-07-28.parquet",
            &write_family_parquet(&day2, None),
        );
        // A non-Parquet file in the same directory must be ignored.
        std::fs::write(dir.path().join("manifest.json"), b"{}").unwrap();

        let store = ArchiveParquetHistStore::from_dir(dir.path()).unwrap();
        assert_eq!(store.files().len(), 2, "only the two .parquet files, not manifest.json");

        let out = store.scan_book_updates(VENUE, "TOK_A", TsRange::all()).unwrap();
        assert_eq!(out.len(), 2, "rows merged across both day files");
        assert_eq!(out[0].ts, 100);
        assert_eq!(out[1].ts, 200);
    }

    #[test]
    fn writes_are_rejected() {
        let store = ArchiveParquetHistStore::from_files(Vec::new());
        assert!(store.append_bars(VENUE, "T", "1m", &[], None).is_err());
        assert!(store.append_quotes(VENUE, "T", &[], None).is_err());
        assert!(store.append_trades(VENUE, "T", &[], None).is_err());
        assert!(store.append_book_updates(VENUE, "T", &[], None).is_err());
        assert!(store.resample_quotes_to_bars(VENUE, "T", "1m", TsRange::all(), None).is_err());
        assert!(store.resample_trades_to_bars(VENUE, "T", "1m", TsRange::all(), None).is_err());
    }

    #[test]
    fn bars_and_account_series_are_always_empty_not_faked() {
        let store = ArchiveParquetHistStore::from_files(Vec::new());
        // NOTE `scan_quotes` is deliberately NOT in this list any more — it is a real derived-L1
        // verb now (see `derived_l1_*` below). It was here, and that is precisely the bug: an
        // "always empty" quote lane is indistinguishable from a quiet market to every caller.
        assert_eq!(store.load_bars(VENUE, "T", "1m", TsRange::all()).unwrap(), vec![]);
        assert_eq!(store.scan_symbol_properties(VENUE, "T", TsRange::all()).unwrap(), vec![]);
        assert_eq!(store.properties_as_of(VENUE, "T", 1_000).unwrap(), None);
        assert_eq!(store.scan_equity(VENUE, "T", TsRange::all()).unwrap(), vec![]);
        assert_eq!(store.scan_exec_fills(VENUE, "T").unwrap(), vec![]);
        assert_eq!(store.scan_exec_orders(VENUE, "T").unwrap(), vec![]);
        assert_eq!(store.scan_funding(VENUE, "T", TsRange::all()).unwrap(), vec![]);
        assert_eq!(store.scan_chain(VENUE, "T", TsRange::all()).unwrap(), vec![]);
    }

    /// The core of the derived-L1 fold: a snapshot seeds the book, deltas move it, and ONE
    /// `QuoteTick` is emitted per update whose top of book actually changed — carrying real sizes
    /// off the ladder, never a fabricated one.
    #[test]
    fn derived_l1_emits_a_quote_per_top_of_book_change_with_real_sizes() {
        let rows: Vec<Row<'_>> = vec![
            // snapshot: L1 = 0.50 x 10 / 0.51 x 12
            (
                "TOK",
                100,
                101,
                1,
                "book",
                "none",
                0.0,
                0.0,
                "[[0.5,10.0],[0.49,50.0]]",
                "[[0.51,12.0],[0.52,60.0]]",
                0.01,
                "",
            ),
            // delta on a DEEPER bid level (0.49) — L1 unchanged, must emit NOTHING
            ("TOK", 200, 201, 2, "price_change", "buy", 0.49, 99.0, "", "", 0.01, ""),
            // delta REMOVING the best ask (qty 0) — best ask steps 0.51 -> 0.52, L1 changed
            ("TOK", 300, 301, 3, "price_change", "sell", 0.51, 0.0, "", "", 0.01, ""),
        ];
        let bytes = write_family_parquet(&rows, None);
        let dir = tempfile::tempdir().unwrap();
        let path = write_temp_parquet(dir.path(), "day.parquet", &bytes);
        let store = ArchiveParquetHistStore::from_files([path]);

        let q = store.scan_quotes(VENUE, "TOK", TsRange::all()).unwrap();
        assert_eq!(
            q.len(),
            2,
            "snapshot + the L1-moving delta only, not the deep-level one: {q:?}"
        );

        assert_eq!(q[0].ts, 100);
        assert_eq!(q[0].local_ts, 101, "the update's own local_ts, not its ts");
        near(q[0].bid, 0.5, "snapshot bid");
        near(q[0].ask, 0.51, "snapshot ask");
        assert_eq!(q[0].bid_size, 10.0, "size comes off the ladder, never fabricated");
        assert_eq!(q[0].ask_size, 12.0);
        assert_eq!(q[0].symbol, "TOK");

        assert_eq!(q[1].ts, 300, "the deep-level delta at ts=200 emitted nothing");
        near(q[1].bid, 0.5, "bid untouched by an ask-side delta");
        assert_eq!(q[1].bid_size, 10.0);
        near(q[1].ask, 0.52, "best ask stepped up when 0.51 was removed");
        assert_eq!(q[1].ask_size, 60.0);
    }

    /// Price comparison for the derived-L1 fold. Prices round-trip through `L2Book`'s tick INDEX
    /// (`price_of(tick) = tick * tick_size`), so `51 * 0.01` need not be bit-identical to the
    /// literal `0.51` — an exact `assert_eq!` here would be testing f64 representation, not the
    /// fold. Half a tick is the meaningful tolerance: anything larger is a real mis-price.
    fn near(got: f64, want: f64, what: &str) {
        assert!((got - want).abs() < 0.005, "{what}: got {got}, want {want}");
    }

    /// A one-sided book emits NO quote. A `QuoteTick` with a 0.0 leg is not a quote, and a strategy
    /// that priced against it would be quoting into a phantom side.
    #[test]
    fn derived_l1_skips_one_sided_books_rather_than_emitting_a_zero_leg() {
        let rows: Vec<Row<'_>> = vec![
            // bids only — no ask at all
            ("TOK", 100, 100, 1, "book", "none", 0.0, 0.0, "[[0.5,10.0]]", "[]", 0.01, ""),
            // the ask side arrives; NOW there is a two-sided quote
            ("TOK", 200, 200, 2, "price_change", "sell", 0.55, 4.0, "", "", 0.01, ""),
        ];
        let bytes = write_family_parquet(&rows, None);
        let dir = tempfile::tempdir().unwrap();
        let path = write_temp_parquet(dir.path(), "day.parquet", &bytes);
        let store = ArchiveParquetHistStore::from_files([path]);

        let q = store.scan_quotes(VENUE, "TOK", TsRange::all()).unwrap();
        assert_eq!(q.len(), 1, "only the two-sided state quotes: {q:?}");
        assert_eq!(q[0].ts, 200);
        near(q[0].bid, 0.5, "bid");
        near(q[0].ask, 0.55, "ask");
        assert_eq!((q[0].bid_size, q[0].ask_size), (10.0, 4.0));
    }

    /// Write an `l1_quotes`-shaped Parquet file (the archive's own L1 stream schema:
    /// `token_id, condition_id, ts, local_ts, bid, ask, bid_size, ask_size`).
    fn write_l1_quotes_parquet(rows: &[(&str, i64, i64, f64, f64, f64, f64)]) -> Vec<u8> {
        use datafusion::arrow::array::{Decimal128Array, Int64Array, StringArray};
        let schema = Arc::new(Schema::new(vec![
            Field::new("token_id", DataType::Utf8, false),
            Field::new("condition_id", DataType::Utf8, false),
            Field::new("ts", DataType::Int64, false),
            Field::new("local_ts", DataType::Int64, false),
            Field::new("bid", DataType::Decimal128(9, 4), false),
            Field::new("ask", DataType::Decimal128(9, 4), false),
            Field::new("bid_size", DataType::Decimal128(18, 6), false),
            Field::new("ask_size", DataType::Decimal128(18, 6), false),
        ]));
        // Scale a column of f64s into the archive's fixed-point Decimal128 encoding.
        let dec_col = |vals: Vec<f64>, scale: f64, precision: u8, q: i8| {
            Decimal128Array::from(
                vals.iter().map(|v| (v * scale).round() as i128).collect::<Vec<i128>>(),
            )
            .with_precision_and_scale(precision, q)
            .unwrap()
        };
        let cols: Vec<ArrayRef> = vec![
            Arc::new(StringArray::from(rows.iter().map(|r| r.0).collect::<Vec<&str>>())),
            Arc::new(StringArray::from(rows.iter().map(|_| "cond").collect::<Vec<&str>>())),
            Arc::new(Int64Array::from(rows.iter().map(|r| r.1).collect::<Vec<i64>>())),
            Arc::new(Int64Array::from(rows.iter().map(|r| r.2).collect::<Vec<i64>>())),
            Arc::new(dec_col(rows.iter().map(|r| r.3).collect(), 10_000.0, 9, 4)),
            Arc::new(dec_col(rows.iter().map(|r| r.4).collect(), 10_000.0, 9, 4)),
            Arc::new(dec_col(rows.iter().map(|r| r.5).collect(), 1_000_000.0, 18, 6)),
            Arc::new(dec_col(rows.iter().map(|r| r.6).collect(), 1_000_000.0, 18, 6)),
        ];
        let batch = RecordBatch::try_new(schema.clone(), cols).unwrap();
        let mut buf = Vec::new();
        let mut w =
            ArrowWriter::try_new(&mut buf, schema, Some(WriterProperties::builder().build()))
                .unwrap();
        w.write(&batch).unwrap();
        w.close().unwrap();
        buf
    }

    /// **The archive's own `l1_quotes` stream WINS over deriving from book depth.**
    ///
    /// Every date partition — flat and family alike — ships `l1_quotes.parquet` beside
    /// `book_events.parquet`, because the recorder writes L1 and L2 as separate streams. When both
    /// are present, the recorded stream is what the venue actually published; derived L1 is a
    /// reconstruction and is the fallback for holding only the book file.
    ///
    /// The values here are deliberately IMPOSSIBLE to derive from the book (bid 0.11/ask 0.12 with
    /// sizes 1/2, against a book of 0.5/0.51 x10/x12), so the assertion can only pass if the
    /// recorded file was actually used.
    #[test]
    fn recorded_l1_quotes_win_over_derived_when_both_files_are_present() {
        let book: Vec<Row<'_>> = vec![(
            "TOK",
            100,
            100,
            1,
            "book",
            "none",
            0.0,
            0.0,
            "[[0.5,10.0]]",
            "[[0.51,12.0]]",
            0.01,
            "",
        )];
        let dir = tempfile::tempdir().unwrap();
        let book_path = write_temp_parquet(
            dir.path(),
            "book_events.parquet",
            &write_family_parquet(&book, None),
        );
        let q_path = write_temp_parquet(
            dir.path(),
            "l1_quotes.parquet",
            &write_l1_quotes_parquet(&[("TOK", 100, 101, 0.11, 0.12, 1.0, 2.0)]),
        );

        // Book only -> derived L1 (the fallback).
        let derived = ArchiveParquetHistStore::from_files([book_path.clone()]);
        let d = derived.scan_quotes(VENUE, "TOK", TsRange::all()).unwrap();
        assert_eq!(d.len(), 1);
        near(d[0].bid, 0.5, "derived bid");

        // Both files -> the RECORDED stream, not the book-derived one.
        let both = ArchiveParquetHistStore::from_files([book_path, q_path]);
        let r = both.scan_quotes(VENUE, "TOK", TsRange::all()).unwrap();
        assert_eq!(r.len(), 1, "{r:?}");
        near(r[0].bid, 0.11, "recorded bid — not the book's 0.5");
        near(r[0].ask, 0.12, "recorded ask — not the book's 0.51");
        assert_eq!(r[0].bid_size, 1.0);
        assert_eq!(r[0].ask_size, 2.0);
        assert_eq!(r[0].local_ts, 101, "the recorded stream's own local_ts");

        // And the book lane still works with a quote file alongside — the l1_quotes file must not
        // be fed to the book decoder, which would be a hard schema error, not an empty result.
        assert_eq!(both.scan_book_updates(VENUE, "TOK", TsRange::all()).unwrap().len(), 1);
        assert!(both.scan_trades(VENUE, "TOK", TsRange::all()).unwrap().is_empty());
    }

    /// **The anchor gate.** Deltas alone never produce a quote: a range scan of a delta stream
    /// starts with an EMPTY book, so its "top of book" is the best of a partial ladder, not the
    /// real one.
    ///
    /// This is the fix for a measured, real divergence (`derived_l1_divergence_report`). Scanning a
    /// live market's opening 300s window, the fold reported 0.42/0.65, 0.42/0.56, 0.49/0.56,
    /// 0.49/0.55 while the recorder's own L1 said 0.50/0.51 — and became exact at the very
    /// millisecond the first snapshot landed (ts=…000179), agreeing event-for-event on all 12,687
    /// quotes after it. Four confident, wrong quotes at the open of every market.
    #[test]
    fn derived_l1_emits_nothing_until_a_snapshot_anchors_the_book() {
        let rows: Vec<Row<'_>> = vec![
            // deltas only — a two-sided book forms, but it was never anchored
            ("TOK", 100, 100, 1, "price_change", "buy", 0.42, 30.0, "", "", 0.01, ""),
            ("TOK", 110, 110, 2, "price_change", "sell", 0.65, 30.0, "", "", 0.01, ""),
            ("TOK", 120, 120, 3, "price_change", "sell", 0.55, 28.0, "", "", 0.01, ""),
        ];
        let bytes = write_family_parquet(&rows, None);
        let dir = tempfile::tempdir().unwrap();
        let path = write_temp_parquet(dir.path(), "day.parquet", &bytes);
        let store = ArchiveParquetHistStore::from_files([path]);
        let q = store.scan_quotes(VENUE, "TOK", TsRange::all()).unwrap();
        assert!(
            q.is_empty(),
            "an un-anchored book must emit nothing — a partial ladder's best is not the L1: {q:?}"
        );

        // Same stream, with a snapshot in front: now it anchors and emits from there.
        let rows: Vec<Row<'_>> = vec![
            ("TOK", 90, 90, 1, "book", "none", 0.0, 0.0, "[[0.5,10.0]]", "[[0.51,12.0]]", 0.01, ""),
            ("TOK", 100, 100, 2, "price_change", "buy", 0.42, 30.0, "", "", 0.01, ""),
        ];
        let bytes = write_family_parquet(&rows, None);
        let dir2 = tempfile::tempdir().unwrap();
        let path2 = write_temp_parquet(dir2.path(), "day.parquet", &bytes);
        let store2 = ArchiveParquetHistStore::from_files([path2]);
        let q2 = store2.scan_quotes(VENUE, "TOK", TsRange::all()).unwrap();
        assert_eq!(
            q2.len(),
            1,
            "the snapshot anchors; the deeper 0.42 bid does not move L1: {q2:?}"
        );
        near(q2[0].bid, 0.5, "anchored bid");
        near(q2[0].ask, 0.51, "anchored ask");
    }

    /// A mid-stream `GapStart` UN-anchors the book: no quotes until the next `Snapshot` re-seeds
    /// it, because data between the two is by definition missing.
    ///
    /// Found the same way as the cold-start gate — by drilling one real market. Token 9119933407…
    /// carries `gap_start` at ts=1785026504199, and the derived stream's first disagreement with
    /// the recorder's L1 lands 1.2s later at ts=1785026505435, with `live_resume` at
    /// ts=1785026505552. Folding deltas across a gap keeps applying them to a book missing whatever
    /// was lost, so the L1 is confidently wrong until an anchor repairs it.
    ///
    /// `LiveResume` must NOT re-anchor on its own: its doc says "the re-seed `Snapshot` follows",
    /// so the transport is healthy again while the book still is not.
    #[test]
    fn derived_l1_gap_unanchors_until_the_next_snapshot() {
        let rows: Vec<Row<'_>> = vec![
            (
                "TOK",
                100,
                100,
                1,
                "book",
                "none",
                0.0,
                0.0,
                "[[0.5,10.0]]",
                "[[0.51,12.0],[0.52,20.0]]",
                0.01,
                "",
            ),
            // anchored: removing the best ask steps L1 to 0.52 — still two-sided, so it emits
            ("TOK", 200, 200, 2, "price_change", "sell", 0.51, 0.0, "", "", 0.01, ""),
            // transport lost — everything from here is untrustworthy
            ("TOK", 300, 300, 0, "status", "none", 0.0, 0.0, "", "", 0.01, "gap_start"),
            // a delta across the gap must NOT emit (the book is missing whatever was lost)
            ("TOK", 400, 400, 3, "price_change", "buy", 0.53, 5.0, "", "", 0.01, ""),
            // live again — but the book is still not valid, so still nothing
            ("TOK", 500, 500, 0, "status", "none", 0.0, 0.0, "", "", 0.01, "live_resume"),
            ("TOK", 600, 600, 4, "price_change", "buy", 0.54, 6.0, "", "", 0.01, ""),
            // the re-seed snapshot: anchored again, emits from here
            (
                "TOK",
                700,
                700,
                5,
                "book",
                "none",
                0.0,
                0.0,
                "[[0.6,20.0]]",
                "[[0.61,22.0]]",
                0.01,
                "",
            ),
        ];
        let bytes = write_family_parquet(&rows, None);
        let dir = tempfile::tempdir().unwrap();
        let path = write_temp_parquet(dir.path(), "day.parquet", &bytes);
        let store = ArchiveParquetHistStore::from_files([path]);

        let q = store.scan_quotes(VENUE, "TOK", TsRange::all()).unwrap();
        let ts: Vec<i64> = q.iter().map(|x| x.ts).collect();
        assert_eq!(
            ts,
            vec![100, 200, 700],
            "emit while anchored (100, 200), nothing across the gap (400, 600 — even after \
             live_resume at 500), then again from the re-seed snapshot (700): {q:?}"
        );
        near(q[2].bid, 0.6, "re-anchored bid");
        near(q[2].ask, 0.61, "re-anchored ask");
    }

    /// The quote lane honors venue and range exactly like the book lane it folds.
    #[test]
    fn derived_l1_respects_venue_and_range() {
        let rows: Vec<Row<'_>> = vec![
            (
                "TOK",
                100,
                100,
                1,
                "book",
                "none",
                0.0,
                0.0,
                "[[0.5,10.0]]",
                "[[0.51,10.0]]",
                0.01,
                "",
            ),
            ("TOK", 9_000, 9_000, 2, "price_change", "buy", 0.52, 3.0, "", "", 0.01, ""),
        ];
        let bytes = write_family_parquet(&rows, None);
        let dir = tempfile::tempdir().unwrap();
        let path = write_temp_parquet(dir.path(), "day.parquet", &bytes);
        let store = ArchiveParquetHistStore::from_files([path]);

        assert!(
            store.scan_quotes("binance", "TOK", TsRange::all()).unwrap().is_empty(),
            "a non-polymarket venue has nothing here"
        );
        let q = store.scan_quotes(VENUE, "TOK", TsRange::of(0, 1_000)).unwrap();
        assert_eq!(q.len(), 1, "the ts=9000 update is out of range: {q:?}");
        assert_eq!(q[0].ts, 100);
    }

    /// `Arc<dyn HistStore + Send + Sync>` must accept this store — the exact shape
    /// `harness::run_backtest`/`hist_replay::replay_ticks` take.
    #[test]
    fn is_usable_as_a_dyn_hist_store_trait_object() {
        let store: std::sync::Arc<dyn HistStore + Send + Sync> =
            std::sync::Arc::new(ArchiveParquetHistStore::from_files(Vec::new()));
        assert!(store.scan_book_updates(VENUE, "T", TsRange::all()).unwrap().is_empty());
    }

    // ---- derived-L1 vs recorded-l1_quotes divergence report (never run in CI) --------------------
    //
    // The backtest over 580 BTC-5m markets agreed to within ~0.26pp between this store's DERIVED L1
    // and a `DataFusionHist` whose quote lane came from the recorder's own `l1_quotes` (via
    // ClickHouse). Close — but not zero, and "close" is not an explanation. This walks the SAME
    // universe market by market and localises the residual: which markets diverge, by how much, and
    // then event by event inside the worst one.
    //
    // The two sources are deliberately different, so SOME divergence is expected; the question is
    // only whether it is the expected kind:
    //   - derived L1 = fold this file's own book depth, emit on top-of-book change
    //   - recorded l1_quotes = the live recorder's own L1 capture, its own timestamps, its own dedup
    //
    // Self-skips when any input is absent — same idiom as the live measure below. Run manually:
    // `cargo test -p vike-backfill --features vike-archive --lib archive_store::tests::derived_l1_divergence_report -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn derived_l1_divergence_report() {
        const ARCHIVE: &str = "/var/lib/vike/dl/btc5m_2026-07-26.parquet";
        const STORE: &str = "/var/lib/vike/btc_store";
        const UNIVERSE: &str = "/tmp/uni_final.tsv";
        const TENOR_MS: i64 = 300_000;
        // How many markets to walk. ONE by default: the point is to LOCALISE a divergence, and one
        // market answers "is there one, and what shape is it" in ~14s where the full 580 costs ~20
        // minutes. Widen by writing a count into `<UNIVERSE>.markets` — libtest REJECTS unknown CLI
        // flags ("Unrecognized option"), and an env var would need a settings-registry row for what
        // is a test-only knob.
        let max_markets: usize = std::fs::read_to_string(format!("{UNIVERSE}.markets"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(1);
        for p in [ARCHIVE, STORE, UNIVERSE] {
            if !Path::new(p).exists() {
                eprintln!("{p} not present — skipping derived-L1 divergence report");
                return;
            }
        }
        let universe = std::fs::read_to_string(UNIVERSE).unwrap();
        let markets: Vec<(String, i64)> = universe
            .lines()
            .filter_map(|l| {
                let mut f = l.split('\t');
                let _family = f.next()?;
                let token = f.next()?.to_string();
                let end: i64 = f.next()?.trim().parse().ok()?;
                Some((token, end))
            })
            .collect();
        assert!(!markets.is_empty(), "universe parsed empty");

        let archive = ArchiveParquetHistStore::from_files([PathBuf::from(ARCHIVE)]);
        let store = vike_data::DataFusionHist::open(STORE).expect("open reference store");

        // (token, derived_count, recorded_count, market_end, first disagreeing ts)
        let mut rows: Vec<(String, usize, usize, i64, Option<i64>)> = Vec::new();
        for (token, end) in markets.iter() {
            if rows.len() >= max_markets {
                break;
            }
            let range = TsRange::of(end - TENOR_MS, *end);
            let d = archive.scan_quotes(VENUE, token, range).unwrap();
            let r = store.scan_quotes(VENUE, token, range).unwrap();
            if d.is_empty() && r.is_empty() {
                continue; // market outside this file's day — not a divergence
            }
            let first_bad = first_l1_disagreement(&d, &r);
            rows.push((token.clone(), d.len(), r.len(), *end, first_bad));
        }
        assert!(!rows.is_empty(), "no market had quotes on either side");

        let with_data = rows.len();
        let agreeing = rows.iter().filter(|r| r.4.is_none()).count();
        let total_d: usize = rows.iter().map(|r| r.1).sum();
        let total_r: usize = rows.iter().map(|r| r.2).sum();
        let mut by_gap: Vec<&(String, usize, usize, i64, Option<i64>)> = rows.iter().collect();
        by_gap.sort_by_key(|r| -((r.1 as i64 - r.2 as i64).abs()));

        eprintln!("\n=== derived-L1 vs recorded-l1_quotes, {with_data} markets with data ===");
        eprintln!("markets whose L1 step function NEVER disagrees: {agreeing}/{with_data}");
        eprintln!("total quotes: derived={total_d} recorded={total_r}");
        eprintln!("\nworst 10 by |count gap|:");
        eprintln!(
            "{:<22} {:>9} {:>9} {:>8}  first-disagreement-ts",
            "token", "derived", "recorded", "gap"
        );
        for r in by_gap.iter().take(10) {
            eprintln!(
                "{:<22} {:>9} {:>9} {:>8}  {}",
                &r.0[..22.min(r.0.len())],
                r.1,
                r.2,
                r.1 as i64 - r.2 as i64,
                r.4.map(|t| t.to_string()).unwrap_or_else(|| "-".into()),
            );
        }

        // Event-by-event inside the worst market, so the SHAPE of the divergence is visible rather
        // than inferred from counts.
        // Dump the first market that actually DISAGREES, falling back to the worst count gap when
        // none does. A count gap alone is usually just the anchor gate suppressing the pre-anchor
        // head start — benign and already understood — whereas a step-function disagreement is the
        // thing still unexplained, so that is what deserves the event-by-event look.
        if let Some(worst) = by_gap.iter().find(|r| r.4.is_some()).or_else(|| by_gap.first()) {
            let range = TsRange::of(worst.3 - TENOR_MS, worst.3);
            let d = archive.scan_quotes(VENUE, &worst.0, range).unwrap();
            let r = store.scan_quotes(VENUE, &worst.0, range).unwrap();
            if let Some(bad) = worst.4 {
                eprintln!(
                    "\nfirst disagreement at ts={bad}; window opens at {}",
                    worst.3 - TENOR_MS
                );
                // STRADDLE the disagreement: the last 3 events at or before it and the first 5 at
                // or after, per side. "First N within a window" truncates before the interesting
                // moment whenever the stream is dense — which is exactly when it matters.
                let straddle = |v: &[QuoteTick], label: &str| {
                    let split = v.partition_point(|q| q.ts < bad);
                    eprintln!("--- {label}, straddling the disagreement ---");
                    for q in v[split.saturating_sub(3)..(split + 5).min(v.len())].iter() {
                        eprintln!(
                            "  {}ts={} bid={} x{} ask={} x{}",
                            if q.ts >= bad { ">" } else { " " },
                            q.ts,
                            q.bid,
                            q.bid_size,
                            q.ask,
                            q.ask_size
                        );
                    }
                };
                straddle(&d, "derived");
                straddle(&r, "recorded");
            }
            eprintln!(
                "\n=== event-by-event, worst market {} ===",
                &worst.0[..22.min(worst.0.len())]
            );
            eprintln!("--- derived (first 20) ---");
            for q in d.iter().take(20) {
                eprintln!(
                    "  ts={} bid={} x{} ask={} x{}",
                    q.ts, q.bid, q.bid_size, q.ask, q.ask_size
                );
            }
            eprintln!("--- recorded (first 20) ---");
            for q in r.iter().take(20) {
                eprintln!(
                    "  ts={} bid={} x{} ask={} x{}",
                    q.ts, q.bid, q.bid_size, q.ask, q.ask_size
                );
            }
        }
    }

    /// First ts at which the two quote streams disagree about the L1 **in force at that instant**.
    ///
    /// Compared as STEP FUNCTIONS, not element-wise: the two sources are allowed to emit at
    /// different moments (different dedup, different capture), so zipping them by index would
    /// report every stream as totally divergent and explain nothing. Instead, at each event ts in
    /// either stream, ask both "what is your latest quote at or before this ts?" and compare those.
    /// Prices compare within half a tick, for the same reason [`near`] does. `None` = the two never
    /// disagree anywhere in the window.
    fn first_l1_disagreement(a: &[QuoteTick], b: &[QuoteTick]) -> Option<i64> {
        // TIE-TOLERANT: when several events share a timestamp the feed defines NO order among
        // them, and the two pipelines sort independently, so "the state after ts" can legitimately
        // differ by which same-ts event happens to be last. Measured (token 1151341668…, ts=364):
        // both streams carried the same two events, `bid=0.41 x13.3` and `bid=0.4 x208.58`, in
        // opposite order — identical before, identical after. Comparing the state-after-ts alone
        // reports that as a divergence, which is an artifact of the comparator, not of the data.
        //
        // So a ts agrees if the two sides carry the same SET of L1 states at it, or if the state
        // after it matches. Only a genuine difference in what was seen survives both.
        let states_at = |v: &[QuoteTick], ts: i64| -> Vec<(u64, u64)> {
            let mut s: Vec<(u64, u64)> = v
                .iter()
                .filter(|q| q.ts == ts)
                .map(|q| ((q.bid * 1000.0).round() as u64, (q.ask * 1000.0).round() as u64))
                .collect();
            s.sort_unstable();
            s
        };
        let latest_at = |v: &[QuoteTick], ts: i64| -> Option<(f64, f64)> {
            v.iter().rev().find(|q| q.ts <= ts).map(|q| (q.bid, q.ask))
        };
        let mut times: Vec<i64> = a.iter().map(|q| q.ts).chain(b.iter().map(|q| q.ts)).collect();
        times.sort_unstable();
        times.dedup();
        // Only compare once BOTH streams have started; a pure head-start is a count difference,
        // reported by the count columns, not a mid-stream divergence.
        let both_live = match (a.first(), b.first()) {
            (Some(x), Some(y)) => x.ts.max(y.ts),
            _ => return None,
        };
        for ts in times {
            if ts < both_live {
                continue;
            }
            if let (Some((ab, aa)), Some((bb, ba))) = (latest_at(a, ts), latest_at(b, ts)) {
                let after_matches = (ab - bb).abs() < 0.005 && (aa - ba).abs() < 0.005;
                if !after_matches && states_at(a, ts) != states_at(b, ts) {
                    return Some(ts);
                }
            }
        }
        None
    }

    // ---- live measurement smoke (never run in CI; needs the real the CI box archive file) ------------
    //
    // Reports the MEASURE deliverable numbers this module's report is built on: wall clock, bytes
    // read (via `plan`'s row-group byte accounting), row groups touched vs total, rows returned, and
    // peak RSS (`/proc/self/status` `VmHWM`, Linux-only, best-effort) for ONE token's own market
    // window vs the same query repeated over ~20 tokens. Self-skips when the file is absent — same
    // idiom as `vike_archive.rs`'s `#[ignore]`d live smokes. Run manually:
    // `cargo test -p vike-backfill --features vike-archive --lib archive_store::tests::live_measure_against_real_archive_file -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_measure_against_real_archive_file() {
        const FILE: &str = "/var/lib/vike/dl/btc5m_2026-07-28.parquet";
        if !Path::new(FILE).exists() {
            eprintln!("{FILE} not present — skipping live archive-store measurement");
            return;
        }

        fn vm_hwm_kb() -> Option<u64> {
            let status = std::fs::read_to_string("/proc/self/status").ok()?;
            status.lines().find_map(|l| {
                l.strip_prefix("VmHWM:")
                    .and_then(|rest| rest.split_whitespace().next())
                    .and_then(|kb| kb.parse().ok())
            })
        }

        // 25 real token ids from this file's own day, oldest-window-first (harvested via
        // `clickhouse-local`'s standalone `GROUP BY token_id` against the same local file — no
        // server, no network). The first is used for the single-token case; all 25 for the
        // "~20 tokens" case (a realistic per-series backtest replay loop: one `scan_book_updates`
        // call per token, exactly like `hist_replay::replay_ticks` issues per series).
        const TOKENS: &[&str] = &[
            "23152407599655538885623805493900043467958601874029530629927915276594719410439",
            "104571443997234897304141858465981034372030063880357705082249379826847038542378",
            "14301790185379892771571254247303545757717650772133892621663602964384542523992",
            "78321747154810254051861975218789122051845823043795362938649577164074888086089",
            "97671438448529185350787963197240804605805884897399485407503693494408417168470",
            "26256870810957662813774985645741265151480872553185993802730211703418628116159",
            "53027238618278186187577262403781830403994334114229466701325039465002184402157",
            "96667851774266765992556672614454228715564184201094718939745824592319584816112",
            "104086802706154418012875514448736113322113701846888813699955218638729179713845",
            "69050896154587792492371870680355670647404731895517712766508577896915780266670",
            "21413240782782670867191403316273233750669966867951507759590252952907554173144",
            "64363682592539029771920593705361530319135028911791177669001455328725856174386",
            "57997709997188651018796419454040666912401809435049910488400909521576768666389",
            "36325126872656430136186868653334900065037869194804379074055328330745125586278",
            "79393919153640374967889303608748517110647328768868919134062021800595655663540",
            "67708259947866665757301650057559387706903035949289005175172523672038772089859",
            "9783993234210266699347245365566496415323137082179260422643701204848379402442",
            "80887692812220726900805645119609289512621731695120160205175177755807711515699",
            "47151877687402178187507313687642483587217295197325736942711148947667700139749",
            "98251074153187170609910333754018623687412278037670001730907786212785897353274",
        ];

        let store = ArchiveParquetHistStore::from_files([PathBuf::from(FILE)]);

        // ---- single token, whole file's ts range (its own window is well inside it) -----------
        let one = TOKENS[0];
        let t0 = std::time::Instant::now();
        let plan = store.plan(one, TsRange::all()).unwrap();
        let plan_ms = t0.elapsed().as_millis();
        let t1 = std::time::Instant::now();
        let rows = store.scan_book_updates(VENUE, one, TsRange::all()).unwrap();
        let scan_ms = t1.elapsed().as_millis();
        let rss = vm_hwm_kb();
        println!(
            "SINGLE token={one} plan_ms={plan_ms} scan_ms={scan_ms} rows={} \
             row_groups={}/{} bytes={}/{} vm_hwm_kb={rss:?}",
            rows.len(),
            plan[0].selected_row_groups,
            plan[0].total_row_groups,
            plan[0].selected_compressed_bytes,
            plan[0].total_compressed_bytes,
        );

        // ---- ~20 tokens, one scan_book_updates call each (a realistic replay loop) -------------
        let t2 = std::time::Instant::now();
        let mut total_rows = 0usize;
        let mut total_selected_rg = 0usize;
        let mut total_rg = 0usize;
        let mut total_selected_bytes: i64 = 0;
        let mut total_bytes: i64 = 0;
        for &tok in TOKENS {
            let p = store.plan(tok, TsRange::all()).unwrap();
            total_selected_rg += p[0].selected_row_groups;
            total_rg += p[0].total_row_groups;
            total_selected_bytes += p[0].selected_compressed_bytes;
            total_bytes += p[0].total_compressed_bytes;
            total_rows += store.scan_book_updates(VENUE, tok, TsRange::all()).unwrap().len();
        }
        let many_ms = t2.elapsed().as_millis();
        let rss_many = vm_hwm_kb();
        println!(
            "MANY tokens={} total_ms={many_ms} avg_ms={:.1} total_rows={total_rows} \
             row_groups_selected_sum={total_selected_rg} row_groups_total_sum={total_rg} \
             bytes_selected_sum={total_selected_bytes} bytes_total_sum={total_bytes} \
             vm_hwm_kb={rss_many:?}",
            TOKENS.len(),
            many_ms as f64 / TOKENS.len() as f64,
        );
    }
}
