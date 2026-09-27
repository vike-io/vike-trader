//! What an engaged HALT sentinel lets OUT — the ONE predicate every [`crate::ExecutionClient`]
//! shares, whichever side of the seam it sits on.
//!
//! The sentinel FILE itself (its path precedence, the `VIKE_HALT_FILE` override, the
//! armability probe, the once-per-process report) is not here — it lives in
//! `crates/vike-bridge-core/src/halt.rs`, which is where the environment read and the `tracing`
//! report belong and where every venue adapter already looks. **Only the decision moved down**, and
//! only because the set of things that must agree on it grew past the crates that can see
//! vike-bridge-core.
//!
//! ## Why this is in vike-exec and not in the bridge crate that owns the file
//!
//! `vike-paper`'s `PaperExecutionClient` is an [`crate::ExecutionClient`] like any venue adapter,
//! and a mounted paper exchange must refuse an opening order under a HALT file for the same reason a
//! live one does — an operator rehearses the kill switch on the paper daemon, and a switch that
//! silently does nothing there teaches them it is armed when it is not. But `vike-paper` depends on
//! vike-model + vike-exec + vike-fills and **must not** grow a transport dependency:
//! `vike-bridge-core` owns the ureq/tungstenite/rustls stack, so reaching the predicate through it
//! would compile an HTTP/WS/TLS tree into the backtest's own fill primitive and onto every
//! `cargo deny --all-features` audit of it. That is the `vike-secrets` / `vike-alerting` argument
//! wearing the kill-switch hat: when two crates that cannot see each other must agree on one rule,
//! the rule moves DOWN to the crate they share, and the original home re-exports it so no call site
//! changes.
//!
//! vike-exec is that shared crate, and it is the right one on the merits rather than by elimination:
//! it declares the [`crate::ExecutionClient`] trait this predicate gates, and it owns the sentinel's
//! in-process twin — [`crate::RiskGate`] under `TradingState::Halted`, which answers the SAME
//! question against the real position book. Keeping both definitions of "what a stop lets out" in
//! one crate is the point; see the asymmetry note below.
//!
//! ## ⚠ The two mechanisms agree on POLICY and differ in VERIFICATION STRENGTH
//!
//! [`crate::RiskGate`] admits only a POSITION-VERIFIED reduce (`vike_model::is_covered_reduce`: the
//! order must OPPOSE the position and be COVERED by it) because it holds the position book. The
//! `ExecutionClient` boundary does not — an adapter owns a command channel and a sentinel path and,
//! with one exception noted on [`halt_admits_submit`], nothing that could say how big any position
//! is — so at that boundary the caller-asserted `reduce_only` flag is the only evidence in
//! existence, and [`halt_admits_submit`] takes it. The full argument (and the honest residual: a
//! strategy bug that mis-tags its entries `reduce_only` can open risk through a HALT file on a FLAT
//! book, which nothing anywhere catches) is in `crates/vike-bridge-core/src/halt.rs`'s module doc,
//! next to the file that engages it.
//!
//! ⚠ The exception does NOT weaken that residual, and the reason is worth carrying: an adapter's
//! book is evidence FOR a reduce and never against one. It is built forwards from what the process
//! observed, so "no such position" and "not learned yet" are the same state in it, and a gate that
//! refused on that silence would refuse an operator's exit after every restart. Verification can
//! only ever ADD admits at this boundary — which is why the exception is spelled `closes || …` and
//! not `closes`.
//!
//! ## The halt-admit POLICY — and why `Verify` is weaker than the word suggests
//!
//! [`halt_admits_submit_under`] takes a [`vike_model::HaltAdmit`] mode and the venue's own
//! [`PositionEvidence`]. [`vike_model::HaltAdmit::Admit`] — the default, including
//! `vike_config::Policy::halt_admit`'s — is the flag-trusting rule above, byte-identical, evidence
//! IGNORED; [`halt_admits_submit`] IS that call, so the two cannot drift.
//! [`vike_model::HaltAdmit::Verify`] is strictly TIGHTER: it still requires `reduce_only`, and
//! additionally refuses a submit the venue's own book PROVES opens risk.
//!
//! ⚠ **`Verify` refuses only what it can prove, and THIS PARAGRAPH IS THE SINGLE AUTHORITY for
//! that** (`docs/ops/kill-switches.md`, `vike_config::Policy::halt_admit` and
//! `crates/vike-model/src/halt_admit.rs` cite it rather than restating it, so the sentence cannot
//! rot in four places). Every unknown ADMITS: never fetched, fetch failed, a reconcile answer this
//! build could not fully read, an answer for another account, socket dropped since the fetch,
//! poisoned lock, unresolvable symbol — **and, decisively, a book holding no position in the
//! symbol being judged at all**. The promise being kept is *"halting cannot trap you"*, not
//! *"halting is airtight"* — an operator who restarts a process under a halt and finds a book that
//! has not been filled in yet must still be able to close. [`PositionEvidence`] is a THREE-state
//! enum for exactly that reason: an `Option<f64>` with an `unwrap_or(0.0)` at the comparison site is
//! the same bug wearing a type.
//!
//! ⚠ **A refusal must rest on a POSITIVE report, never on an absence — and the difference is not
//! decidable at this crate.** [`PositionEvidence::Flat`] is always manufactured from an absence
//! somewhere (an opposing-exposure total of `0`), so it is only a FACT if the layer that produced it
//! had positive evidence about that instrument. It is the producing layer that owes this, and the
//! word "COMPLETE" on [`PositionEvidence::fetched`] is the whole obligation:
//! `crates/bridges/ctrader/src/positions.rs`'s `unauthoritative_for` is the one
//! implementation, and its type doc carries the MEASURED trap that a weaker rule let through — a
//! venue that simply omitted a position from an otherwise perfect reconcile answer, whose exit was
//! then refused under an engaged halt. No count of undecodable rows can catch a row that was never
//! sent; only requiring a positive report can.
//!
//! `vike_model::halt_verify_support` is the per-venue row saying where `Verify` is real (cTrader
//! alone today — the only adapter holding a position book at a halt boundary) and, everywhere else,
//! the reason it degrades to `Admit`. That degrade is REPORTED at MOUNT by `vike_mount::make_engine`
//! rather than at the first halted submit, for the same reason
//! `vike_bridge_core::halt::halt_path_arming_error` exists: a switch that is not armed looks exactly
//! like one that is, right up until it is needed.

