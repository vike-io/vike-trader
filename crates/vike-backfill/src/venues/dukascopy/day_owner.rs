//! Which of a series' keys own which days, for Dukascopy's three quote lanes. The table those keys are
//! judged by, and the day-owner lock both lanes take, are `crate::archive_import`'s.

use vike_data::SeriesCoverage;
use vike_store::HistoryStoreDataFusion;

use crate::archive_import::{DayOwnerKey, DayOwnerRole, DayOwners};
use crate::error::CollectError;

use super::{
    PUBLICATION_MARGIN_MS, archive_quote_commit_key, provisional_quote_commit_key,
    quote_commit_key, quote_series,
};

impl DayOwners {
    /// Parse `keys` — a quote series' commit log, as `vike_store::HistoryStoreDataFusion::series_facts` or
    /// `series_commits` answers it — for `symbol`'s three Dukascopy quote lanes.
    ///
    /// A key belongs to the series' lanes when it opens with one of the three formatters' heads FOR
    /// THIS SYMBOL — `dukascopy:EURUSD:`, `dukascopy-provisional:EURUSD:`, `dukascopy-archive:EURUSD:`,
    /// each derived from its formatter rather than spelled again here. Every other key is not a
    /// Dukascopy quote lane's and owns no day: another producer's, or another symbol's sharing a
    /// grouped leaf.
    pub fn parse(symbol: &str, keys: &[String]) -> Self {
        let heads = [
            (DayOwnerRole::Http, key_head(quote_commit_key, symbol)),
            (DayOwnerRole::Provisional, key_head(provisional_quote_commit_key, symbol)),
            (DayOwnerRole::Archive, key_head(archive_quote_commit_key, symbol)),
        ];
        let keys = keys
            .iter()
            .filter_map(|key| {
                heads.iter().find_map(|(lane, head)| {
                    let bounds = key.strip_prefix(head.as_str())?;
                    Some(DayOwnerKey { lane: *lane, range: parse_bounds(bounds), key: key.clone() })
                })
            })
            .collect();
        Self::new(PUBLICATION_MARGIN_MS, keys)
    }
}

/// The head `format` writes for `symbol` before its `{start}-{end}` bounds — `dukascopy:EURUSD:` for
/// [`quote_commit_key`] — read off the formatter itself with the bounds `0-0` cut away, so the three
/// templates are spelled once, in their formatters, and nowhere in the parser.
pub(super) fn key_head(format: fn(&str, i64, i64) -> String, symbol: &str) -> String {
    let mut head = format(symbol, 0, 0);
    head.truncate(head.len() - "0-0".len());
    head
}

/// The `{start}-{end}` bounds of a Dukascopy quote key, or `None` when they do not parse. The split
/// is the first `-` after the first character with an `i64` on each side, so a pre-1970 (negative)
/// bound on either side reads correctly.
pub(super) fn parse_bounds(bounds: &str) -> Option<(i64, i64)> {
    bounds
        .char_indices()
        .skip(1)
        .filter(|&(_, c)| c == '-')
        .find_map(|(at, _)| Some((bounds[..at].parse().ok()?, bounds[at + 1..].parse().ok()?)))
}

/// `symbol`'s Dukascopy quote series' coverage and commit log, from ONE manifest parse
/// (`vike_store::HistoryStoreDataFusion::series_facts`): what a plan that writes nothing — a dry run —
/// classifies its days against through [`DayOwners::parse`], without taking the day-owner lock. An
/// import that WRITES takes the same read under the lock instead, through
/// `crate::archive_import::ArchiveImport::begin`.
pub fn quote_series_facts(
    hist: &HistoryStoreDataFusion,
    symbol: &str,
) -> Result<(SeriesCoverage, Vec<String>), CollectError> {
    Ok(hist.series_facts(&quote_series(symbol))?)
}
