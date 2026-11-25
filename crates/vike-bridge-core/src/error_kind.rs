//! Per-venue error taxonomy: each venue's OWN documented error-code table, read before the venue-neutral
//! [`ErrorKind::classify`](crate::transport::ErrorKind::classify).
//!
//! ## Why every venue code lives in its venue's table
//! [`crate::transport`] owns only what is venue-neutral: the two transport sentinels
//! ([`E_TIMEOUT_AMBIGUOUS`](crate::transport::E_TIMEOUT_AMBIGUOUS) / `code == 0`), the HTTP-status-as-code range
//! and the rate-limit words. It reads no venue's business codes: one flat table across venues reads one venue's
//! code as another's as soon as two code ranges overlap (deribit's `10004` `order_not_found` was Bybit's "auth").
//! The tables live in the VENUE crates (`vike-binance`/`vike-bybit`/`vike-okx`/`vike-deribit`/`vike-ctrader`
//! `error_codes.rs`): bridge-core sits BELOW the bridges and must not name them. This module owns only the vocabulary —
//! [`VenueTaxonomy`] is plain `fn` pointers, so a venue publishes a `pub const TAXONOMY: VenueTaxonomy` and any
//! consumer passes it down. No dyn, no registry, no allocation.
//!
//! ```text
//!   VenueTaxonomy.by_code(code)   -> Some(kind)   // this venue's official table
//!   VenueTaxonomy.by_msg(msg)     -> Some(kind)   // substring heuristic — may REFINE only
//!   ErrorKind::classify(code,msg) -> kind         // the venue-neutral layer
//!   a venue code still Unknown    -> UnmappedCode // never final; WARN once per code
//! ```
//!
//! [`classify_venue`] walks exactly that order. With NO taxonomy supplied it is *definitionally* `err.kind()`.
//!
//! ## The `by_msg` disposition guard (safety-critical)
//! `by_code` is authoritative: it is transcribed from the venue's own published table. `by_msg` has no such
//! standing — it is an unanchored substring heuristic over text the venue can reword at will, and a stray match is
//! a live hazard in BOTH directions (a 5xx whose text says "insufficient upstream capacity" turning a retryable
//! outage into terminal rejects; a `-2010 NEW_ORDER_REJECTED / "Market is closed."` turning a terminal reject into
//! an unbounded retry loop). So a message may win only when it AGREES with the disposition of what the code
//! already says (its `by_code` row, else the venue-neutral layer), or when nothing has an opinion
//! ([`ErrorKind::Unknown`](crate::transport::ErrorKind::Unknown)). Refinement within a disposition (`-2010`
//! "insufficient balance" `InvalidRequest` → `InsufficientFunds`, both terminal) is exactly what `by_msg` is for; a
//! disposition FLIP from a substring is structurally impossible. A venue code with NO row has an opinion too: it is
//! unmapped, OUTCOME UNKNOWN, so no words make it a refusal (decision 0301: decide on a type, never on text).
//!
//! ## An unmapped venue code is never final
//! A venue code its table does not map is [`ErrorKind::UnmappedCode`](crate::transport::ErrorKind::UnmappedCode):
//! the error keeps the venue's code and words, its outcome is OUTCOME UNKNOWN (never resent, never called refused),
//! and the first sighting of each (venue, code) is logged at WARN so an operator sees the new code and adds its row.
//! Defaulting an unknown code to final is the mistake CCXT makes (an unmapped code becomes a non-retryable
//! `ExchangeError`), and it is how a venue's new transient code parks an account.
//!
//! ## The order-write rule
//! [`VenueTaxonomy::submit_outcome_unknown`] is the ONE rule every submit, amend and leverage-write arm of the
//! table-holding venues asks: the transport's sentinel, a row filed under
//! [`ErrorKind::Timeout`](crate::transport::ErrorKind::Timeout), or a code that says only that the server had
//! trouble or that an order already rests under the order client id ([`VenueTaxonomy::leaves_write_open`]), or a
//! venue code the table does not map. Every other row keeps its verdict, the venue's refusal. An unmapped code on a
//! write is OUTCOME UNKNOWN as everywhere else (its first sighting reported at WARN), resolved by the read keyed by
//! the order client id or the stuck-order watchdog (decision 0303): each table holds its venue's documented
//! order-path codes, so an unmapped one is a code the venue's page does not list, and what it did is not known.

