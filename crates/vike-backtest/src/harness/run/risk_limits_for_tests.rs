//! `risk_limits_for`: absent `[risk]` maps to `None`, present maps to the profile conversion.

use super::*;

const BASE_TOML: &str = r#"
[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "bar"
interval = "1d"
from = "0"
to = "100000"

[engine]
cash = 1000.0

[strategy]
name = "buy_hold"
"#;

#[test]
fn none_when_risk_section_absent() {
    let profile = BacktestProfile::from_toml_str(BASE_TOML).unwrap();
    assert!(profile.risk.is_none());
    assert!(
        risk_limits_for(&profile).is_none(),
        "absent `[risk]` must map to `None`, not an armed-but-empty gate"
    );
}

#[test]
fn some_matching_to_risk_limits_when_risk_section_present() {
    let toml =
        format!("{BASE_TOML}\n[risk]\nmax_notional_per_order = 5000.0\nmax_leverage = 3.0\n");
    let profile = BacktestProfile::from_toml_str(&toml).unwrap();
    let risk = profile.risk.as_ref().expect("`[risk]` configured");
    let got = risk_limits_for(&profile).expect("Some when `[risk]` is present");
    assert_eq!(got, risk.to_risk_limits(), "must use the SAME converter, not a re-derived one");
    assert_eq!(got.max_notional_per_order, Some(5000.0));
    assert_eq!(got.max_leverage, Some(3.0));
    assert_eq!(got.im_requirement, Some(1.0 / 3.0), "3x arms the buying-power check at 1/3");
}
