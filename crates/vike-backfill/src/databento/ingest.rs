//! Drive the pure Databento parsers into the `vike-data` hist store with idempotent commit keys.
//! `ingest_file` reads a fetched CSV file; `backfill` fetches then ingests. Commit key shape:
//! `databento:{kind}:{symbol}:{start}-{end}` (a re-run of the same window is a store no-op).

use std::path::Path;

use vike_data::{DataFusionHist, HistStore};

use crate::databento::client::{fetch_to_file, GetRange, HIST_BASE};
use crate::databento::parse;
use crate::error::CollectError;

/// Which schema/series a Databento CSV file holds.
#[derive(Debug, Clone)]
pub enum DbnKind {
    Trades,
    Ohlcv(String),
    QuotesMbp1,
    BookMbp10,
}

impl DbnKind {
    /// The Databento `schema` string for this kind.
    pub fn schema(&self) -> String {
        match self {
            DbnKind::Trades => "trades".into(),
            DbnKind::Ohlcv(iv) => format!("ohlcv-{iv}"),
            DbnKind::QuotesMbp1 => "mbp-1".into(),
            DbnKind::BookMbp10 => "mbp-10".into(),
        }
    }
    fn tag(&self) -> &'static str {
        match self {
            DbnKind::Trades => "trades",
            DbnKind::Ohlcv(_) => "bars",
            DbnKind::QuotesMbp1 => "quotes",
            DbnKind::BookMbp10 => "book",
        }
    }
}

/// Ingest an in-memory CSV body (the file-reading `ingest_file` is a thin wrapper). Returns rows
/// written to the store.
pub fn ingest_str(
    store: &DataFusionHist,
    venue: &str,
    symbol: &str,
    kind: &DbnKind,
    csv: &str,
    window: &str,
) -> Result<usize, CollectError> {
    let key = format!("databento:{}:{symbol}:{window}", kind.tag());
    let n = match kind {
        DbnKind::Trades => {
            store.append_trades(venue, symbol, &parse::parse_trades(csv), Some(&key))?
        }
        DbnKind::Ohlcv(iv) => {
            store.append_bars(venue, symbol, iv, &parse::parse_ohlcv(csv), Some(&key))?
        }
        DbnKind::QuotesMbp1 => {
            store.append_quotes(venue, symbol, &parse::parse_quotes_mbp1(csv), Some(&key))?
        }
        DbnKind::BookMbp10 => {
            store.append_book_updates(venue, symbol, &parse::parse_book_mbp10(csv), Some(&key))?
        }
    };
    Ok(n)
}

/// Read a fetched CSV file and ingest it.
pub fn ingest_file(
    store: &DataFusionHist,
    venue: &str,
    symbol: &str,
    kind: &DbnKind,
    path: &Path,
    window: &str,
) -> Result<usize, CollectError> {
    let csv = std::fs::read_to_string(path)
        .map_err(|e| CollectError::Fetch(format!("read {}: {e}", path.display())))?;
    ingest_str(store, venue, symbol, kind, &csv, window)
}

/// Fetch one range from Databento then ingest it. `venue` is the store namespace (vendor default
/// or a `--venue` override). Returns rows written.
#[allow(clippy::too_many_arguments)]
pub fn backfill(
    store: &DataFusionHist,
    api_key: &str,
    dataset: &str,
    venue: &str,
    symbol: &str,
    kind: &DbnKind,
    start: &str,
    end: &str,
    tmp: &Path,
) -> Result<usize, CollectError> {
    let schema = kind.schema();
    let req = GetRange { dataset, symbols: symbol, schema: &schema, start, end };
    let path = fetch_to_file(HIST_BASE, api_key, &req, tmp)?;
    ingest_file(store, venue, symbol, kind, &path, &format!("{start}-{end}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_schema_and_tag_strings() {
        assert_eq!(DbnKind::Trades.schema(), "trades");
        assert_eq!(DbnKind::Ohlcv("1m".into()).schema(), "ohlcv-1m");
        assert_eq!(DbnKind::QuotesMbp1.schema(), "mbp-1");
        assert_eq!(DbnKind::BookMbp10.schema(), "mbp-10");
        assert_eq!(DbnKind::Ohlcv("1d".into()).tag(), "bars");
    }
}
