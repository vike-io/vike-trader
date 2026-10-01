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
