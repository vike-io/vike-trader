//! Drive the pure Tardis parsers into the hist store, one day at a time, with idempotent commit
//! keys `tardis:{kind}:{symbol}:{YYYY-MM-DD}`. `backfill_range` iterates a list of days, skipping
//! absent ones (client `Ok(None)`). Bars are NOT a Tardis data type — they are produced by
//! resampling ingested quotes downstream (the bin's `--data-type bars` path), so this module only
//! ingests the three native series.

use std::collections::HashSet;

use vike_data::{DataFusionHist, HistStore};

use crate::error::CollectError;
use crate::tardis::client::fetch_day;
use crate::tardis::parse;

/// Which Tardis dataType a file holds.
#[derive(Debug, Clone)]
pub enum TardisKind {
    Trades,
    Quotes,
    BookL2,
}

impl TardisKind {
    pub fn data_type(&self) -> &'static str {
        match self {
            TardisKind::Trades => "trades",
            TardisKind::Quotes => "quotes",
            TardisKind::BookL2 => "incremental_book_L2",
        }
    }
    fn tag(&self) -> &'static str {
        match self {
            TardisKind::Trades => "trades",
            TardisKind::Quotes => "quotes",
            TardisKind::BookL2 => "book",
        }
    }
}

/// Ingest one day's decompressed CSV body. Returns rows written.
pub fn ingest_day_str(
    store: &DataFusionHist,
    venue: &str,
    symbol: &str,
    kind: &TardisKind,
    csv: &str,
    day: &str,
) -> Result<usize, CollectError> {
    let key = format!("tardis:{}:{symbol}:{day}", kind.tag());
    let n = match kind {
        TardisKind::Trades => {
            store.append_trades(venue, symbol, &parse::parse_trades(csv), Some(&key))?
        }
        TardisKind::Quotes => {
            store.append_quotes(venue, symbol, &parse::parse_quotes(csv), Some(&key))?
        }
        TardisKind::BookL2 => {
            store.append_book_updates(venue, symbol, &parse::parse_book_l2(csv), Some(&key))?
        }
    };
    Ok(n)
}

/// Fetch + ingest each day in `days` (`(year, month, day)`), skipping absent days. Returns total
/// rows written. `_seen` guards against a caller passing duplicate days.
pub fn backfill_range(
    store: &DataFusionHist,
    api_key: Option<&str>,
    exchange: &str,
    venue: &str,
    symbol: &str,
    kind: &TardisKind,
    days: &[(i32, u32, u32)],
) -> Result<usize, CollectError> {
    let mut total = 0usize;
    let mut seen = HashSet::new();
    for &(y, m, d) in days {
        if !seen.insert((y, m, d)) {
            continue;
        }
        let day = format!("{y:04}-{m:02}-{d:02}");
        match fetch_day(api_key, exchange, kind.data_type(), y, m, d, symbol)? {
            Some(csv) => total += ingest_day_str(store, venue, symbol, kind, &csv, &day)?,
            None => tracing::info!(%day, %symbol, "tardis day absent (404) — skipped"),
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_data_type_and_tag() {
        assert_eq!(TardisKind::Trades.data_type(), "trades");
        assert_eq!(TardisKind::Quotes.data_type(), "quotes");
        assert_eq!(TardisKind::BookL2.data_type(), "incremental_book_L2");
        assert_eq!(TardisKind::BookL2.tag(), "book");
    }
}
