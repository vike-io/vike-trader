//! The parameterized SMA-cross Rhai strategy the Studio sweep tests scan over `param("fast", ..)`.

/// `param("fast", ..)` is the axis a sweep grid overrides, so each grid point trades differently.
pub const SWEEP_SCRIPT: &str = r#"
const QTY = 1.0;
let fast = param("fast", 5.0);
fn on_bar(strategy) {
    let f = sma(fast.to_int()); let s = sma(20);
    if s.is_nan() { return; }
    // an entry the other way also closes the position it meets
    if f > s {
        strategy.entry("Long", OrderSide::Long).size(QTY);
    } else {
        strategy.entry("Short", OrderSide::Short).size(QTY);
    }
}
"#;
