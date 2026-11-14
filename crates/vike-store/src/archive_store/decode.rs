//! Pure Arrow column decode of the archive's stream shapes, unit-tested against synthetic batches.
//!
//! **The ONE decoder of the `data.vike.io` archive's three streams** — `book_events`, `trades` and
//! `l1_quotes` — for `vike_backfill`'s `vike_archive` ingest, which decodes the files a row group at
//! a time and used to keep a near-copy of every function here. Which rows a reader wants is the
//! [`RowScope`] argument, never how a row decodes: the ingest asks for every row of a coarsely pruned
//! row group, each attributed to its own `token_id`.

use datafusion::arrow::array::{
    BinaryArray, Decimal128Array, Float64Array, Int64Array, StringArray, UInt64Array,
};
use datafusion::arrow::record_batch::RecordBatch;
use vike_marketdata::{L2BookLevel, L2BookUpdate, L2BookUpdateKind, TickQuote, TickTrade};

use super::{CTX, PRICE_SCALE_DIVISOR, SIZE_SCALE_DIVISOR};
use vike_data::{HistoryStoreError, TimestampRange};

// ---- Arrow column decode (pure; unit-tested against synthetic batches) --------------------------

pub(crate) fn str_col<'a>(
    b: &'a RecordBatch,
    name: &str,
) -> Result<&'a StringArray, HistoryStoreError> {
    b.column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<StringArray>())
        .ok_or_else(|| HistoryStoreError::Query(format!("{CTX}: missing/!string column {name}")))
}

pub(crate) fn i64_col<'a>(
    b: &'a RecordBatch,
    name: &str,
) -> Result<&'a Int64Array, HistoryStoreError> {
    b.column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<Int64Array>())
        .ok_or_else(|| HistoryStoreError::Query(format!("{CTX}: missing/!int64 column {name}")))
}

fn u64_col<'a>(b: &'a RecordBatch, name: &str) -> Result<&'a UInt64Array, HistoryStoreError> {
    b.column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<UInt64Array>())
        .ok_or_else(|| HistoryStoreError::Query(format!("{CTX}: missing/!uint64 column {name}")))
}

/// The archive's `trades` stream carries `price`/`size` as plain `Float64`, unlike the
/// `book_events` shape's `Decimal128`.
fn f64_col<'a>(b: &'a RecordBatch, name: &str) -> Result<&'a Float64Array, HistoryStoreError> {
    b.column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<Float64Array>())
        .ok_or_else(|| HistoryStoreError::Query(format!("{CTX}: missing/!float64 column {name}")))
}

fn decimal_col<'a>(
    b: &'a RecordBatch,
    name: &str,
) -> Result<&'a Decimal128Array, HistoryStoreError> {
    b.column_by_name(name).and_then(|c| c.as_any().downcast_ref::<Decimal128Array>()).ok_or_else(
        || HistoryStoreError::Query(format!("{CTX}: missing/!decimal128 column {name}")),
    )
}

fn dec(arr: &Decimal128Array, i: usize, divisor: f64) -> f64 {
    arr.value(i) as f64 / divisor
}

/// `event_type`/`side` decode as plain `Utf8` on the real family day-files and as raw `Binary` on
/// the flat `data.vike.io/archive` layout (ClickHouse's `Enum8` Parquet export writes raw bytes
/// without a UTF8 annotation; the family export's pipeline re-materializes the same two columns as
/// `Utf8` — both measured against real files). This enum is the ONE decode of either physical
/// shape, so a file of either shape decodes without error.
pub(crate) enum StrOrBinCol<'a> {
    Utf8(&'a StringArray),
    Bin(&'a BinaryArray),
}

impl StrOrBinCol<'_> {
    /// UTF-8(-lossy for the `Binary` case) decode of one row (malformed bytes degrade to `""`
    /// rather than panicking — one bad row must not fail a whole scan).
    pub(crate) fn value(&self, i: usize) -> &str {
        match self {
            StrOrBinCol::Utf8(a) => a.value(i),
            StrOrBinCol::Bin(a) => std::str::from_utf8(a.value(i)).unwrap_or(""),
        }
    }
}

