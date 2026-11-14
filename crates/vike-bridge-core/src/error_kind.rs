//! Per-venue error taxonomy: the OPT-IN layer that refines the central
//! [`ErrorKind::classify`](crate::transport::ErrorKind::classify) with each venue's OWN documented
//! error-code table.
//!
//! ## Why a second layer instead of growing the central table
//! [`crate::transport`] already owns the venue-neutral baseline: the two transport sentinels
//! ([`E_TIMEOUT_AMBIGUOUS`](crate::transport::E_TIMEOUT_AMBIGUOUS) / `code == 0`), the
//! HTTP-status-as-code range, and a ~20-code seed of the most common Binance/Bybit/OKX business
//! codes. That seed is deliberately small and flat (it relies on the three venues' codes being
//! numerically disjoint) and it is consumed on LIVE order paths (hyperliquid's transport retry,
//! its `must_requery` submit arms). Growing it in place would change what those callers see.
//!
//! So this module adds a layer ABOVE it rather than editing it:
//!
//! ```text
//!   VenueTaxonomy.by_code(code)   -> Some(kind)   // this venue's official table, wins outright
//!   VenueTaxonomy.by_msg(msg)     -> Some(kind)   // substring heuristic — may REFINE only
//!   ErrorKind::classify(code,msg) -> kind         // the untouched central baseline
//! ```
//!
//! [`classify_venue`] walks exactly that order. With NO taxonomy supplied it is *definitionally*
//! `err.kind()` — so every existing caller is byte-identical and the whole layer is inert until a
//! venue opts in.
//!
//! ## The `by_msg` disposition guard (safety-critical)
//! `by_code` is authoritative: it is transcribed from the venue's own published table and each venue
//! crate pins that it never changes a baseline code's DISPOSITION. `by_msg` has no such standing —
//! it is an unanchored substring heuristic over text the venue can reword at will, and a stray match
//! on a body the baseline already classified is a live hazard in BOTH directions (a 5xx whose text
//! says "insufficient upstream capacity" turning a retryable outage into terminal rejects; a
//! `-2010 NEW_ORDER_REJECTED / "Market is closed."` turning a terminal reject into an unbounded
//! retry loop). So [`classify_venue`] lets `by_msg` win only when it AGREES with the baseline's
//! disposition — or when the baseline has no opinion at all
//! ([`ErrorKind::Unknown`](crate::transport::ErrorKind::Unknown)). Refinement within a disposition
//! (`-2010` "insufficient balance" `InvalidRequest` → `InsufficientFunds`, both terminal) is exactly
//! what `by_msg` is for and is untouched; a disposition FLIP from a substring is structurally
//! impossible.
//!
//! ## Layering
//! The tables themselves live in the VENUE crates (`vike-binance`/`vike-bybit`/`vike-okx`
//! `error_codes.rs`), not here: bridge-core sits BELOW the bridges and must not name them. This
//! module owns only the vocabulary — [`VenueTaxonomy`] is a pair of plain `fn` pointers, so a venue
//! publishes a `pub const TAXONOMY: VenueTaxonomy` and any consumer passes it down. No dyn, no
//! registry, no allocation.
//!
//! ## Safe posture on an unknown code
//! An unrecognized code falls through to [`ErrorKind::Unknown`](crate::transport::ErrorKind::Unknown),
//! which is neither retryable nor re-queryable — i.e. terminal
//! ([`is_terminal`](crate::transport::ErrorKind::is_terminal)). That is the intended
//! "default Fatal" posture: a venue code nobody has classified must abort the request, never spin
//! on it.

use crate::transport::{ErrorKind, VenueApiError};

/// Exact-code lookup into a venue's official error table. `None` = this venue's table has no rule
/// for `code` (fall through to the next layer). Written as a plain `fn` pointer so a venue crate can
/// publish it in a `const` and bridge-core never needs to name that crate.
pub type CodeTable = fn(i64) -> Option<ErrorKind>;

/// Substring/shape lookup over a venue's error MESSAGE — the fallback for venues that overload one
/// numeric code across many causes (Binance's `-2010 NEW_ORDER_REJECTED` carries "Account has
/// insufficient balance…", "Duplicate order sent", "Market is closed" all under one code). `None` =
/// no rule matched.
pub type MessageTable = fn(&str) -> Option<ErrorKind>;

