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
    /// The first instance is `vike_hyperliquid::history::HyperliquidKlines::fetch`: a dispatch
    /// seam carries ONE symbol, so it can only ever express `coin == symbol`, which is right for
    /// every HL perp and wrong for HL spot. Rather than guess a coin, it refuses and names the way
    /// forward — a canonical spelling for the pair in `vike-catalog`.
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

impl From<vike_data::source::SourceError> for CollectError {
    fn from(e: vike_data::source::SourceError) -> Self {
        match e {
            vike_data::source::SourceError::Fetch(s) => CollectError::Fetch(s),
            vike_data::source::SourceError::Refused(s) => CollectError::Refused(s),
        }
    }
}
