//! Dukascopy tick backfill: fetch keyless `.bi5` history via `vike_dukascopy` and ingest it
//! into the DataFusion hist store (`vike_data::DataFusionHist`).
//!
//! Ported source: the fetch half is `vike_dukascopy` (itself a port of the Python
//! `dukascopy_source`); the ingest/derive half is the `vike_data::HistStore` seam. This module is
//! the seam between them — no venue code in vike-data, no store code in vike-dukascopy.
//!
//! Flow: `fetch_ticks_range` → [`tick_to_quote`] → `append_quotes` (idempotent by commit key) →
//! `resample_quotes_to_bars` (the parity-tested `vike_model::consolidate_quotes`).

use vike_data::{DataFusionHist, HistStore, TsRange};
use vike_dukascopy::{self as dukascopy, Tick};
use vike_model::QuoteTick;

use crate::error::CollectError;

/// Venue tag under which Dukascopy series live in the hist store (`venue=dukascopy` in the tree).
pub const VENUE: &str = "dukascopy";

/// Pure map: one Dukascopy [`Tick`] → a [`QuoteTick`]. Bid/ask pass through unchanged; the venue's
/// per-side tick *volumes* become the L1 *sizes* (bid_vol→bid_size, ask_vol→ask_size). No rounding
/// or arithmetic — the hist store's `to_bits()` parity gate must hold bit-exactly across this hop.
pub fn tick_to_quote(tick: &Tick, symbol: &str) -> QuoteTick {
    QuoteTick {
        ts: tick.ts,
        local_ts: 0,
        bid: tick.bid,
        ask: tick.ask,
        bid_size: tick.bid_vol,
        ask_size: tick.ask_vol,
        symbol: symbol.to_string(),
    }
}

/// The idempotency guard for a `[start_ms, end_ms]` backfill window: a re-run with the same window
/// is a no-op in the store (batch-level dedup — never per-row value dedup, per the store contract).
pub fn quote_commit_key(symbol: &str, start_ms: i64, end_ms: i64) -> String {
    format!("dukascopy:{symbol}:{start_ms}-{end_ms}")
}

/// Fetch Dukascopy tick history for `[start_ms, end_ms]`, map each tick to a [`QuoteTick`], and
/// `append_quotes` it into the store under `(venue=dukascopy, symbol)`. Idempotent by
/// [`quote_commit_key`]. Returns rows written (0 if the window was already ingested).
///
/// NOTE: this DOES network I/O (Dukascopy's public CDN) — the offline gate exercises the pure map +
/// ingest path directly with synthetic ticks; only the `#[ignore]`d live test drives this end to end.
pub fn backfill_dukascopy_quotes(
    hist: &DataFusionHist,
    symbol: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<usize, CollectError> {
    let ticks =
        dukascopy::fetch_ticks_range(symbol, start_ms, end_ms).map_err(CollectError::Fetch)?;
    let quotes: Vec<QuoteTick> = ticks.iter().map(|t| tick_to_quote(t, symbol)).collect();
    let key = quote_commit_key(symbol, start_ms, end_ms);
    Ok(hist.append_quotes(VENUE, symbol, &quotes, Some(&key))?)
}

/// Resample stored Dukascopy quotes → OHLCV bars at `interval` for `symbol` over `range`, writing
/// them back into the store. Thin wrapper over `HistStore::resample_quotes_to_bars`, which reads the
/// stored quote slice and folds it with the parity-tested `vike_model::consolidate_quotes`.
/// Idempotent: the derived batch is keyed by `(symbol, interval, range)` so re-running is a no-op.
/// Returns bars written.
pub fn resample_and_store_bars(
    hist: &DataFusionHist,
    symbol: &str,
    interval: &str,
    range: TsRange,
) -> Result<usize, CollectError> {
    let bound = |b: Option<i64>| b.map(|v| v.to_string()).unwrap_or_else(|| "*".to_string());
    let key = format!(
        "dukascopy-resample:{symbol}:{interval}:{}-{}",
        bound(range.start),
        bound(range.end),
    );
    Ok(hist.resample_quotes_to_bars(VENUE, symbol, interval, range, Some(&key))?)
}
