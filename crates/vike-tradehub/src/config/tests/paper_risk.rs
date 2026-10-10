//! `resolve_paper_risk_limits`: the run profile's risk-budget resolver.

use super::*;

// ---------------------------------------------------------------------------------------------
// resolve_paper_risk_limits — the RunProfile risk-budget resolver (Task 2).
// ---------------------------------------------------------------------------------------------

/// MERGE-SAFETY PROPERTY: absent a profile, the resolved limits must be byte-identical to
/// `RiskLimits::new()` — the value the daemon has hardcoded since before this wiring existed.
#[test]
fn no_profile_is_byte_identical_to_the_pre_profile_default() {
    let got = resolve_paper_risk_limits(None).expect("no profile is never an error");
    assert_eq!(got, RiskLimits::new(), "no profile must not change mounted behavior at all");
}

#[test]
fn profile_operator_budget_fields_are_armed() {
    let toml = r#"
mode = "paper"
[risk]
max_notional_per_order      = 100.0
max_total_exposure          = 500.0
max_orders_per_window       = 3
window_ms                   = 2000
max_leverage                = 4.0
required_free_bp_pct        = 0.1
block_reduce_only_overshoot = true
"#;
    let profile = RunProfile::from_toml_str(toml).expect("profile parses and validates");
    let got = resolve_paper_risk_limits(Some(&profile)).expect("NoGridFetched never errors");
    assert_eq!(got.max_notional_per_order, Some(100.0));
    assert_eq!(got.max_total_exposure, Some(500.0));
    assert_eq!(got.max_orders_per_window, Some(3));
    assert_eq!(got.window_ms, 2000);
    assert_eq!(got.max_leverage, Some(4.0));
    // DERIVED from `max_leverage` (issue #822): the TOML has no `im_requirement` key.
    assert_eq!(got.im_requirement, Some(0.25));
    assert_eq!(got.required_free_bp_pct, 0.1);
    assert!(got.block_reduce_only_overshoot);
    // No instrument fields were set in the profile -> stay None (NoGridFetched takes the
    // profile's own instrument fields verbatim; an unset field is None, not inherited from
    // anywhere else — there is no "anywhere else" on a paper mount).
    assert_eq!(got.tick_size, None);
    assert_eq!(got.lot_size, None);
    assert_eq!(got.min_qty, None);
    assert_eq!(got.min_notional, None);
}

#[test]
fn profile_may_also_supply_instrument_fields_under_no_grid_fetched() {
    // The PAPER mount's one legitimate use of `apply_to`'s NoGridFetched exception: a
    // `mode = "paper"` profile's own instrument-grid fields take effect with no opt-in of any
    // kind, since `RunProfile::grid_source` derives `NoGridFetched` from `mode = "paper"` and
    // nothing else ever fetches a grid for a paper mount.
    let toml = r#"
mode = "paper"
[risk]
tick_size    = 0.01
lot_size     = 1.0
min_qty      = 1.0
min_notional = 1.0
"#;
    let profile = RunProfile::from_toml_str(toml).expect("profile parses and validates");
    let got = resolve_paper_risk_limits(Some(&profile)).expect("NoGridFetched never errors");
    assert_eq!(got.tick_size, Some(0.01));
    assert_eq!(got.lot_size, Some(1.0));
    assert_eq!(got.min_qty, Some(1.0));
    assert_eq!(got.min_notional, Some(1.0));
}
