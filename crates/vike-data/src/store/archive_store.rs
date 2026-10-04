//! `ArchiveParquetHistStore` — a `crate::HistStore` that reads `data.vike.io` Polymarket
//! archive Parquet files **IN PLACE**, so a tick-replay backtest needs no import step at all.
//!
//! Why this exists: getting one day of BTC-5m archive data into the DataFusion hist store today
//! costs a download PLUS a full decode→re-encode "import" (~1m45s/day single-core, measured —
//! mostly Parquet-encode + zstd + per-series commits, the rest mapping/decode). The data is
//! *already* Parquet and the store is *already* Parquet — the import is a format round-trip that
//! mostly re-encodes what it just decoded. This store skips that round-trip: it opens the archive
//! file(s) directly and answers `HistStore` reads straight off them, at query time. The precedent
//! was `backtest_bridge`'s `ClickHousePolyHistStore`, which did the same thing over ClickHouse (no
//! local copy, no ingest); this was the local-Parquet-file analogue of it. ⚠ That store is DELETED
//! (2026-09-20) — see `crates/vike-data/src/store/backtest_store.rs`'s module doc — so this is
//! now the read-in-place store rather than one of two, and a Polymarket backtest has exactly two
//! backends: this one and `crate::DataFusionHist`.
//!
//! **Schema note — this is the "family" day-file layout, not the flat `venue=.../date=...`
//! archive layout `vike_backfill::vike_archive` documents.** The real files this was built and measured
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
//! `vike_archive::book_updates_from_batch` (same scale divisors, same
//! event_type/status/side conventions) rather than a shared extraction: `vike_archive.rs` is under
//! concurrent edit by two other in-flight branches (`ingest-parallel`/`local-file`) at the time
//! this was written, and this module only needs read-only access to a handful of its PUBLIC items
//! (`VENUE`, `select_row_groups`) — reused directly below, not copied. The duplication is
//! unit-tested to match the documented conventions row-for-row (see `tests` below); pushing this
//! back into one shared helper is a natural follow-up once the concurrent edits land. The two
//! LEAF decoders that were byte-identical on both sides — [`parse_levels_json`] and
//! [`StrOrBinCol`] — have been pulled together already: they are `pub` here, and
//! `vike_backfill`'s `vike_archive` and `events_api` call these rather than carrying a copy.
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
//! This used to be the one place this store's convention differed, deliberately, from
//! `ClickHousePolyHistStore` (which scoped its ClickHouse `BETWEEN` on `local_ts`). ⚠ That store is
//! DELETED (2026-09-20), so both surviving backends scope on `ts` and the divergence is now
//! between this store and the recorder's OTHER stamp rather than between two stores.
//!
//! ⚠ **The difference was recorded here as "an observation for whoever eventually reconciles the
//! two". It has since been MEASURED, and it is not a reconciliation job — it is a permanent,
//! named difference, and the direction is not the one the observation implied.**
//! [`crate::window_clock::WindowClock`] is the single authority for both the numbers and the
//! argument; the two facts worth carrying before you get there are that the gap is NOT feed latency
//! (98.5% of the tape is `price_change`, tens of milliseconds) but the full-ladder `book` snapshot
//! rows, whose venue stamp can be HOURS older than their arrival; and that this store — the `ts`
//! side — is therefore the one that can miss a window's only snapshot ANCHOR, which is exactly what
//! [`ArchiveParquetHistStore::scan_quotes`]' anchor-gated fold refuses to emit quotes without.
//! Every `scan_book_updates` call reports how many of the rows it DID return a `local_ts`-scoped
//! window would have excluded, so a window the two clocks disagree about is audible from inside one
//! run; it cannot report the opposite direction, because those rows are never fetched here.
//!
//! **Verbs answered vs refused** (the "do not fake data" posture the deleted ClickHouse store
//! shared):
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
//! `load_bars` and `scan_symbol_properties` are always empty (this archive genuinely holds neither
//! series, and [`ArchiveParquetHistStore::scan_equity`]'s doc argues why those two keep the empty
//! while the account lanes stopped). Everything else this store cannot hold REFUSES — ⚠ **in two
//! different `DataError` variants, and this is the module where that split is load-bearing:**
//!
//! * `DataError::Unsupported`, the CAPABILITY answer — the ACCOUNT plane (`scan_equity`,
//!   `scan_exec_fills`, `scan_exec_orders`, this store's own three arms, which read `Ok(vec![])`
//!   until the sweep that carried
//!   `docs/decisions/0080-the-account-funding-kind-takes-the-qualified-name.md` verdict 6 onto
//!   them), plus the two kinds whose refusing TRAIT DEFAULTS this store inherits whole, read half
//!   and write half alike: `exec_funding` and `chain`.
//! * `DataError::Query` — `depth` and the catalog verbs (`list_series`, `inventory`,
//!   `series_commits`). They refuse for the same reason and say it in an older variant: those
//!   defaults landed in #1424 and #1433, before `Unsupported` existed at all (#2072), and nothing
//!   swept them.
//!
//! ⚠ Do not read either half off the other. `every_kind_this_store_cannot_hold_refuses_and_names_itself`
//! matches `Err(DataError::Unsupported(_))` and PANICS on anything else, so a `depth` or catalog
//! case added to it from a paragraph claiming they all refuse with `Unsupported` fails on a
//! perfectly correct refusal — which is what this paragraph claimed for one review round.
//! Every `append_*`/`resample_*_to_bars` this store IMPLEMENTS is rejected in a third variant
//! again, `DataError::Io` through this module's own `read_only` helper — this store is READ-ONLY
//! over files an operator downloaded once, never a second writer of them.

