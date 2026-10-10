/// The pre-connect PREVIEW (`to_risk_limits`, built at the `make_engine` site) equals the final
/// verdict for the two budget caps, operator-owned on every merge path: both present passes
/// `require_live_risk_budget`; one missing is refused naming it.
#[test]
fn preview_budget_matches_the_gate_verdict() {
    let full = vike_model::ProfileRisk {
        max_notional_per_order: Some(100.0),
        max_total_exposure: Some(500.0),
        ..Default::default()
    };
    assert!(super::require_live_risk_budget("binance", &full.to_risk_limits(), true).is_ok());
    let half =
        vike_model::ProfileRisk { max_notional_per_order: Some(100.0), ..Default::default() };
    let err = super::require_live_risk_budget("binance", &half.to_risk_limits(), true)
        .expect_err("one missing cap must refuse");
    assert!(format!("{err}").contains("max_total_exposure"));
}