/// One venue's error taxonomy: its exact-code table plus its message-substring fallback.
///
/// A venue crate publishes one of these as a `const`; consumers thread it into [`classify_venue`]
/// (or [`VenueApiError::kind_with`], which it is shaped to feed).
#[derive(Debug, Clone, Copy)]
pub struct VenueTaxonomy {
    /// The venue this table belongs to (`"binance"`), for logs and assertion messages.
    pub venue: &'static str,
    /// Exact venue business codes, from the venue's official error-code documentation.
    pub by_code: CodeTable,
    /// Message-substring fallback, for codes the venue overloads.
    pub by_msg: MessageTable,
}

impl VenueTaxonomy {
    /// This taxonomy's answer for `err`, or `None` to defer to the central baseline. Exactly the
    /// shape [`VenueApiError::kind_with`] wants, so a venue adapter can write
    /// `err.kind_with(|e| TAXONOMY.lookup(e))`.
    ///
    /// Applies the same `by_msg` disposition guard [`classify_venue`] does (see the module doc): a
    /// message substring may refine the baseline's bucket but never flip its disposition.
    pub fn lookup(&self, err: &VenueApiError) -> Option<ErrorKind> {
        if let Some(k) = (self.by_code)(err.code) {
            return Some(k);
        }
        let from_message = (self.by_msg)(&err.msg)?;
        let baseline = err.kind();
        (baseline == ErrorKind::Unknown || same_disposition(from_message, baseline))
            .then_some(from_message)
    }
}

/// Do two kinds prescribe the same ACTION (retry / re-query / stop)? The unit the `by_msg` guard and
/// the venue crates' adoption-safety pins both compare on: a reclassification that preserves this
/// triple is a refinement (it only sharpens diagnostics and the advised backoff), while one that
/// changes it changes what a caller DOES with a live order.
pub fn same_disposition(a: ErrorKind, b: ErrorKind) -> bool {
    (a.is_retryable(), a.must_requery(), a.is_terminal())
        == (b.is_retryable(), b.must_requery(), b.is_terminal())
}

/// Classify `err` under an OPTIONAL venue taxonomy: the venue's exact-code table first, then its
/// message substrings (guarded — see the module doc), then the untouched central baseline
/// ([`ErrorKind::classify`](crate::transport::ErrorKind::classify)).
///
/// `None` returns *precisely* `err.kind()` — the identity that keeps this whole layer inert until a
/// venue opts in.
pub fn classify_venue(taxonomy: Option<&VenueTaxonomy>, err: &VenueApiError) -> ErrorKind {
    match taxonomy {
        Some(t) => err.kind_with(|e| t.lookup(e)),
        None => err.kind(),
    }
}

/// What a caller should DO about a failed order submit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitDisposition {
    /// Transient — re-issue the same request after a backoff scaled by
    /// [`ErrorKind::backoff_scale`](crate::transport::ErrorKind::backoff_scale).
    Retry(ErrorKind),
    /// AMBIGUOUS: the venue may hold the order. Re-query status; NEVER synthesize a terminal reject
    /// and NEVER blind-retry (that double-submits).
    Requery,
    /// Terminal — synthesize the `OrderRejected` the venue-adapter contract requires, naming the
    /// kind so the rejection is diagnosable.
    Reject(ErrorKind),
}

/// Map a classified failure to the action a submit path should take.
///
/// The ordering is safety-critical and mirrors [`crate::transport`]'s existing invariants:
/// [`must_requery`](crate::transport::ErrorKind::must_requery) is checked FIRST so the ambiguous
/// timeout can never be shadowed into a retry (double-submit) or a reject (phantom position).
pub fn submit_disposition(kind: ErrorKind) -> SubmitDisposition {
    if kind.must_requery() {
        SubmitDisposition::Requery
    } else if kind.is_retryable() {
        SubmitDisposition::Retry(kind)
    } else {
        SubmitDisposition::Reject(kind)
    }
}

#[path = "error_kind_tests.rs"]
#[cfg(test)]
mod error_kind_tests;
