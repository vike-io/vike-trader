use super::*;
use std::collections::HashMap;
use vike_hyperliquid::exec::market_slippage_for;

/// THE load-bearing property of this whole phase, end to end and network-free: **no
/// `policy.toml` on disk ⇒ every venue gets exactly today's value.** Driven through the REAL
/// loader (`load(None, &{})` is precisely "no home directory, no project file, no
/// environment") rather than by asserting `Policy::default()`, so a future default that stopped
/// being the no-file answer would fail here too.
#[test]
fn an_absent_policy_file_is_exactly_the_mount_default() {
    let settings = vike_config::load(None, &HashMap::new()).unwrap();
    let mount = MountPolicy::from(&settings.policy);
    assert_eq!(mount, MountPolicy::default());
    assert_eq!(mount.market_slippage, None, "no file ⇒ no band ⇒ the venue's own literal");
    // …and the ACCOUNT-aggregate ceiling is OFF, so `RiskGate` runs no `over-account-exposure`
    // comparison and every existing deployment's verdicts are byte-identical. This is the
    // no-change claim for the one axis this PR adds, asserted through the REAL loader.
    assert_eq!(
        mount.max_account_exposure, None,
        "no file ⇒ no account ceiling ⇒ the pre-trade gate is exactly what it was"
    );
    // …and the SIZING-EQUITY ceiling is OFF, so `ExecutionEngine::sizing_equity` is bit-
    // identical to `resolved_equity` and every strategy sizes against exactly the number it
    // sized against before this axis existed. The no-change claim for the second axis this PR
    // adds, asserted through the REAL loader rather than about `Policy::default()`.
    assert_eq!(
        mount.max_sizing_equity, None,
        "no file ⇒ no equity ceiling ⇒ sizing and admission see exactly what they saw"
    );
    // …and the halt sentinel keeps trusting the caller's `reduce_only` flag, on every venue,
    // exactly as it did before this field existed. This is the byte-identical claim for the
    // kill switch, asserted through the REAL loader rather than about `Policy::default()`.
    assert_eq!(mount.halt_admit, HaltAdmit::Admit, "no file ⇒ the flag-trusting halt rule");
    for venue in vike_model::VENUES {
        assert_eq!(
            vike_model::effective_halt_admit(mount.halt_admit, venue),
            (HaltAdmit::Admit, None),
            "{venue} must be in `admit` with NOTHING to report — a default that logged would \
                 make the no-change claim false in the trace file too"
        );
    }
    // …and that `None` resolves, at the venue, to the exact literal hyperliquid priced with
    // before any of this existed. This is the byte-identical claim, asserted rather than
    // asserted-about.
    assert_eq!(market_slippage_for(mount.market_slippage), 0.05);
    // ⚠ THE ONE FIELD FOR WHICH "no file" IS NOT "no change": every venue reads `paper`, so the
    // mount refuses every arming credential presence would have produced. Asserted here, in the
    // test that owns the no-file claim, so the exception is stated where the rule is.
    for venue in vike_model::VENUES {
        assert_eq!(
            mount.venue_mode(venue),
            vike_config::VenueMode::Paper,
            "{venue}: an unstated arming ceiling must be the SAFE end, not the wide one"
        );
    }
    // …and nobody stated it, which is the fact the migration warning self-silences on.
    assert!(!mount.venues.is_declared(), "no file ⇒ nobody declared an arming");
}

/// The other half: a value in the file DOES reach the venue's resolver, verbatim. Pure — the
/// file→`Policy` half is gated by `vike-config`'s own `tests/load.rs`; this pins the projection
/// and the venue hand-off, which is the part that was missing.
#[test]
fn a_configured_band_reaches_the_venue_resolver_verbatim() {
    let policy = Policy { market_slippage: Some(0.002), ..Policy::default() };
    assert_eq!(MountPolicy::from(&policy).market_slippage, Some(0.002));
    assert_eq!(market_slippage_for(Some(0.002)), 0.002);
}

/// Threading a policy through the mount can only ever TIGHTEN the band, never widen it — the
/// bound lives at the venue (`vike_bridge_core::market_slippage`), so this edge cannot be used
/// to route around it. Pinned here as well as there because THIS is the path an operator's file
/// now travels.
#[test]
fn no_policy_value_can_widen_the_venue_default() {
    for wide in [0.0500001, 0.1, 0.5, 50.0, f64::INFINITY, f64::NAN] {
        let policy = Policy { market_slippage: Some(wide), ..Policy::default() };
        let applied = market_slippage_for(MountPolicy::from(&policy).market_slippage);
        assert!(applied <= 0.05, "{wide} widened the applied band to {applied}");
        assert!(applied.is_finite(), "{wide} produced a non-finite band");
    }
}

/// The halt-admit knob travels the whole way — `policy.toml` value → `Policy` → `MountPolicy`
/// → the mode `make_engine` puts in force — and lands DIFFERENTLY per venue, which is the point
/// of carrying it rather than assuming it applies everywhere.
#[test]
fn a_configured_verify_reaches_the_one_venue_that_can_honour_it_and_degrades_elsewhere() {
    let policy = Policy { halt_admit: HaltAdmit::Verify, ..Policy::default() };
    let mount = MountPolicy::from(&policy);
    assert_eq!(mount.halt_admit, HaltAdmit::Verify, "the projection must carry it verbatim");

    // cTrader honours it: the only adapter with a position book at its halt boundary.
    assert_eq!(
        vike_model::effective_halt_admit(mount.halt_admit, "ctrader"),
        (HaltAdmit::Verify, None)
    );
    // Everywhere else it degrades — and, load-bearing, with a REASON to report. A silent
    // degrade is the defect this wiring exists to remove, not the feature.
    for venue in vike_model::VENUES.iter().filter(|v| **v != "ctrader") {
        let (effective, why) = vike_model::effective_halt_admit(mount.halt_admit, venue);
        assert_eq!(effective, HaltAdmit::Admit, "{venue}");
        assert!(why.is_some(), "{venue} degraded with nothing to tell the operator");
    }
}

