//! The L2 book codec: events exploded to one row per level on write, regrouped into events on read.

use std::sync::Arc;

use datafusion::arrow::array::ArrayRef;
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::arrow::record_batch::RecordBatch;

use vike_model::{BookLevel, BookUpdate, BookUpdateKind};

use crate::store::hist::DataError;

use super::super::q;
use super::{
    RowSymbolFn, SeriesCodec, bool_col, f64_col, gather_bool, gather_f64, gather_i8, gather_i64,
    gather_str, i8_col, i64_col, opt_str_col,
};

// ---- book updates (kind=book; schema v1) -----------------------------------------------------
// One row PER LEVEL; rows of one event share (ts, seq, kind). An event with zero levels (status
// kinds; degenerate empty snapshot) writes ONE placeholder row is_bid=true, price=0.0, size=0.0,
// decoded back to empty level vecs (price==0&&size==0 is not a real level on any venue — Polymarket
// prices are in (0,1) and no venue quotes at exactly 0; the schema-version metadata is the escape
// hatch if that ever changes).
//
// The per-level ROW trio is generated like every other kind; the explode (`book_rows`) and regroup
// (`book_updates_from_rows`) around it are this kind's own and stay hand-written.

pub(super) const BOOK_SCHEMA_VERSION: &str = "1";

pub(super) fn book_kind_code(k: BookUpdateKind) -> i8 {
    match k {
        BookUpdateKind::Delta => 0,
        BookUpdateKind::Snapshot => 1,
        BookUpdateKind::GapStart => 2,
        BookUpdateKind::Stale => 3,
        BookUpdateKind::LiveResume => 4,
    }
}

fn book_kind_from(code: i8) -> Result<BookUpdateKind, DataError> {
    Ok(match code {
        0 => BookUpdateKind::Delta,
        1 => BookUpdateKind::Snapshot,
        2 => BookUpdateKind::GapStart,
        3 => BookUpdateKind::Stale,
        4 => BookUpdateKind::LiveResume,
        other => return Err(q(format!("unknown book kind code {other}"))),
    })
}

/// One exploded per-level row — the write-side flattening of a [`BookUpdate`].
#[derive(Clone)] // grouped writes sort symbol-major, which reorders rows
pub(crate) struct BookRow {
    pub ts: i64,
    pub local_ts: i64,
    pub seq: i64,
    pub kind: i8,
    pub is_bid: bool,
    pub price: f64,
    pub size: f64,
    pub tick_size: f64,
    /// The event's own symbol. Redundant under the per-symbol layout — the series path names it and
    /// `book_updates_from_rows` re-injects it from `ctx` — and REQUIRED under a grouped series,
    /// where one part holds many symbols and the path can no longer say whose row this is.
    ///
    /// Book is why this matters at all: one Polymarket token's market is ~352,935 book rows against
    /// ~17,364 quotes and ~1,087 trades, so grouping quotes and trades alone would leave ~95% of the
    /// volume un-grouped.
    pub symbol: String,
}

/// Explode events into per-level rows (placeholder row for zero-level events).
pub(crate) fn book_rows(updates: &[BookUpdate]) -> Vec<BookRow> {
    let mut out = Vec::new();
    for u in updates {
        let base = |is_bid: bool, price: f64, size: f64| BookRow {
            ts: u.ts,
            local_ts: u.local_ts,
            seq: u.seq as i64,
            kind: book_kind_code(u.kind),
            is_bid,
            price,
            size,
            tick_size: u.tick_size,
            symbol: u.symbol.clone(),
        };
        if u.bids.is_empty() && u.asks.is_empty() {
            out.push(base(true, 0.0, 0.0)); // placeholder — decodes to empty levels
            continue;
        }
        for &BookLevel { price: p, qty: s } in &u.bids {
            out.push(base(true, p, s));
        }
        for &BookLevel { price: p, qty: s } in &u.asks {
            out.push(base(false, p, s));
        }
    }
    out
}

