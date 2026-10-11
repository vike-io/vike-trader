//! What an engaged HALT sentinel lets OUT — the ONE predicate every [`crate::ExecutionClient`]
//! shares, whichever side of the seam it sits on.
//!
//! The sentinel FILE (path precedence, armability probe, once-per-process report, the shared
//! `exists()`) lives in `crates/vike-bridge-core/src/halt.rs`. **Only the decision lives here**:
//! `vike-paper`'s `PaperExecutionClient` must refuse an opening order under a HALT file like any
//! venue adapter (operators rehearse the kill switch on the paper daemon) yet must not depend on
//! bridge-core's ureq/tungstenite/rustls stack, so the rule moved DOWN to the crate both share and
//! bridge-core re-exports it. vike-exec also declares the trait this predicate gates and owns its
//! in-process twin, [`crate::RiskGate`] under `TradingState::Halted`.
//!
//! ## ⚠ The two mechanisms agree on POLICY and differ in VERIFICATION STRENGTH
//!
//! [`crate::RiskGate`] admits only a POSITION-VERIFIED reduce (`vike_model::is_covered_reduce`: the
//! order must OPPOSE the position and be COVERED by it) because it holds the position book. The
//! `ExecutionClient` boundary does not (one exception is noted on [`halt_admits_submit`]), so there
//! the caller-asserted `reduce_only` flag is the only evidence in existence, and
//! [`halt_admits_submit`] takes it. The honest residual — a strategy bug that mis-tags its entries
//! `reduce_only` can open risk through a HALT file on a FLAT book, which nothing anywhere catches —
//! is argued in `crates/vike-bridge-core/src/halt.rs`'s module doc, next to the file that engages
//! it. Never "simplify" by deleting either layer.
//!
//! ⚠ The exception does NOT weaken that residual: an adapter's book is evidence FOR a reduce and
//! never against one. It is built forwards from what the process observed, so "no such position"
//! and "not learned yet" are the same state in it, and a gate refusing on that silence would refuse
//! an operator's exit after every restart. Verification can only ever ADD admits at this boundary —
//! which is why the exception is spelled `closes || …` and not `closes`.
//!
//! ## The halt-admit POLICY — and why `Verify` is weaker than the word suggests
//!
//! [`halt_admits_submit_under`] weighs a [`vike_model::HaltAdmit`] mode and the venue's own
//! [`PositionEvidence`]: `Admit` (the default, `vike_config::Policy::halt_admit`'s too) is the
//! flag-trusting rule above, and [`halt_admits_submit`] IS that call; `Verify` additionally
//! refuses a submit the venue's own book PROVES opens risk.
//!
//! ⚠ **`Verify` refuses only what it can prove, and THIS PARAGRAPH IS THE SINGLE AUTHORITY for
//! that** (`docs/ops/kill-switches.md`, `vike_config::Policy::halt_admit` and
//! `crates/vike-model/src/orders/halt_admit.rs` cite it rather than restating it). Every unknown
//! ADMITS: never fetched, fetch failed, a reconcile answer this build could not fully read, an
//! answer for another account, socket dropped since the fetch, poisoned lock, unresolvable symbol
//! — **and, decisively, a book holding no position in the symbol being judged at all**. The
//! promise being kept is *"halting cannot trap you"*, not *"halting is airtight"*: an operator who
//! restarts a process under a halt and finds a book not yet filled in must still be able to close.
//!
//! ⚠ **A refusal must rest on a POSITIVE report, never on an absence — and the difference is not
//! decidable at this crate.** [`PositionEvidence::Flat`] is always manufactured from an absence
//! somewhere (an opposing-exposure total of `0`), so it is a FACT only if the producing layer had
//! positive evidence about that instrument; the word "COMPLETE" on [`PositionEvidence::fetched`] is
//! that layer's whole obligation. `crates/bridges/ctrader/src/positions.rs`'s `unauthoritative_for`
//! is the one implementation, and its type doc carries the MEASURED trap a weaker rule let through:
//! a venue that omitted a position from an otherwise perfect reconcile answer, whose exit was then
//! refused under an engaged halt. Only requiring a positive report catches a row never sent.
//!
//! `vike_model::halt_verify_support` is the per-venue row saying where `Verify` is real (cTrader
//! alone today) and, everywhere else, why it degrades to `Admit`. That degrade is REPORTED at
//! MOUNT by `vike_mount::make_engine`, for the reason
//! `vike_bridge_core::halt::halt_path_arming_error` exists: a switch that is not armed looks
//! exactly like one that is, until it is needed.