use vike_model::{HaltAdmit, OrderRequest};

/// What the venue's own position book says about a submit — the third argument to
/// [`halt_admits_submit_under`], and a THREE-state answer on purpose.
///
/// ⚠ **`Option<f64>` was the obvious shape and it is the bug.** The first position-verified admit
/// on cTrader (#1180) looked the position up in a map that starts EMPTY and so refused a genuine
/// exit after every restart — an absent entry read as "flat" because `unwrap_or(0.0)` says so. "We
/// have not been told" and "we have been told there is nothing" are different facts and only one of
/// them may refuse. Making that unrepresentable is this enum's entire job;
/// `crates/bridges/ctrader/src/positions.rs`'s `PositionBook` is the matching change on the
/// evidence-PRODUCING side, and it is the half that actually decides which state you get.
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
    /// The evidence a venue whose halt boundary holds no position book supplies. A named constant
    /// rather than a bare `Unknown("…")` at each call site, so the venues that share the reason
    /// share the wording too.
    pub const NO_BOOK: Self =
        PositionEvidence::Unknown("this adapter's halt boundary holds no position book");

    /// Build evidence from a FETCHED, COMPLETE book's opposing-exposure total. `> 0` ⇒
    /// [`Self::Opposing`]; `0` (or negative) ⇒ [`Self::Flat`]; **non-finite ⇒ [`Self::Unknown`]**,
    /// because a NaN is a broken computation and evidence that is not evidence must never refuse.
    ///
    /// ⚠ **The caller owes the word COMPLETE, and this function cannot check it.** A total of `0`
    /// over a book that silently dropped rows it could not decode — or that was never told anything
    /// about this instrument in the first place — is an absence of information wearing the costume
    /// of a fact. The producing layer must answer [`Self::Unknown`] in both cases and never reach
    /// here: `crates/bridges/ctrader/src/positions.rs`'s `unauthoritative_for` is the
    /// rule (provenance AND per-symbol coverage), and `crates/bridges/ctrader/src/conn.rs`'s
    /// `rebuild_position_book` is what sets the provenance half.
    ///
    /// Callers construct through this rather than the variants so `Opposing(0.0)` — a value that
    /// means "flat" while reading as "there is something there" — cannot exist.
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
///
/// Re-exported from `vike_bridge_core::halt` at its historical path, so the ~14 adapters and tests
/// that name it there are unchanged.
pub const HALT_REJECT_REASON: &str = "halted: HALT file present";

