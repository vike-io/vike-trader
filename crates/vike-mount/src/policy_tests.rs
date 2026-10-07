use super::*;

/// `policy.halt_admit` row → `Policy` → `MountPolicy` → the mode `make_engine` puts in force,
/// landing DIFFERENTLY per venue (why it is carried, not assumed everywhere).
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
    // Everywhere else it degrades WITH a REASON: a silent degrade is the defect this removes.
    for venue in vike_model::VENUES.iter().filter(|v| **v != "ctrader") {
        let (effective, why) = vike_model::effective_halt_admit(mount.halt_admit, venue);
        assert_eq!(effective, HaltAdmit::Admit, "{venue}");
        assert!(why.is_some(), "{venue} degraded with nothing to tell the operator");
    }
}

/// `Default` IS the no-policy value: passing it and passing nothing are interchangeable.
#[test]
fn the_default_is_the_no_policy_answer() {
    assert_eq!(MountPolicy::default().market_slippage, None);
    assert_eq!(MountPolicy::default().halt_admit, HaltAdmit::Admit);
    assert_eq!(MountPolicy::default().venue_mode("bybit"), VenueMode::Paper);
    assert_eq!(MountPolicy::from(&Policy::default()), MountPolicy::default());
}

/// **The ceiling lands PER VENUE**: a `Policy` naming two venues projects onto a `MountPolicy`
/// answering each differently; every unnamed venue keeps the safe default.
///
/// ⚠ Built through `VenuePolicy::declare`, not a parsed `policy.venues` patch: `Policy::apply` is
/// `pub(crate)` to vike-config. The PATCH half (`PolicyPatch::venues` → `Policy::apply` →
/// `is_declared`; the loader folds the `policy.venues.*` rows into that patch) is gated by
/// `crates/vike-config/src/policy_tests.rs`'s `a_venues_table_sets_what_it_names_and_inherits_the_rest`
/// and `an_all_paper_table_declares_while_no_table_does_not`; this pins the PROJECTION.
#[test]
fn a_venues_row_reaches_the_projection_per_venue() {
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
    assert!(mount.venues.is_declared(), "the rows stated an arming; the projection carries it");
}

/// **THE RELATIONSHIP GATE, stated behaviourally rather than in a comment.**
///
/// `policy` and a run profile's `[risk]` both carry `max_notional_per_order` and judge different
/// acts: the profile's reaches `vike_exec::RiskLimits` (EVERY order the core admits); this one
/// guards three EDGE surfaces a human types at and reaches no `RiskLimits`. The difference is the
/// `max_notional_per_order: _` binding; its other records (that binding's `//`, a
/// `vike_config::ceilings::PRE_TRADE_CEILINGS` row) are prose, and folding the field in leaves
/// every gate over that table green.
///
/// So the drop is asserted where it is OBSERVABLE: two policies differing ONLY in that key
/// project onto EQUAL mounts (`MountPolicy` is `PartialEq` over every field), which stops
/// holding the moment any field carries the value.
///
/// ⚠ RED is the gate doing its job, not a test to relax: folding this ceiling into `RiskLimits`
/// MOVES `crate::require_live_risk_budget`'s pre-connect refusal (its text names the profile's
/// `[risk]`; the module doc calls that "a decision, not plumbing"). Decide, state it, and update
/// `PRE_TRADE_CEILINGS` (its `refuses_live_mount_when_absent` column is gated against that
/// refusal's source) in the same PR.
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

    // …and with every OTHER carried field distinctive, so the claim is not an artefact of two
    // defaults: a future field carrying the ceiling ALONGSIDE these is still caught.
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
    // The control: `loud` differs from `bare`, so the comparison above can fail for its reason.
    assert_ne!(loud(None), bare(None), "the `loud` fixture must not collapse to the default");
}