pub(crate) fn str_or_bin_col<'a>(
    b: &'a RecordBatch,
    name: &str,
) -> Result<StrOrBinCol<'a>, HistoryStoreError> {
    let col = b
        .column_by_name(name)
        .ok_or_else(|| HistoryStoreError::Query(format!("{CTX}: missing column {name}")))?;
    if let Some(a) = col.as_any().downcast_ref::<StringArray>() {
        return Ok(StrOrBinCol::Utf8(a));
    }
    if let Some(a) = col.as_any().downcast_ref::<BinaryArray>() {
        return Ok(StrOrBinCol::Bin(a));
    }
    Err(HistoryStoreError::Query(format!("{CTX}: column {name} is neither string nor binary")))
}

/// Decode a `[[price,size],...]` JSON depth column (plain numbers — NOT pmxt's stringified-number
/// variant `vike_backfill`'s `pmxt::map::parse_levels` handles). Empty string (delta/status/
/// trade rows) and malformed JSON both degrade to an empty depth rather than a decode error — one
/// bad row must not fail a whole scan. The ONE copy: `vike_backfill`'s `events_api` decodes its
/// ladders through it too.
pub fn parse_levels_json(json: &str) -> Vec<L2BookLevel> {
    if json.is_empty() {
        return Vec::new();
    }
    serde_json::from_str::<Vec<L2BookLevel>>(json).unwrap_or_default()
}

/// The book update a row of the `book_events` shape is, read from its `event_type` and `status`:
/// `None` for an unrecognized `status` label (skipped rather than guessed at) and for every other
/// `event_type` (`trade` is no book update; `tick_size_change` carries none of its own). The ONE
/// reading, for this archive decoder and `vike_backfill`'s `events_api`: two sources of one tape.
pub fn book_kind_of(event_type: &str, status: &str) -> Option<L2BookUpdateKind> {
    match event_type {
        "book" => Some(L2BookUpdateKind::Snapshot),
        "price_change" => Some(L2BookUpdateKind::Delta),
        "status" => match status {
            "gap_start" => Some(L2BookUpdateKind::GapStart),
            "stale" => Some(L2BookUpdateKind::Stale),
            "live_resume" => Some(L2BookUpdateKind::LiveResume),
            _ => None,
        },
        _ => None,
    }
}

/// [`vike_marketdata::TickTrade::is_buyer_maker`] of a trade whose `side` is the TAKER side: `"sell"`
/// means the taker sold, so the buyer was the resting maker. The ONE reading, as [`book_kind_of`].
pub fn is_buyer_maker_of(side: &str) -> bool {
    side == "sell"
}

/// Which rows of a batch a decoder keeps. Every decoded row is attributed to its OWN `token_id`
/// either way — under [`RowScope::Symbol`] that is the asked symbol, because no other row is kept.
#[derive(Debug, Clone, Copy)]
pub enum RowScope<'a> {
    /// Every row — the ingest walk, whose (coarsely pruned) row groups mix many tokens in one
    /// batch and are split per symbol by the caller.
    All,
    /// Only `symbol`'s rows whose `ts` falls inside `range` (inclusive, scoped by `ts`, the
    /// venue/frame clock).
    Symbol { symbol: &'a str, range: TimestampRange },
}

impl RowScope<'_> {
    fn keeps(&self, token: &str, ts: i64) -> bool {
        match *self {
            RowScope::All => true,
            RowScope::Symbol { symbol, range } => {
                token == symbol
                    && ts >= range.start.unwrap_or(i64::MIN)
                    && ts <= range.end.unwrap_or(i64::MAX)
            }
        }
    }
}