use std::collections::BTreeSet;
use std::sync::Mutex;

use crate::transport::{E_TIMEOUT_AMBIGUOUS, ErrorKind, VenueApiError, is_venue_code};
use vike_execution::recon::ReconcileFetchError;

/// Exact-code lookup into a venue's official error table. `None` = this venue's table has no rule
/// for `code` (fall through to the next layer). Written as a plain `fn` pointer so a venue crate can
/// publish it in a `const` and bridge-core never needs to name that crate.
pub type CodeTable = fn(i64) -> Option<ErrorKind>;

/// Substring/shape lookup over a venue's error MESSAGE — the fallback for venues that overload one
/// numeric code across many causes (Binance's `-2010 NEW_ORDER_REJECTED` carries "Account has
/// insufficient balance…", "Duplicate order sent", "Market is closed" all under one code). `None` =
/// no rule matched.
pub type MessageTable = fn(&str) -> Option<ErrorKind>;

/// One venue's error taxonomy: its exact-code table, its message-substring fallback and its
/// server-trouble codes.
///
/// A venue crate publishes one of these as a `const`; consumers thread it into [`classify_venue`]
/// (or [`VenueApiError::kind_with`], which [`Self::lookup`] is shaped to feed).
#[derive(Debug, Clone, Copy)]
pub struct VenueTaxonomy {
    /// The venue this table belongs to (`"binance"`), for logs and assertion messages.
    pub venue: &'static str,
    /// Exact venue business codes, from the venue's official error-code documentation.
    pub by_code: CodeTable,
    /// Message-substring fallback, for codes the venue overloads.
    pub by_msg: MessageTable,
    /// Does this code, beyond the `Timeout` rows, say nothing final about a write ([`Self::submit_outcome_unknown`])?
    /// The server had trouble (binance `-1000`/`-1001`, bybit `10016`, okx `50001`/`50013`/`50026`, deribit
    /// `11094`/`13028`), or an order already rests under the order client id, so the order is one we sent (bybit
    /// `110072`/`170141`, okx `51016`; binance's `-4116` and `-2010` "Duplicate order sent." need the words, which its
    /// submit arms read beside this rule: `says_order_client_id_taken`).
    pub leaves_write_open: fn(i64) -> bool,
}

impl VenueTaxonomy {
    /// This taxonomy's answer for `err`, or `None` to defer to the venue-neutral layer. Exactly the
    /// shape [`VenueApiError::kind_with`] wants, so a venue adapter can write
    /// `err.kind_with(|e| TAXONOMY.lookup(e))` where only a mapped kind matters (a rate-limit check); a caller
    /// that acts on an unmapped code goes through [`classify_venue`].
    ///
    /// Applies the `by_msg` disposition guard (see the module doc): a message substring may refine what the
    /// code says but never flip its disposition, and a venue code with no row stands unmapped.
    pub fn lookup(&self, err: &VenueApiError) -> Option<ErrorKind> {
        let from_code = (self.by_code)(err.code);
        let standing = from_code.unwrap_or_else(|| match err.kind() {
            ErrorKind::Unknown if is_venue_code(err.code) => ErrorKind::UnmappedCode,
            neutral => neutral,
        });
        let from_message = (self.by_msg)(&err.msg)
            .filter(|m| standing == ErrorKind::Unknown || same_disposition(*m, standing));
        from_message.or(from_code)
    }