/// Does an engaged HALT sentinel let this submit through? `true` ⇒ send it; `false` ⇒ refuse it and
/// synthesize the terminal [`HALT_REJECT_REASON`] rejection.
///
/// **The whole rule is `request.reduce_only`** — a halt stops orders that OPEN risk, and must never
/// trap an operator in a position. A cancel does not discharge that: a cancel removes a resting
/// ORDER, whereas getting out of a POSITION requires SENDING one, and `OrderIntent::Flatten` mints a
/// `reduce_only` MARKET leg for `|position|`. Until this exemption existed, `market-exit` under a
/// HALT file ran its mass-cancel and then had every flatten leg rejected, leaving the operator
/// halted WITH the position still open.
///
/// ⚠ **This is [`halt_admits_submit_under`] at [`HaltAdmit::Admit`] with no book, spelled as its own
/// function rather than restated.** Every client at a boundary that holds no position book calls it
/// and is therefore unaffected by the halt-admit policy — which is the honest answer for them, since
/// `Verify` has nothing to verify against there (`vike_model::halt_verify_support` carries the
/// per-venue reason and `vike_mount::make_engine` reports the degrade at mount). It delegates rather
/// than duplicating `request.reduce_only` so the byte-identical claim for `Admit` is STRUCTURAL: the
/// default cannot diverge from this function, because it IS this function.
///
/// It exists as a named, shared function rather than an `if` in each client because several
/// independent implementations enforce this sentinel — `vike_bridge_core::ExecActor` (every venue on
/// the shared actor), hyperliquid's bespoke client, and `vike_paper::PaperExecutionClient` — and the
/// pre-existing pair had already drifted in exactly this way once: both blocked `submit`, but only
/// hyperliquid's blocked `submit_batch` explicitly. A second copy of "what a halt lets out" is a
/// second chance to disagree about it.
///
/// ⚠ **ONE client EXTENDS this rather than replacing it, and the difference is load-bearing.**
/// `crates/bridges/ctrader/src/exec.rs`'s `halt_admits_this_submit` is `closes || <this>`: it also
/// admits what that venue's own POSITION BOOK proves is a close, because cTrader routes reduces by
/// inspecting positions — on a hedging account a plain opposite-side order closes without carrying
/// `reduce_only`, and refusing it under a halt would trap the operator.
///
/// It EXTENDS rather than replaces because a position book answers in one direction only. It is
/// evidence FOR a close; a position it does not hold is silence, not a denial — cTrader's book holds
/// nothing until the venue ANSWERS a reconcile for it, and that answer is best-effort at connect and
/// again at every reconnect, so a restarted daemon can have heard nothing about a position the
/// account already holds. A verified-ONLY gate shipped there first and refused exactly
/// those exits, which is the trap this exemption exists to prevent. The dividing line for a future
/// client: **hold only the request ⇒ call this; hold a position book ⇒ call this AND admit what the
/// book proves on top — never instead.**
///
/// ⚠ It takes the WHOLE request, not a bool, deliberately. The obvious signature
/// (`halt_admits_submit(reduce_only: bool)`) reads at the call site as though the caller had already
/// decided the question, and callers pass what is convenient. Taking `&OrderRequest` keeps the
/// decision here, where the doc explaining it also lives, and leaves room to tighten the predicate
/// (an order-type or size condition) without touching any client.
#[inline]
pub fn halt_admits_submit(request: &OrderRequest) -> bool {
    halt_admits_submit_under(request, HaltAdmit::Admit, PositionEvidence::NO_BOOK)
}

