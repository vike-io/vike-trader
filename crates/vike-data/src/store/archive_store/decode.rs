//! Pure Arrow column decode of the archive's stream shapes, unit-tested against synthetic batches.

use datafusion::arrow::array::{
    BinaryArray, Decimal128Array, Int64Array, StringArray, UInt64Array,
};
use datafusion::arrow::record_batch::RecordBatch;
use vike_model::{BookLevel, BookUpdate, BookUpdateKind, QuoteTick, TradeTick};

use super::{CTX, PRICE_SCALE_DIVISOR, SIZE_SCALE_DIVISOR};
use crate::{DataError, TsRange};

// ---- Arrow column decode (pure; unit-tested against synthetic batches) --------------------------

pub(crate) fn str_col<'a>(b: &'a RecordBatch, name: &str) -> Result<&'a StringArray, DataError> {
    b.column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<StringArray>())
        .ok_or_else(|| DataError::Query(format!("{CTX}: missing/!string column {name}")))
}

pub(crate) fn i64_col<'a>(b: &'a RecordBatch, name: &str) -> Result<&'a Int64Array, DataError> {
    b.column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<Int64Array>())
        .ok_or_else(|| DataError::Query(format!("{CTX}: missing/!int64 column {name}")))
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

pub(crate) fn str_or_bin_col<'a>(
    b: &'a RecordBatch,
    name: &str,
) -> Result<StrOrBinCol<'a>, DataError> {
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
pub(crate) fn book_updates_from_batch(
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
pub(crate) fn quotes_from_l1_batch(
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

pub(crate) fn trades_from_batch(
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