    /// Does a failed ORDER WRITE (a submit, an amend, a leverage POST) with this `code` leave its fate UNKNOWN
    /// (it may have landed)? The transport's own sentinel ([`E_TIMEOUT_AMBIGUOUS`]), a code this table files
    /// under [`ErrorKind::Timeout`] (the venue's own "status unknown" code, handed over as the body's code, so
    /// not the sentinel), [`Self::leaves_write_open`], or a venue code the table does not map (OUTCOME UNKNOWN, as
    /// [`classify_venue`] gives it; its first sighting reported at WARN). `true` means re-query by order client id
    /// (`crate::rest::resolve_ambiguous_submit`), never a terminal `OrderRejected`; `false` is the venue's
    /// refusal: every other row (binance futures' `-2019` and `-5022` among them), a pre-send failure, an HTTP
    /// status with no row, a code a bridge minted itself. The ONE rule behind every submit arm of the venues that
    /// hold a table.
    ///
    /// Not `classify_venue(..).must_requery()`: a server-trouble code is `ServerError` or `VenueMaintenance` by
    /// kind (NOT NOW on a read), which that would make a terminal reject of a write that may have landed.
    pub fn submit_outcome_unknown(&self, code: i64) -> bool {
        let row = (self.by_code)(code);
        let left_open = (self.leaves_write_open)(code);
        let unmapped = row.is_none() && !left_open && is_venue_code(code);
        if unmapped {
            report_unmapped(self.venue, code, None);
        }
        code == E_TIMEOUT_AMBIGUOUS
            || left_open
            || unmapped
            || row.is_some_and(ErrorKind::must_requery)
    }
}

/// Do two kinds prescribe the same ACTION (retry / re-query / stop)? The unit the `by_msg` guard and the
/// venue crates' pins compare on: a reclassification that keeps the
/// [`failure_outcome`](ErrorKind::failure_outcome) is a refinement (it only sharpens diagnostics), while one that
/// changes it changes what a caller DOES with a live order.
pub fn same_disposition(a: ErrorKind, b: ErrorKind) -> bool {
    a.failure_outcome() == b.failure_outcome()
}

/// Classify `err` under an OPTIONAL venue taxonomy: the venue's exact-code table first, then its message
/// substrings (guarded — see the module doc), then the venue-neutral layer
/// ([`ErrorKind::classify`](crate::transport::ErrorKind::classify)); a venue code nothing maps is
/// [`ErrorKind::UnmappedCode`], its first sighting logged at WARN.
///
/// `None` returns *precisely* `err.kind()`: with no table there is no venue to call a code unmapped for.
pub fn classify_venue(taxonomy: Option<&VenueTaxonomy>, err: &VenueApiError) -> ErrorKind {
    let Some(t) = taxonomy else { return err.kind() };
    match err.kind_with(|e| t.lookup(e)) {
        ErrorKind::Unknown if is_venue_code(err.code) => {
            report_unmapped(t.venue, err.code, Some(&err.msg));
            ErrorKind::UnmappedCode
        }
        kind => kind,
    }
}

/// `err` as a failed READ, for the caller that decides on it: its outcome judged under `taxonomy`
/// ([`classify_venue`], the venue's own code table first; an unmapped code is outcome unknown, never refused), its
/// words kept.
pub fn read_error(taxonomy: Option<&VenueTaxonomy>, err: &VenueApiError) -> ReconcileFetchError {
    ReconcileFetchError::new(classify_venue(taxonomy, err).failure_outcome(), err.to_string())
}

/// The (venue, code) pairs already reported unmapped, so each is said ONCE per process.
static UNMAPPED_SEEN: Mutex<BTreeSet<(&'static str, i64)>> = Mutex::new(BTreeSet::new());

/// shortcut: at most this many distinct codes are reported per process (a venue answering ever-new codes cannot
/// grow the set without bound); the rest are still classified, only not logged.
const UNMAPPED_REPORT_LIMIT: usize = 256;

/// Log `venue`'s unmapped `code` at WARN the first time this process sees it, with the venue's `words` when the
/// caller holds them.
fn report_unmapped(venue: &'static str, code: i64, words: Option<&str>) {
    let mut seen = vike_utility::mutex::lock(&UNMAPPED_SEEN);
    if seen.len() >= UNMAPPED_REPORT_LIMIT || !seen.insert((venue, code)) {
        return;
    }
    drop(seen);
    tracing::warn!(
        venue,
        code,
        words = words.unwrap_or(""),
        "a venue error code its table does not map, read as an unmapped code (never final); add its row to the \
         venue's error_codes table (said once per code)"
    );
}

#[path = "error_kind_tests.rs"]
#[cfg(test)]
mod error_kind_tests;
