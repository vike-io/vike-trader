//! The one collect-run error type, shared by every venue collector (dukascopy, binance, …): an
//! ingest/read/resample failure from the hist store, a venue-fetch failure (network / decode), a
//! REQUEST this seam cannot express and refuses before touching either, a request that RAN but
//! left chunks the store refused to supersede unwritten, or a chunked request that was asked to
//! STOP between two of its chunks and did.

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
    /// A CHUNKED request was asked to STOP, and stopped at the boundary between two of its chunks:
    /// nothing failed. Every chunk before the boundary is written whole and stays written — a
    /// chunk's commit is one locked manifest publish, so it is there whole or not at all — and the
    /// chunk at the boundary and every one after it were never fetched. Repeating the request
    /// resumes, because the chunks it finished have their commit keys spent and are skipped.
    ///
    /// ⚠ **Distinct from every variant above, deliberately, and the distinction is the point.** It
    /// is not a [`CollectError::Fetch`] or a [`CollectError::Data`]: nothing failed, and "retry, or
    /// check the network" would send the reader after a fault that does not exist. It is not a
    /// [`CollectError::Refused`]: the request was valid, and some of it may already be written. And
    /// it is not a [`CollectError::SupersedeRefused`]: that one RAN to its end, while this one
    /// stopped before it, so "nothing was left unattempted" would be false of it. Folding it into any
    /// of them would also lose the one thing a stopped request must never be read as — an `Ok`. A
    /// window it did not finish is not in the store, so it never answers as a row count.
    ///
    /// The ingest that stops composes the payload: the boundary, what the chunks before it wrote,
    /// any chunk it skipped earlier, and that repeating the request resumes. Who asked it to stop
    /// is not its business — the probe it was handed (`&dyn Fn() -> bool`, `true` meaning "stop
    /// now") is its caller's, and the caller knows why.
    /// `crates/vike-backfill/src/klines.rs`'s `ingest_klines_chunked` and
    /// `crates/vike-backfill/src/venues/dukascopy.rs`'s `backfill_quotes_then_bars` are the two
    /// ingests that ask it, between chunks and never inside one.
    Stopped(String),
}

impl std::fmt::Display for CollectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CollectError::Data(e) => write!(f, "hist store: {e}"),
            CollectError::Fetch(e) => write!(f, "venue fetch: {e}"),
            CollectError::Refused(e) => write!(f, "refused: {e}"),
            CollectError::SupersedeRefused(e) => write!(f, "hist store: {e}"),
            CollectError::Stopped(e) => write!(f, "stopped: {e}"),
        }
    }
}

impl std::error::Error for CollectError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CollectError::Data(e) => Some(e),
            // None of these carries a nested cause: a fetch failure is already a rendered string from
            // the bridge, a refusal has no cause at all — nothing was attempted — a skipped-chunk
            // report is a rendered string of its own, the refusing store errors having been logged as
            // each chunk was skipped, and a stop is no failure at all.
            CollectError::Fetch(_)
            | CollectError::Refused(_)
            | CollectError::SupersedeRefused(_)
            | CollectError::Stopped(_) => None,
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
