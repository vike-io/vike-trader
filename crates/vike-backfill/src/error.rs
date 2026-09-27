//! The one collect-run error type, shared by every venue collector (dukascopy, binance, …): an
//! ingest/read/resample failure from the hist store, a venue-fetch failure (network / decode), or a
//! REQUEST this seam cannot express and refuses before touching either.

use vike_data::DataError;

/// Errors from a collect run.
#[derive(Debug)]
pub enum CollectError {
    /// Ingest / read / resample failure from the hist store.
    Data(DataError),
    /// Venue history fetch failure (network / decode).
    Fetch(String),
    /// The request was REFUSED before any network or store I/O — the seam cannot express it, so
    /// serving it would write something wrong rather than fail.
    ///
    /// ⚠ **Distinct from [`CollectError::Fetch`] deliberately, and the distinction is the point.**
    /// A `Fetch` says the venue was asked and did not answer; an operator reads it as "retry, or
    /// check the network". A refusal says the venue was never asked and never should be for this
    /// argument — retrying is exactly wrong. Folding one into the other would print
    /// `venue fetch: …` over a request that never left the box, which is the accurate-sounding
    /// message this tree keeps paying for elsewhere.
    ///
    /// The first instance is `crate::hyperliquid::backfill_hyperliquid_klines_by_symbol`: a
    /// dispatch seam carries ONE symbol, so it can only ever express `coin == symbol`, which is
    /// right for every HL perp and wrong for HL spot. Rather than guess a coin, it refuses and
    /// names the bin whose `--symbols vike=source` grammar CAN express the mapping.
    Refused(String),
}

impl std::fmt::Display for CollectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CollectError::Data(e) => write!(f, "hist store: {e}"),
            CollectError::Fetch(e) => write!(f, "venue fetch: {e}"),
            CollectError::Refused(e) => write!(f, "refused: {e}"),
        }
    }
}

impl std::error::Error for CollectError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CollectError::Data(e) => Some(e),
            // Neither carries a nested cause: a fetch failure is already a rendered string from the
            // bridge, and a refusal has no cause at all — nothing was attempted.
            CollectError::Fetch(_) | CollectError::Refused(_) => None,
        }
    }
}

impl From<DataError> for CollectError {
    fn from(e: DataError) -> Self {
        CollectError::Data(e)
    }
}
