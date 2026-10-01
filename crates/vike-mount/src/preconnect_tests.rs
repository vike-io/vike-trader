/// The pre-connect PREVIEW equals the post-merge verdict for the two budget caps: a profile
/// supplying both passes the same `require_live_risk_budget` the refusal calls; one missing
/// either is refused naming it. (`to_risk_limits` is the preview the `make_engine` site
/// builds; the two caps are operator-owned on every merge path, so preview == final.)
#[test]
fn preview_budget_matches_the_gate_verdict() {
    let full = vike_exec::ProfileRisk {
        max_notional_per_order: Some(100.0),
        max_total_exposure: Some(500.0),
        ..Default::default()
    };
    assert!(super::require_live_risk_budget("binance", &full.to_risk_limits(), true).is_ok());
    let half = vike_exec::ProfileRisk { max_notional_per_order: Some(100.0), ..Default::default() };
    let err = super::require_live_risk_budget("binance", &half.to_risk_limits(), true)
        .expect_err("one missing cap must refuse");
    assert!(format!("{err}").contains("max_total_exposure"));
}