series_codec! {
    Codec = BookCodec,
    Row = BookRow,
    rows = rows,
    ctx = symbol,
    schema_fn = book_schema,
    columns_fn = book_columns,
    decode_fn = book_rows_from_batch,
    meta = ("vike.schema.book", BOOK_SCHEMA_VERSION),
    row_symbol = symbol,
    fields {
        ts: i64 = |&i| rows[i].ts,
        local_ts: i64 = |&i| rows[i].local_ts,
        seq: i64 = |&i| rows[i].seq,
        kind: i8 = |&i| rows[i].kind,
        is_bid: bool = |&i| rows[i].is_bid,
        price: f64 = |&i| rows[i].price,
        size: f64 = |&i| rows[i].size,
        tick_size: f64 = |&i| rows[i].tick_size,
        // additive (series grouping) — same reasoning as the quote/trade codecs, and the one that
        // matters most by volume: book is ~95% of a Polymarket token's rows.
        symbol_col: str_add = |&i| rows[i].symbol.as_str(),
    },
    decode_row = |i| BookRow {
        ts: ts.value(i),
        local_ts: local_ts.value(i),
        seq: seq.value(i),
        kind: kind.value(i),
        is_bid: is_bid.value(i),
        price: price.value(i),
        size: size.value(i),
        tick_size: tick_size.value(i),
        // Absent OR empty falls back to `ctx`, exactly like quotes/trades: a per-symbol caller may
        // legally leave `BookUpdate::symbol` empty and be tagged from the path.
        symbol: symbol_col
            .as_mut()
            .map(|v| std::mem::take(&mut v[i]))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| symbol.to_string()),
    },
    sort_key = |row| (row.ts, row.seq),
}

/// Regroup sorted per-level rows into events. Boundary rule: a status-kind row is ALWAYS its own
/// single-row event; two consecutive Delta/Snapshot rows sharing `(seq, kind)` are treated as ONE
/// event's levels BY DESIGN.
///
/// This relies on Delta/Snapshot seqs being monotonic-DISTINCT across events (status kinds are
/// force-split by the `status` flag and per `crates/vike-marketdata/src/orderbook.rs` carry seq 0) — so two DISTINCT
/// events can never share `(seq, kind)`. The `debug_assert!` below guards that invariant: the rows of
/// one event share a frame timestamp, so if a feed bug ever emitted two distinct Delta/Snapshot events
/// with the same seq their rows would differ in `ts` and the assert fires instead of silently merging
/// them. It cannot false-positive on the legitimate multi-level case (those rows share `ts`/`local_ts`).
pub(crate) fn book_updates_from_rows(
    rows: Vec<BookRow>,
    symbol: &str,
) -> Result<Vec<BookUpdate>, DataError> {
    let mut out: Vec<BookUpdate> = Vec::new();
    for r in rows {
        let kind = book_kind_from(r.kind)?;
        // Resolve the row's symbol ONCE, and compare like with like below. Comparing the row's RAW
        // symbol against the event's RESOLVED one splits every multi-level event whose rows carry an
        // empty symbol (the legal per-symbol case): the event takes `ctx`, the next row is still "",
        // they differ, and each level becomes its own event.
        let row_symbol: &str = if r.symbol.is_empty() { symbol } else { &r.symbol };
        let status = !matches!(kind, BookUpdateKind::Delta | BookUpdateKind::Snapshot);
        let placeholder = r.price == 0.0 && r.size == 0.0;
        // A GROUPED part interleaves symbols, so a symbol CHANGE always starts a new event even when
        // (seq, kind) would otherwise fold — without this, two tokens sharing a seq would merge
        // into one BookUpdate carrying the other symbol's levels.
        let start_new = status
            || out.last().is_none_or(|u| {
                u.seq as i64 != r.seq
                    || u.symbol != row_symbol
                    || book_kind_code(u.kind) != book_kind_code(kind)
                    || !matches!(u.kind, BookUpdateKind::Delta | BookUpdateKind::Snapshot)
            });
        if start_new {
            out.push(BookUpdate {
                ts: r.ts,
                local_ts: r.local_ts,
                seq: r.seq as u64,
                kind,
                tick_size: r.tick_size,
                bids: Vec::new(),
                asks: Vec::new(),
                // The ROW's symbol when it has one (a grouped part holds many), else `ctx`. The
                // fallback is NOT redundant with decode's: `book_rows` copies `BookUpdate::symbol`
                // verbatim, and a per-symbol caller may legally leave that empty — so the
                // `book_rows` -> `book_updates_from_rows` round trip compaction performs would
                // otherwise lose the symbol entirely.
                symbol: row_symbol.to_string(),
            });
        } else {
            // Folding into the previous event because `(seq, kind)` matched — only sound if this is
            // genuinely the same frame (same ts/local_ts). Guards the monotonic-distinct-seq invariant.
            debug_assert!(
                out.last().is_some_and(|u| u.ts == r.ts && u.local_ts == r.local_ts),
                "book regroup: consecutive Delta/Snapshot rows share (seq, kind) but differ in \
                 timestamp — Delta/Snapshot seqs must be monotonic-distinct (would wrongly merge \
                 two events into one)"
            );
        }
        if !placeholder {
            let u = out.last_mut().expect("just pushed or existing");
            if r.is_bid {
                u.bids.push(BookLevel::new(r.price, r.size));
            } else {
                u.asks.push(BookLevel::new(r.price, r.size));
            }
        }
    }
    Ok(out)
}
