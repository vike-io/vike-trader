//! The SMA(5/20)-cross Rhai strategy the Studio Run tests drive over the random walk.

/// It TRADES over `random_walk_bars` (the sibling `walk.rs`), so a run over it carries a non-empty
/// trades vector and an equity curve with real return dispersion.
///
/// No `crate::` path: the `tests/` binaries include this file with
/// `#[path = "../src/test_support/cross_script.rs"] mod cross_script;`.
pub const CROSS_SCRIPT: &str = r#"
const QTY = 1.0;
fn on_bar() {
    let f = sma(5); let s = sma(20);
    if s.is_nan() { return; }
    let target = if f > s { QTY } else { -QTY };
    let delta = target - position();
    if abs(delta) > 1e-12 { market(if delta > 0.0 { 1 } else { -1 }, abs(delta)); }
}
"#;