use std::collections::HashSet;
use std::fs::File;
use std::path::{Path, PathBuf};
use vike_model::BookLevel;

use datafusion::arrow::array::{
    BinaryArray, Decimal128Array, Float64Array, Int64Array, StringArray, UInt64Array,
};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use datafusion::parquet::file::metadata::ParquetMetaData;
use datafusion::parquet::file::statistics::Statistics;

use crate::{DataError, ExecFillRow, ExecOrderRow, HistStore, TsRange};
use vike_model::{
    Bar, BookUpdate, BookUpdateKind, EquitySample, L2Book, QuoteTick, SymbolProperties, TradeTick,
};

use crate::window_clock::{WindowClock, other_clock_excludes};

/// Vendor prefix for error messages (mirrors `vike_backfill::vike_archive`/`vike_backfill::arrowutil`'s `CTX`
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
/// but as raw `Binary` on the flat `data.vike.io/archive` layout `vike_backfill`'s `vike_archive`
/// documents — this enum is the ONE decode of either physical shape (that crate's own column
/// constructor, `str_or_bin_col`, builds these same variants under its own error type), so a file
/// of either shape decodes without error.
pub enum StrOrBinCol<'a> {
    Utf8(&'a StringArray),
    Bin(&'a BinaryArray),
}

impl StrOrBinCol<'_> {
    /// UTF-8(-lossy for the `Binary` case) decode of one row (malformed bytes degrade to `""`
    /// rather than panicking — one bad row must not fail a whole scan).
    pub fn value(&self, i: usize) -> &str {
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

/// Decode a `[[price,size],...]` JSON depth column (plain numbers — NOT pmxt's stringified-number
/// variant `vike_backfill`'s `pmxt::map::parse_levels` handles). Empty string (delta/status/
/// trade rows) and malformed JSON both degrade to an empty depth rather than a decode error — one
/// bad row must not fail a whole scan. The ONE copy: `vike_backfill`'s `vike_archive` and
/// `events_api` decode their ladders through it too.
pub fn parse_levels_json(json: &str) -> Vec<BookLevel> {
    if json.is_empty() {
        return Vec::new();
    }
    serde_json::from_str::<Vec<BookLevel>>(json).unwrap_or_default()
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
                let level = BookLevel {
                    price: dec(price, i, PRICE_SCALE_DIVISOR),
                    qty: dec(size, i, SIZE_SCALE_DIVISOR),
                };
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
/// `is_buyer_maker = true` (the same convention `vike_backfill::vike_archive::trades_from_batch` uses for
/// the flat layout — and the deleted `backtest_bridge`'s own `trades_from_batch` used for the
/// ClickHouse one, which is where the convention was first written down).
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

    /// The ONE spelling of the account-plane refusal the three `scan_*` arms below return, so the
    /// three cannot drift apart in wording and an operator greps one sentence rather than three.
    ///
    /// It names the KIND, the VERB and the `(venue, symbol)` that was asked for, matching
    /// `crate::HistStore::scan_funding`'s defaulted refusal: the reader has to learn WHICH question
    /// was refused, because the cure is to point the caller at a store that holds the account plane
    /// (`crate::DataFusionHist`) rather than to fix anything here. `DataError::Unsupported`, not
    /// `Io`/`Query` — the call was well-formed and nothing failed; this backend has no such lane.
    ///
    /// ⚠ `kind` is the BARE kind and the `kind=` prefix is composed here, matching
    /// `crate::HistStore::scan_funding`'s refusal — deliberately, not stylistically.
    /// `crates/vike-ops/tests/store_kind_gate.rs`'s
    /// `no_kind_literal_escapes_a_declared_site_and_the_path_helpers_stay_private` counts every
    /// hand-built `"kind=` LITERAL in this crate as a series path built outside `series_dir`, and a
    /// refusal MESSAGE is not a path. Spelling it as an argument would have to be declared in that
    /// gate's roster, which would dilute the one thing the roster is for.
    ///
    /// ⚠ `kind` is a free `&str` at the three call sites, and what stops it drifting is a
    /// COMPLETENESS test rather than a type — the per-roster playbook's shape, the same one every
    /// per-venue table uses. `every_kind_this_store_cannot_hold_refuses_and_names_itself` drives one
    /// case per `vike_model::ACCOUNT_KINDS` entry and asserts the message THIS helper built names
    /// it, so a fifth account kind landing on an empty trait default, or a rename of one — 0079 did
    /// `funding` → `exec_funding` already — reddens instead of leaving a refusal that names a kind
    /// which no longer exists.
    fn no_account_plane(kind: &str, verb: &str, venue: &str, symbol: &str) -> DataError {
        DataError::Unsupported(format!(
            "{CTX}: this store is published MARKET Parquet and holds no account plane, so it \
             cannot answer kind={kind} ({verb} asked for {venue}:{symbol}) — an empty result \
             would be indistinguishable from an account that recorded none"
        ))
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
    ///
    /// `HistStore::bar_edges` is therefore INHERITED rather than overridden: its default derives the
    /// edges from this read, so it answers the empty `BarEdges` too, and a load of nothing has
    /// nothing to bound.
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
            let BookLevel { price: b, qty: bs } = bk.best_bid().unwrap_or(BookLevel::new(0.0, 0.0));
            let BookLevel { price: a, qty: as_ } =
                bk.best_ask().unwrap_or(BookLevel::new(0.0, 0.0));
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
        let rows = self.scan_book_updates_impl(symbol, range)?;
        // The visible half of the window-clock divergence (module doc). Reported on the BOOK lane
        // and not on the trade/quote lanes deliberately: the measured harm is a missing snapshot
        // ANCHOR, which only this lane carries, and a per-scan line on every lane would turn a
        // signal into noise in a batch that scans hundreds of markets.
        let n =
            other_clock_excludes(WindowClock::Ts, rows.iter().map(|u| (u.ts, u.local_ts)), range);
        if n > 0 {
            tracing::warn!(
                symbol,
                returned = rows.len(),
                outside_local_ts_window = n,
                "archive store: this window was scoped on ts; {n} of {} returned rows carry a \
                 `local_ts` outside it, so a `local_ts`-scoped run over the same window would not \
                 see them (vike_data::window_clock::WindowClock)",
                rows.len()
            );
        }
        Ok(rows)
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

    /// **The account plane REFUSES**, and this is the one `HistStore` answer on this store that is
    /// a statement about the STORE rather than about the data.
    ///
    /// These three used to read `Ok(Vec::new())` under the comment *"No account equity/exec-log
    /// series live in a market-data archive either"*, which is a true sentence attached to a false
    /// answer: the premise is about what this store CAN hold and the return value is what a store
    /// that HELD the kind says when it looked and found nothing in range. A caller asking an
    /// archive of published market Parquet for its own fills got `[]` and read *"you have no
    /// fills"*.
    ///
    /// That is the collision
    /// `docs/decisions/0080-the-account-funding-kind-takes-the-qualified-name.md` verdict 6 closed
    /// for `exec_funding`: one value for three facts — the series holds nothing in range, the
    /// series does not exist, and this backend cannot answer the question at all. It is also the
    /// defect this module's own doc already warns about in the loudest terms it has, over
    /// [`ArchiveParquetHistStore::scan_quotes`]: *"in a `HistStore`, 'I cannot answer this' must
    /// never be spelled the same way as 'the answer is none'"*. The quote lane was corrected on
    /// measured harm (a scalper scoring -2.74% where the truth was -27.64%); these three are the
    /// same shape, corrected before somebody pays for them.
    ///
    /// **What this costs a caller: nothing that was not already handled.** The three verbs reach
    /// production through exactly three helpers, every one of which propagates rather than
    /// swallowing: `crates/vike-data/src/exec_index.rs`'s `recent_seen_trade_ids` and
    /// `recent_order_statuses`, and `crates/vike-report/src/store.rs`'s `equity_curve_from_store`
    /// (plus `crates/vike-report/src/journal_read.rs`'s `fills_from_store`, which no binary reads
    /// yet). Their callers already treat an `Err` as a named degradation —
    /// `crates/vike-core/src/journal_view.rs` logs it per symbol and says what the reconcile pass
    /// loses, `crates/vike-report/src/tearsheet_cli.rs` prints it and falls back to the
    /// realized-only curve — so the refusal arrives where a message about it already exists. No
    /// call site changed for this; that was checked rather than assumed.
    ///
    /// ⚠ `load_bars` and `scan_symbol_properties` above deliberately KEEP their empty `Ok`, and the
    /// asymmetry is argued rather than accidental —
    /// `crates/vike-backtest/src/hist_replay.rs`'s `properties_source` calls `properties_as_of`
    /// (derived over `scan_symbol_properties`) once PER TICK PER SYMBOL and logs any error, so a
    /// refusal there turns an archive-backed replay's log into one warning per tick without
    /// changing a single fill. That is a call site the refusal makes WORSE, so it does not get one;
    /// the account lanes have no such caller.
    fn scan_equity(
        &self,
        venue: &str,
        symbol: &str,
        _range: TsRange,
    ) -> Result<Vec<EquitySample>, DataError> {
        Err(Self::no_account_plane("equity", "scan_equity", venue, symbol))
    }
    fn scan_exec_fills(&self, venue: &str, symbol: &str) -> Result<Vec<ExecFillRow>, DataError> {
        Err(Self::no_account_plane("exec_fill", "scan_exec_fills", venue, symbol))
    }
    fn scan_exec_orders(&self, venue: &str, symbol: &str) -> Result<Vec<ExecOrderRow>, DataError> {
        Err(Self::no_account_plane("exec_order", "scan_exec_orders", venue, symbol))
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

    // exec_funding / chain stay on the trait's own defaults, and those defaults now REFUSE — which
    // is the right answer for this store and the reason it grows no arm for either. cohort and
    // perp_metrics stay on the two defaults that still answer empty; `crate::store::hist`'s cohort section
    // carries the argument for why that pair was left, and the test below pins it.
}

// ---- the ARCHIVE's own read-side constants, moved down 2026-09-20 ---------------------------
//
// `VENUE` and `select_row_groups` were declared in `vike_backfill::vike_archive` — the HTTP
// DOWNLOADER — and imported up from this reader. They are read-side facts about the published
// files (which venue they carry; which row groups a token set can possibly be in), not about
// fetching, and holding them in the downloader is what forced this whole module to live above
// the engine. They live with the reader now, and the downloader imports them from here, which is
// downward. The pruning function is PURE over already-loaded Parquet metadata: no network, no
// credentials, no venue client.
/// `token_id` column's per-row-group min/max statistics. `None` (no filter) selects every row
/// group. An explicit-but-empty token set selects none. Byte-lexicographic comparison — the exact
/// comparison Parquet defines for `BYTE_ARRAY`/`Utf8` column statistics, and correct regardless of
/// whether the file happens to be globally sorted: a row group's own min/max are always true
/// bounds over that group's own rows, so exclusion never drops a real match. Falls back to
/// "include everything" whenever pruning data is unavailable (no `token_id` column found, or a
/// row group's column carries no statistics) — pruning is an optimization, never a filter that can
/// silently lose rows.
pub fn select_row_groups(md: &ParquetMetaData, tokens: Option<&HashSet<String>>) -> Vec<usize> {
    let total = md.num_row_groups();
    let Some(tokens) = tokens else {
        return (0..total).collect();
    };
    if tokens.is_empty() {
        return Vec::new();
    }
    let Some(col_idx) =
        md.file_metadata().schema_descr().columns().iter().position(|c| c.name() == "token_id")
    else {
        return (0..total).collect(); // no token_id column found — can't prune, include everything
    };
    (0..total)
        .filter(|&i| {
            let rg = md.row_group(i);
            let Some(stats) = rg.column(col_idx).statistics() else {
                return true; // no stats on this group's token_id chunk — include defensively
            };
            let (Some(min), Some(max)) = (stats.min_bytes_opt(), stats.max_bytes_opt()) else {
                return true;
            };
            tokens.iter().any(|t| {
                let tb = t.as_bytes();
                min <= tb && tb <= max
            })
        })
        .collect()
}

/// The only venue this archive publishes.
pub const VENUE: &str = "polymarket";

#[path = "archive_store_tests.rs"]
#[cfg(test)]
mod archive_store_tests;
