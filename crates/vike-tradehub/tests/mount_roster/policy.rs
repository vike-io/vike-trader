//! The tests of `vike-mount`'s policy projection that follow a value across into the hyperliquid
//! bridge, where `vike_hyperliquid::exec::market_slippage_for` resolves the band. They lived in
//! `crates/vike-mount/src/policy_tests.rs` until the venue mount contract finished
//! (docs/decisions/0096): `vike-mount` names no bridge since, so a test that reaches one runs here.

use std::collections::HashMap;

use vike_config::Policy;
use vike_hyperliquid::exec::market_slippage_for;
use vike_model::HaltAdmit;
use vike_mount::MountPolicy;

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