/// [`halt_admits_submit`] under an explicit halt-admit POLICY and the venue's own
/// [`PositionEvidence`] — the full rule, of which [`halt_admits_submit`] is the `Admit`/no-book
/// case.
///
/// **Under [`HaltAdmit::Admit`] — the default, and every deployment with no `policy.toml` — the
/// whole rule is `request.reduce_only`, and `evidence` is IGNORED.** That is byte-identical to the
/// behaviour that shipped before this policy existed, on every venue.
///
/// **Under [`HaltAdmit::Verify`] the rule is strictly TIGHTER, never wider**: still
/// `request.reduce_only`, and additionally not [`PositionEvidence::Flat`]. So `Verify` can only ever
/// refuse a subset of what `Admit` admits — it is impossible to configure this predicate into
/// letting MORE out, which `verify_is_a_subset_of_admit_over_every_input` pins over the whole input
/// space.
///
/// ⚠ **[`PositionEvidence::Opposing`] admits at ANY magnitude, deliberately — including one SMALLER
/// than the order.** The tempting rule is `opposing >= requested` (mirroring
/// `vike_model::is_covered_reduce`, which [`crate::RiskGate`] uses), and here it would be a trap: a
/// `reduce_only` order for more than is open is CAPPED by the venue adapter
/// (`crates/bridges/ctrader/src/positions.rs`'s `plan_reduce` closes `min(requested, available)` and
/// cannot open), so refusing it would deny a genuine partial exit — over-sized by a strategy bug, a
/// stale position size, or plain rounding — in exactly the moment an operator needs it. `Verify`
/// refuses only what it can PROVE opens: a FETCHED, COMPLETE, genuinely FLAT book. The `f64` is
/// carried rather than dropped because it is what the admit log line reports, and because keeping it
/// is what makes `Flat` a distinct state instead of `Opposing(0.0)`.
///
/// ⚠ **There is deliberately no `Refuse` mode** — see [`vike_model::HaltAdmit`]. It was the
/// behaviour until 2026-08-06 and it disarms the panic button in exactly the situations that halt on
/// their own. `Verify` degrades TOWARD `Admit` on every unknown, so it cannot be reached by accident
/// either.
#[inline]
pub fn halt_admits_submit_under(
    request: &OrderRequest,
    mode: HaltAdmit,
    evidence: PositionEvidence,
) -> bool {
    if !request.reduce_only {
        // Opens or adds, by the caller's own assertion. Refused under BOTH modes — no evidence can
        // rescue it, and a `Verify` that admitted an unknown-book OPEN would be the fail-open rule
        // pointed at the wrong half of the problem.
        return false;
    }
    match mode {
        HaltAdmit::Admit => true,
        HaltAdmit::Verify => !matches!(evidence, PositionEvidence::Flat),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(reduce_only: bool) -> OrderRequest {
        OrderRequest {
            client_order_id: "c1".into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            order_type: "market".into(),
            reduce_only,
            ..Default::default()
        }
    }

    /// Every evidence value, so no test below can silently miss one.
    fn all_evidence() -> Vec<PositionEvidence> {
        vec![
            PositionEvidence::NO_BOOK,
            PositionEvidence::Unknown("never fetched"),
            PositionEvidence::Flat,
            PositionEvidence::Opposing(0.5),
            PositionEvidence::Opposing(1000.0),
            PositionEvidence::Opposing(f64::MAX),
        ]
    }

    /// The whole rule, both directions. An opening order is refused; a reducing one is admitted so
    /// a halt can never trap an operator in a position.
    #[test]
    fn a_halt_refuses_opening_orders_and_admits_reducing_ones() {
        assert!(!halt_admits_submit(&req(false)), "an OPENING submit must be refused under halt");
        assert!(
            halt_admits_submit(&req(true)),
            "a reduce_only submit must pass, or `market-exit` cannot flatten from under a HALT file"
        );
    }

    /// **THE byte-identical proof.** Under the DEFAULT mode the predicate is exactly
    /// `request.reduce_only` — for every evidence value, including ones a venue with a real book
    /// would produce — and the no-policy entry point agrees with it everywhere. Weaken this and
    /// every "no behaviour change on the other venues" claim in the halt-admit PR is false.
    #[test]
    fn the_default_mode_is_exactly_reduce_only_whatever_the_evidence_says() {
        for evidence in all_evidence() {
            for reduce_only in [true, false] {
                assert_eq!(
                    halt_admits_submit_under(&req(reduce_only), HaltAdmit::Admit, evidence),
                    reduce_only,
                    "Admit must ignore evidence entirely: {evidence:?}"
                );
                assert_eq!(
                    halt_admits_submit(&req(reduce_only)),
                    halt_admits_submit_under(&req(reduce_only), HaltAdmit::Admit, evidence),
                    "the no-policy entry point must BE the Admit case, not a second copy of it"
                );
            }
        }
    }

    /// **THE restart case**, and the trap this whole design exists to avoid: a book that has never
    /// been fetched must NOT read as "flat". Halt engaged, process just restarted, operator sends
    /// the closing order — it is ADMITTED.
    #[test]
    fn verify_admits_a_closing_order_against_a_never_fetched_book() {
        assert!(
            halt_admits_submit_under(
                &req(true),
                HaltAdmit::Verify,
                PositionEvidence::Unknown("never fetched")
            ),
            "an empty-because-unfetched book refused the exit — halting now traps you, which is \
             the one thing docs/ops/kill-switches.md promises it cannot do"
        );
        // …and every other flavour of "no answer" behaves the same way. ⚠ The LAST one is the
        // decisive addition: a book that was fetched perfectly and simply holds no row for this
        // symbol is an ABSENCE, and a venue that omits a position it holds produces exactly that
        // shape — see `crates/bridges/ctrader/src/positions.rs`'s `PositionBook` type doc for the
        // measured refusal it caused.
        for why in [
            "fetch failed",
            "socket dropped since the fetch",
            "poisoned lock",
            "the reconcile answer could not be read in full",
            "the reconcile answer was for another account",
            "the venue has reported no position in this symbol at all",
        ] {
            assert!(halt_admits_submit_under(
                &req(true),
                HaltAdmit::Verify,
                PositionEvidence::Unknown(why)
            ));
        }
        assert!(halt_admits_submit_under(&req(true), HaltAdmit::Verify, PositionEvidence::NO_BOOK));
    }

    /// The inverse, so fail-open on CLOSES does not quietly become fail-open on OPENS: with a
    /// FETCHED, complete, genuinely flat book, a `reduce_only` order is opening risk and is REFUSED.
    #[test]
    fn verify_refuses_a_reduce_only_order_against_a_fetched_flat_book() {
        assert!(
            !halt_admits_submit_under(&req(true), HaltAdmit::Verify, PositionEvidence::Flat),
            "a reduce_only tag on a flat book is an OPEN — this is the case Verify exists for"
        );
    }

    /// Opposing exposure admits at ANY magnitude, including SMALLER than the order. An
    /// `opposing >= requested` rule would deny a genuine partial exit; the adapter caps the excess
    /// and cannot open with it.
    #[test]
    fn verify_admits_against_any_opposing_exposure_even_a_partial_one() {
        for opposing in [0.000_1, 1.0, 999.0, 1000.0, 1e9] {
            assert!(
                halt_admits_submit_under(
                    &req(true),
                    HaltAdmit::Verify,
                    PositionEvidence::Opposing(opposing)
                ),
                "opposing={opposing} against a 1-unit exit must be admitted — it reduces"
            );
        }
    }

    /// `Verify` can only ever REFUSE MORE than `Admit`, never admit more. Stated as a property over
    /// the whole input space, because "the knob cannot widen a ceiling" is the kind of claim that is
    /// easy to assert and easy to break with one inverted match arm.
    #[test]
    fn verify_is_a_subset_of_admit_over_every_input() {
        for evidence in all_evidence() {
            for reduce_only in [true, false] {
                let r = req(reduce_only);
                let admit = halt_admits_submit_under(&r, HaltAdmit::Admit, evidence);
                let verify = halt_admits_submit_under(&r, HaltAdmit::Verify, evidence);
                assert!(
                    admit || !verify,
                    "Verify admitted something Admit refuses ({reduce_only}, {evidence:?}) — the \
                     policy widened the sentinel instead of tightening it"
                );
            }
        }
    }

    /// An OPENING order is refused under BOTH modes and every evidence value. Unknown evidence
    /// fails OPEN for closes; it must not fail open for opens.
    #[test]
    fn an_opening_order_is_refused_under_every_mode_and_every_evidence() {
        for mode in [HaltAdmit::Admit, HaltAdmit::Verify] {
            for evidence in all_evidence() {
                assert!(
                    !halt_admits_submit_under(&req(false), mode, evidence),
                    "an order that does not even claim to reduce got through: {mode:?}/{evidence:?}"
                );
            }
        }
    }

    /// The constructor is what makes `Opposing(0.0)` — a value meaning "flat" that reads as "there
    /// is something there" — unrepresentable, and it treats a broken number as NO evidence rather
    /// than as flat.
    #[test]
    fn fetched_evidence_never_yields_a_zero_opposing_and_never_refuses_on_a_nan() {
        assert_eq!(PositionEvidence::fetched(0.0), PositionEvidence::Flat);
        assert_eq!(PositionEvidence::fetched(-5.0), PositionEvidence::Flat);
        assert_eq!(PositionEvidence::fetched(2.5), PositionEvidence::Opposing(2.5));
        for broken in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let ev = PositionEvidence::fetched(broken);
            assert!(matches!(ev, PositionEvidence::Unknown(_)), "{broken} produced {ev:?}");
            assert!(
                halt_admits_submit_under(&req(true), HaltAdmit::Verify, ev),
                "a broken computation must never be the thing that refuses an exit"
            );
        }
    }
}
