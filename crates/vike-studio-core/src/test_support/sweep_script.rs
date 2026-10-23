//! The parameterized SMA-cross Rhai strategy the Studio sweep tests scan over `param("fast", ..)`.

/// `param("fast", ..)` is the axis a sweep grid overrides, so each grid point trades differently.
///
/// No `crate::` path: the `tests/` binaries include this file with
/// `#[path = "../src/test_support/sweep_script.rs"] mod sweep_script;`.
pub const SWEEP_SCRIPT: &str = r#"
const QTY = 1.0;
let fast = param("fast", 5.0);
fn on_bar() {
    let f = sma(fast.to_int()); let s = sma(20);
    if s.is_nan() { return; }
    let target = if f > s { QTY } else { -QTY };
    let delta = target - position();
    if abs(delta) > 1e-12 { market(if delta > 0.0 { 1 } else { -1 }, abs(delta)); }
}
"#;