/// Decode one `book_events`-shaped batch into [`L2BookUpdate`]s for the rows `scope` keeps.
/// `trade`/`tick_size_change` rows and an unrecognized `status` label are skipped (a `trade` row is no
/// book update; `tick_size_change` carries no `L2BookUpdate` of its own — state-only, as in pmxt's
/// mapper).
pub fn book_updates_from_batch(
    b: &RecordBatch,
    scope: RowScope<'_>,
) -> Result<Vec<L2BookUpdate>, HistoryStoreError> {
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

    let mut out = Vec::new();
    for i in 0..b.num_rows() {
        let row_timestamp = ts.value(i);
        if !scope.keeps(token_id.value(i), row_timestamp) {
            continue;
        }
        let Some(kind) = book_kind_of(event_type.value(i), status.value(i)) else { continue };
        let (row_bids, row_asks) = match kind {
            L2BookUpdateKind::Snapshot => {
                (parse_levels_json(bids.value(i)), parse_levels_json(asks.value(i)))
            }
            L2BookUpdateKind::Delta => {
                let level = L2BookLevel {
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
        out.push(L2BookUpdate {
            timestamp: row_timestamp,
            local_timestamp: local_ts.value(i),
            sequence: seq.value(i),
            kind,
            tick_size: dec(tick_size, i, PRICE_SCALE_DIVISOR),
            bids: row_bids,
            asks: row_asks,
            symbol: token_id.value(i).to_string(),
        });
    }
    Ok(out)
}

/// Decode one batch of the archive's own `trades` stream into [`TickTrade`]s for the rows `scope`
/// keeps. `price`/`size` are plain `Float64` here (unlike the `book_events` shape's `Decimal128`),
/// and `side` is the TAKER side ([`is_buyer_maker_of`]). `side` is read through [`StrOrBinCol`], so a
/// `Binary` export decodes too.
pub fn trades_from_batch(
    b: &RecordBatch,
    scope: RowScope<'_>,
) -> Result<Vec<TickTrade>, HistoryStoreError> {
    let token_id = str_col(b, "token_id")?;
    let ts = i64_col(b, "ts")?;
    let local_ts = i64_col(b, "local_ts")?;
    let price = f64_col(b, "price")?;
    let size = f64_col(b, "size")?;
    let side = str_or_bin_col(b, "side")?;

    let mut out = Vec::new();
    for i in 0..b.num_rows() {
        let row_timestamp = ts.value(i);
        if !scope.keeps(token_id.value(i), row_timestamp) {
            continue;
        }
        out.push(TickTrade {
            timestamp: row_timestamp,
            local_timestamp: local_ts.value(i),
            price: price.value(i),
            size: size.value(i),
            is_buyer_maker: is_buyer_maker_of(side.value(i)),
            symbol: token_id.value(i).to_string(),
        });
    }
    Ok(out)
}

/// Decode one `l1_quotes` batch into [`TickQuote`]s for the rows `scope` keeps.
///
/// This is the archive's OWN recorded top-of-book — the `poly-l2-recorder` writes L1 and L2 as
/// separate streams, and every family/flat date partition ships `l1_quotes.parquet` beside
/// `book_events.parquet`. Same scale divisors as every other column here (`bid`/`ask` scale-4,
/// sizes scale-6).
pub fn quotes_from_l1_batch(
    b: &RecordBatch,
    scope: RowScope<'_>,
) -> Result<Vec<TickQuote>, HistoryStoreError> {
    let token_id = str_col(b, "token_id")?;
    let ts = i64_col(b, "ts")?;
    let local_ts = i64_col(b, "local_ts")?;
    let bid = decimal_col(b, "bid")?;
    let ask = decimal_col(b, "ask")?;
    let bid_size = decimal_col(b, "bid_size")?;
    let ask_size = decimal_col(b, "ask_size")?;

    let mut out = Vec::new();
    for i in 0..b.num_rows() {
        let row_timestamp = ts.value(i);
        if !scope.keeps(token_id.value(i), row_timestamp) {
            continue;
        }
        out.push(TickQuote {
            timestamp: row_timestamp,
            local_timestamp: local_ts.value(i),
            bid: dec(bid, i, PRICE_SCALE_DIVISOR),
            ask: dec(ask, i, PRICE_SCALE_DIVISOR),
            bid_size: dec(bid_size, i, SIZE_SCALE_DIVISOR),
            ask_size: dec(ask_size, i, SIZE_SCALE_DIVISOR),
            symbol: token_id.value(i).to_string(),
        });
    }
    Ok(out)
}
