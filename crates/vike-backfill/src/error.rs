//! The one collect-run error type, shared by every venue collector (dukascopy, binance, …): an
//! ingest/read/resample failure from the hist store, a venue-fetch failure (network / decode), a
//! REQUEST this seam cannot express and refuses before touching either, or a request that RAN but
//! left chunks the store refused to supersede unwritten.

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
    /// The request RAN to its end, but the store REFUSED to supersede one or more of its SETTLED
    /// chunks and those chunks were SKIPPED — every other chunk is written, and stays written.
    ///
    /// ⚠ **Distinct from [`CollectError::Data`] deliberately, and the distinction is the point.** A
    /// `Data` error aborts the request at the chunk that failed: what it leaves is "the chunks before
    /// it written, the chunks after it never attempted", and the reader's move is to retry. This one
    /// leaves the opposite — nothing was left unattempted, exactly the NAMED chunks are missing, and
    /// a retry of them is the one thing that cannot help. Folding it into `Data` would print one
    /// store error over a request that wrote most of its window and point the reader at the wrong
    /// move. ⚠ **Nor is it [`CollectError::Refused`]**, whose refusal comes BEFORE any I/O: this one
    /// comes after the writes it leaves in place, so "nothing was written" would be false of it.
    ///
    /// The payload is the rendered report, remedy included, and the module that skipped the chunks
    /// composes it — the first instance is `crates/vike-backfill/src/venues/dukascopy.rs`'s
    /// `backfill_quotes_then_bars`, whose doc carries the mechanism (a provisional entry that
    /// background compaction folded into a multi-key part) and why the refusal is permanent. Its
    /// `Display` keeps the `hist store: ` prefix `Data`'s carries: it IS the store that said no, and
    /// text-only classifiers of a stringified error read that prefix.
    SupersedeRefused(String),
}

impl std::fmt::Display for CollectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CollectError::Data(e) => write!(f, "hist store: {e}"),
            CollectError::Fetch(e) => write!(f, "venue fetch: {e}"),
            CollectError::Refused(e) => write!(f, "refused: {e}"),
            CollectError::SupersedeRefused(e) => write!(f, "hist store: {e}"),
        }
    }
}

impl std::error::Error for CollectError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CollectError::Data(e) => Some(e),
            // None of these carries a nested cause: a fetch failure is already a rendered string from
            // the bridge, a refusal has no cause at all — nothing was attempted — and a skipped-chunk
            // report is a rendered string of its own, the refusing store errors having been logged as
            // each chunk was skipped.
            CollectError::Fetch(_)
            | CollectError::Refused(_)
            | CollectError::SupersedeRefused(_) => None,
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