/// `Default` is not merely *similar* to an absent file — it is the same value, so a call site
/// that has no policy to pass and one that passes the default are interchangeable.
#[test]
fn the_default_is_the_no_policy_answer() {
    assert_eq!(MountPolicy::default().market_slippage, None);
    assert_eq!(MountPolicy::default().halt_admit, HaltAdmit::Admit);
    assert_eq!(MountPolicy::default().venue_mode("bybit"), VenueMode::Paper);
    assert_eq!(MountPolicy::from(&Policy::default()), MountPolicy::default());
}

/// **The ceiling travels the whole way and lands PER VENUE** — a `Policy` naming two venues
/// projects onto a `MountPolicy` that answers differently for each, while every venue the file
/// did not name keeps the safe default.
///
/// ⚠ Built through `VenuePolicy::declare` rather than by parsing a `[venues]` table, because
/// `Policy::apply` is `pub(crate)` to vike-config and this crate cannot reach it. The FILE half
/// — `[venues]` → `Policy` → `is_declared` — is gated where it lives, by
/// `crates/vike-config/src/policy_tests.rs`'s `a_venues_table_sets_what_it_names_and_inherits_the_rest`
/// and `an_all_paper_table_declares_while_no_table_does_not`; what this pins is the half those
/// cannot see, the PROJECTION.
#[test]
fn a_venues_table_reaches_the_projection_per_venue() {
    let policy = Policy {
        venues: vike_config::VenuePolicy::default()
            .declare("bybit", VenueMode::Live)
            .declare("binance", VenueMode::Demo),
        ..Policy::default()
    };
    let mount = MountPolicy::from(&policy);
    assert_eq!(mount.venue_mode("bybit"), VenueMode::Live);
    assert_eq!(mount.venue_mode("binance"), VenueMode::Demo);
    assert_eq!(mount.venue_mode("okx"), VenueMode::Paper, "an unnamed venue keeps the default");
    assert_eq!(mount.venue_mode("not-a-venue"), VenueMode::Paper, "and so does a non-venue");
    assert!(mount.venues.is_declared(), "the file stated an arming; the projection carries it");
}

/// **THE RELATIONSHIP GATE, stated behaviourally rather than in a comment.**
///
/// `settings/policy.toml` and a run profile's `[risk]` table both carry a
/// `max_notional_per_order`, and they judge different acts: the profile's reaches
/// `vike_exec::RiskLimits` and is evaluated on EVERY order the core admits, while this one
/// guards three EDGE surfaces a human types at and reaches no `RiskLimits` at all. The whole
/// difference is the `max_notional_per_order: _` binding a few lines up — and until this test
/// existed, the only record of it was the `//` comment beside that binding plus a
/// `vike_config::ceilings::PRE_TRADE_CEILINGS` row. Both are prose. Fold the field in and every
/// gate over that table stays green, because the table did not move.
///
/// So this asserts the drop where it is OBSERVABLE: two policies differing ONLY in that key
/// must project onto EQUAL mounts. Carrying the value means having somewhere to carry it, and
/// the moment that field exists these two stop being equal. No needle, no spelling, no comment
/// — `MountPolicy` is `PartialEq` over every field it has, so "the value is nowhere on the
/// mount's side of the seam" is exactly what is being compared.
///
/// ⚠ If you are reading this because it went RED: that is the gate doing its job, not a test to
/// relax. Folding this ceiling into `RiskLimits` MOVES `crate::require_live_risk_budget`'s
/// pre-connect refusal, whose operator-facing text names the profile's `[risk]` table — this
/// module's doc calls that "a decision, not plumbing". Make the decision, state it, and update
/// `PRE_TRADE_CEILINGS` (whose `refuses_live_mount_when_absent` column is gated against that
/// refusal's own source) in the same PR.
#[test]
fn a_policy_per_order_ceiling_reaches_nothing_the_mount_carries() {
    let bare = |n| MountPolicy::from(&Policy { max_notional_per_order: n, ..Policy::default() });
    assert_eq!(
        bare(Some(1.0)),
        bare(Some(9_999_999.0)),
        "two policies differing ONLY in `max_notional_per_order` projected onto DIFFERENT \
             mounts — the value is now observable here, which means it is being carried"
    );
    assert_eq!(
        bare(Some(1.0)),
        MountPolicy::default(),
        "a policy whose only line is `max_notional_per_order` must project onto exactly the \
             no-file mount: this key arms nothing the venue mount applies"
    );

    // …and again with every OTHER carried field set to a distinctive value, so the claim is
    // not an artefact of comparing two defaults. A future field that carried the ceiling
    // ALONGSIDE these would still be caught here.
    let loud = |n| {
        MountPolicy::from(&Policy {
            max_notional_per_order: n,
            max_account_exposure: Some(12_345.0),
            max_sizing_equity: Some(6_789.0),
            market_slippage: Some(0.002),
            halt_admit: HaltAdmit::Verify,
            venues: VenuePolicy::default().declare("bybit", VenueMode::Live),
            ..Policy::default()
        })
    };
    assert_eq!(
        loud(None),
        loud(Some(42.0)),
        "with every carried ceiling armed, adding `max_notional_per_order` still must change \
             nothing the mount sees"
    );
    // The control: `loud` really does differ from `bare`, so the comparison above is comparing
    // something. An assertion that cannot fail for its stated reason is not evidence.
    assert_ne!(loud(None), bare(None), "the `loud` fixture must not collapse to the default");
}
