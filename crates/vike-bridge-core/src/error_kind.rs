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
pub type MsgTable = fn(&str) -> Option<ErrorKind>;

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
    pub by_msg: MsgTable,
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
        let from_msg = (self.by_msg)(&err.msg)?;
        let baseline = err.kind();
        (baseline == ErrorKind::Unknown || same_disposition(from_msg, baseline)).then_some(from_msg)
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

/// Which classification path an [`crate::exec_actor::ExecActor`] takes on a submit failure.
///
/// **Default is [`Legacy`](ExecErrorPolicy::Legacy)**: the venues' own behavior — same event, same
/// reason string. `Classified` is the opt-in flip.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ExecErrorPolicy {
    /// The venues' own behavior, reproduced EXACTLY — two arms, not one:
    ///
    /// 1. `code == E_TIMEOUT_AMBIGUOUS` → [`SubmitDisposition::Requery`], emitting NOTHING, as
    ///    `binance/spot.rs`, `binance/perp.rs` and the bybit/okx twins match the ambiguous sentinel
    ///    FIRST and route it to `resolve_ambiguous_submit`. An adapter that routes its submit
    ///    failure through the seam at `Legacy` therefore KEEPS its re-query arm; collapsing it into
    ///    a terminal reject would emit a false rejection over a possibly-filled order (phantom
    ///    position).
    /// 2. everything else → a terminal `OrderRejected` carrying the venue's RAW message, no retry.
    ///
    /// Conservative but blunt on arm 2 — it rejects an order that a rate-limit backoff would have
    /// placed. That is what [`Classified`](ExecErrorPolicy::Classified) fixes.
    #[default]
    Legacy,
    /// Route the failure through [`submit_disposition`]: retry the transient kinds, re-query the
    /// ambiguous timeout, and synthesize the terminal reject only for genuinely terminal kinds —
    /// with the [`ErrorKind`] named in the reason so the reject is diagnosable downstream.
    Classified,
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

/// The single decision function behind [`ExecErrorPolicy::Classified`]: map a classified failure to
/// the action a submit path should take.
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

/// How many times / for how long a submit may be re-issued before the classified path gives up and
/// discharges the venue-adapter contract with a terminal `OrderRejected`.
///
/// **Why the budget lives in the seam, not in each adapter.** `Retry` emits nothing, by design — the
/// order's fate is still open. That is only safe if SOMETHING eventually closes it: without a bound,
/// a persistently-retryable classification (an OKX maintenance window, a Cloudflare 403 →
/// `RateLimited`) leaves the order in `Submitted` forever with neither an `OrderAccepted` nor an
/// `OrderRejected` — precisely the "no order may silently vanish" guarantee the contract calls
/// non-negotiable. Putting the bound here means no adapter can regress the guarantee by *omission*:
/// the seam converts an exhausted `Retry` into `Reject` and emits the terminal event itself.
///
/// [`SubmitDisposition::Requery`] is deliberately NOT budget-converted — see
/// [`ExecActor::on_submit_error`](crate::exec_actor::ExecActor::on_submit_error).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubmitRetryBudget {
    /// Total submit ATTEMPTS allowed (the first try counts). `0` or `1` = never retry.
    pub max_attempts: u32,
    /// Wall-clock ceiling across the whole retry sequence. `0` = no time bound.
    pub max_elapsed_ms: u64,
}

impl Default for SubmitRetryBudget {
    /// Conservative for an ORDER submit: a handful of tries inside half a minute. A quote that is
    /// half a minute stale is not the quote the operator wanted placed, so exhausting the budget and
    /// rejecting is the honest answer — the operator (or the strategy) can re-issue at a fresh price.
    fn default() -> Self {
        Self { max_attempts: 5, max_elapsed_ms: 30_000 }
    }
}

impl SubmitRetryBudget {
    /// Has `attempt` (1-based: `1` is the original submit) run the budget out?
    pub fn exhausted(&self, attempt: SubmitAttempt) -> bool {
        attempt.attempt >= self.max_attempts.max(1)
            || (self.max_elapsed_ms > 0 && attempt.elapsed_ms >= self.max_elapsed_ms)
    }
}

/// Where a submit is in its retry sequence, supplied by the caller's loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SubmitAttempt {
    /// 1-based attempt number: `1` on the ORIGINAL submit, `2` on the first re-issue, …
    pub attempt: u32,
    /// Milliseconds elapsed since the original submit.
    pub elapsed_ms: u64,
}

impl SubmitAttempt {
    /// The original submit (attempt 1, zero elapsed).
    pub fn first() -> Self {
        Self { attempt: 1, elapsed_ms: 0 }
    }

    /// The next attempt in the sequence, `elapsed_ms` measured from the original submit.
    pub fn next(self, elapsed_ms: u64) -> Self {
        Self { attempt: self.attempt.saturating_add(1), elapsed_ms }
    }
}

/// The reason string an exhausted-retry [`SubmitDisposition::Reject`] carries — same shape as
/// [`reject_reason`] plus the budget evidence, so an operator can tell "the venue said no" apart from
/// "we gave up retrying".
pub fn retry_exhausted_reason(kind: ErrorKind, attempt: SubmitAttempt, venue_msg: &str) -> String {
    format!(
        "{} (retry budget exhausted after {} attempt(s), {}ms)",
        reject_reason(kind, venue_msg),
        attempt.attempt,
        attempt.elapsed_ms
    )
}

/// The reason string a [`SubmitDisposition::Reject`] carries: the classified kind followed by the
/// venue's own message, so an operator reading a rejected order sees BOTH the taxonomy bucket and
/// the raw venue text. Used only on the `Classified` path — `Legacy` keeps the bare venue message.
pub fn reject_reason(kind: ErrorKind, venue_msg: &str) -> String {
    if venue_msg.is_empty() {
        format!("venue error [{kind}]")
    } else {
        format!("venue error [{kind}]: {venue_msg}")
    }
}

#[path = "error_kind_tests.rs"]
#[cfg(test)]
mod error_kind_tests;
