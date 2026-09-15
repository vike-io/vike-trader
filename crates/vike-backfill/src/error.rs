//! The one collect-run error type, shared by every venue collector (dukascopy, binance, …): an
//! ingest/read/resample failure from the hist store, or a venue-fetch failure (network / decode).

use vike_data::DataError;

/// Errors from a collect run.
#[derive(Debug)]
pub enum CollectError {
    /// Ingest / read / resample failure from the hist store.
    Data(DataError),
    /// Venue history fetch failure (network / decode).
    Fetch(String),
}

impl std::fmt::Display for CollectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CollectError::Data(e) => write!(f, "hist store: {e}"),
            CollectError::Fetch(e) => write!(f, "venue fetch: {e}"),
        }
    }
}

impl std::error::Error for CollectError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CollectError::Data(e) => Some(e),
            CollectError::Fetch(_) => None,
        }
    }
}

impl From<DataError> for CollectError {
    fn from(e: DataError) -> Self {
        CollectError::Data(e)
    }
}