use vike_model::{HaltAdmit, OrderRequest};

/// What the venue's own position book says about a submit — the third argument to
/// [`halt_admits_submit_under`], and a THREE-state answer on purpose.
///
/// ⚠ **`Option<f64>` is the obvious shape and it is the bug.** The first position-verified admit
/// on cTrader (#1180) looked the position up in a map that starts EMPTY, read the absent entry as
/// "flat" through `unwrap_or(0.0)`, and refused a genuine exit after every restart. "We have not
/// been told" and "we have been told there is nothing" are different facts and only one of them
/// may refuse; making that unrepresentable is this enum's entire job.
/// `crates/bridges/ctrader/src/positions.rs`'s `PositionBook` is the evidence-PRODUCING half, and
/// the one that actually decides which state you get.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PositionEvidence {
    /// No trustworthy answer, WITH the reason (logged when a submit is admitted on it). Never
    /// fetched, fetch failed, a reconcile answer that could not be read in full or was for another
    /// account, socket dropped since, poisoned lock, unresolvable symbol, **or no position reported
    /// in this symbol at all** — every one of them ADMITS. See the module doc.
    Unknown(&'static str),
    /// The venue POSITIVELY reported on this instrument, the answer was complete, and there is no
    /// exposure this submit could reduce. The one case a halt may refuse on evidence: the order
    /// OPENS. ⚠ An absence of any report is [`Self::Unknown`], never this.
    Flat,
    /// Fetched, and this much opposing exposure exists (venue-native units, magnitude ≥ 0 — the
    /// side lives in the question, not the number). The submit genuinely reduces something.
    Opposing(f64),
}

impl PositionEvidence {
    /// The evidence a venue whose halt boundary holds no position book supplies — a named
    /// constant so the venues that share the reason share the wording.
    pub const NO_BOOK: Self =
        PositionEvidence::Unknown("this adapter's halt boundary holds no position book");

    /// Build evidence from a FETCHED, COMPLETE book's opposing-exposure total. `> 0` ⇒
    /// [`Self::Opposing`]; `0` (or negative) ⇒ [`Self::Flat`]; **non-finite ⇒ [`Self::Unknown`]**,
    /// because a NaN is a broken computation and evidence that is not evidence must never refuse.
    ///
    /// ⚠ **The caller owes the word COMPLETE, and this function cannot check it.** A `0` over a
    /// book that silently dropped rows it could not decode, or was never told anything about this
    /// instrument, is an absence of information wearing the costume of a fact; the producing layer
    /// must answer [`Self::Unknown`] there and never reach here.
    /// `crates/bridges/ctrader/src/positions.rs`'s `unauthoritative_for` is the rule (provenance
    /// AND per-symbol coverage), and `crates/bridges/ctrader/src/conn.rs`'s
    /// `rebuild_position_book` sets the provenance half.
    ///
    /// Constructing through this rather than the variants makes `Opposing(0.0)` — "flat" reading
    /// as "there is something there" — impossible.
    pub fn fetched(opposing_qty: f64) -> Self {
        if !opposing_qty.is_finite() {
            return PositionEvidence::Unknown(
                "the opposing-exposure total was not a finite number",
            );
        }
        if opposing_qty > 0.0 {
            PositionEvidence::Opposing(opposing_qty)
        } else {
            PositionEvidence::Flat
        }
    }

    /// A short label for a log line — for `Unknown`, the reason itself.
    pub fn label(&self) -> &'static str {
        match self {
            PositionEvidence::Unknown(why) => why,
            PositionEvidence::Flat => "the venue reports no opposing position",
            PositionEvidence::Opposing(_) => "the venue reports opposing exposure",
        }
    }
}

/// The reason carried by the synthesized terminal `OrderRejected` when a submit is refused under
/// halt. Public so downstream (GUI/tests) can recognize a halt rejection by its exact wording.
/// Re-exported from `vike_bridge_core::halt` at its historical path.
pub const HALT_REJECT_REASON: &str = "halted: HALT file present";

/// Does an engaged HALT sentinel let this submit through? `true` ⇒ send it; `false` ⇒ refuse it and
/// synthesize the terminal [`HALT_REJECT_REASON`] rejection.
///
/// **The whole rule is `request.reduce_only`** — a halt stops orders that OPEN risk, and must never
/// trap an operator in a position. A cancel does not discharge that: getting out of a POSITION
/// requires SENDING an order (`OrderIntent::Flatten` mints a `reduce_only` MARKET leg for
/// `|position|`), so without this exemption `market-exit` under a HALT file would mass-cancel and
/// then have every flatten leg rejected, leaving the operator halted WITH the position open.
///
/// ⚠ **This is [`halt_admits_submit_under`] at [`HaltAdmit::Admit`] with no book, delegated so the
/// default cannot drift from it.** Every client whose boundary holds no position book calls it
/// (`vike_bridge_core::ExecActor`, hyperliquid's bespoke client,
/// `vike_paper::PaperExecutionClient`) and is unaffected by the halt-admit policy: `Verify` has
/// nothing to verify against there. One
/// shared function, because the original pair of clients had already drifted once (only
/// hyperliquid's blocked `submit_batch`).
///
/// ⚠ **ONE client EXTENDS this rather than replacing it, and the difference is load-bearing.**
/// `crates/bridges/ctrader/src/exec.rs`'s `halt_admits_this_submit` is `closes || <this>`: it also
/// admits what that venue's own POSITION BOOK proves is a close, because on a hedging account a
/// plain opposite-side order closes without carrying `reduce_only`. It extends because the book is
/// evidence FOR a close only: it holds nothing until the venue ANSWERS a best-effort reconcile, so
/// a restarted daemon can have heard nothing about a held position, and a verified-ONLY gate
/// shipped there first refused exactly those exits. For a future client: **hold only the request ⇒
/// call this; hold a position book ⇒ call this AND admit what the book proves on top — never
/// instead.**
///
/// ⚠ It takes the WHOLE request, not a bool, deliberately: `halt_admits_submit(reduce_only: bool)`
/// reads at the call site as though the caller had already decided the question. Taking
/// `&OrderRequest` keeps the decision here, beside its doc, and leaves room to tighten the
/// predicate (an order-type or size condition) without touching any client.
#[inline]
pub fn halt_admits_submit(request: &OrderRequest) -> bool {
    halt_admits_submit_under(request, HaltAdmit::Admit, PositionEvidence::NO_BOOK)
}

/// [`halt_admits_submit`] under an explicit halt-admit POLICY and the venue's own
/// [`PositionEvidence`] — the full rule, of which [`halt_admits_submit`] is the `Admit`/no-book
/// case.
///
/// **Under [`HaltAdmit::Admit`] — the default, and every deployment with no `policy.halt_admit`
/// row — the rule is `request.reduce_only` and `evidence` is IGNORED. Under [`HaltAdmit::Verify`]
/// it is additionally not [`PositionEvidence::Flat`]: strictly TIGHTER, never wider**
/// (`verify_is_a_subset_of_admit_over_every_input` pins that over the whole input space).
///
/// ⚠ **[`PositionEvidence::Opposing`] admits at ANY magnitude, deliberately — including one SMALLER
/// than the order.** `opposing >= requested` (mirroring `vike_model::is_covered_reduce`, which
/// [`crate::RiskGate`] uses) would be a trap here: a `reduce_only` order for more than is open is
/// CAPPED by the venue adapter (`crates/bridges/ctrader/src/positions.rs`'s `plan_reduce` closes
/// `min(requested, available)` and cannot open), so refusing it would deny a genuine partial exit
/// exactly when an operator needs it. The `f64` is carried for the admit log line, and so that
/// `Flat` is a distinct state instead of `Opposing(0.0)`.
///
/// ⚠ **There is deliberately no `Refuse` mode** — see [`vike_model::HaltAdmit`]: it disarms the
/// panic button in exactly the situations that halt on their own. `Verify` degrades TOWARD `Admit`
/// on every unknown, so it cannot be reached by accident either.
#[inline]
pub fn halt_admits_submit_under(
    request: &OrderRequest,
    mode: HaltAdmit,
    evidence: PositionEvidence,
) -> bool {
    if !request.reduce_only {
        // Opens or adds, by the caller's own assertion: refused under BOTH modes. A `Verify` that
        // admitted an unknown-book OPEN would be the fail-open rule aimed at the wrong half.
        return false;
    }
    match mode {
        HaltAdmit::Admit => true,
        HaltAdmit::Verify => !matches!(evidence, PositionEvidence::Flat),
    }
}

#[path = "halt_tests.rs"]
#[cfg(test)]
mod halt_tests;
