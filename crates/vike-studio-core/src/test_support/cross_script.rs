//! The SMA(5/20)-cross Rhai strategies the Studio Run tests drive over the random walk.

/// It TRADES over `random_walk_bars` (the sibling `walk.rs`) on a margin series, so a run over it carries a
/// non-empty trades vector and an equity curve with real return dispersion. ⚠ It REVERSES (an entry the other
/// way is one sell of twice the size), so on a spot pair, which a slice run judges by the spot law (decision
/// 0276), its first sell is refused and it closes no trade: a test that counts trades on a spot slice runs
/// [`LONG_CROSS_SCRIPT`].
pub const CROSS_SCRIPT: &str = r#"
const QTY = 1.0;
fn on_bar(strategy) {
    let f = sma(5); let s = sma(20);
    if s.is_nan() { return; }
    // an entry the other way also closes the position it meets
    if f > s {
        strategy.entry("Long", OrderSide::Long).size(QTY);
    } else {
        strategy.entry("Short", OrderSide::Short).size(QTY);
    }
}
"#;

/// [`CROSS_SCRIPT`] LONG-ONLY: buy above, sell what was bought below, which the spot law allows, so it trades
/// on a spot pair as on a margin series.
pub const LONG_CROSS_SCRIPT: &str = r#"
const QTY = 1.0;
fn on_bar(strategy) {
    let f = sma(5); let s = sma(20);
    if s.is_nan() { return; }
    if f > s {
        strategy.entry("Long", OrderSide::Long).size(QTY);
    } else if strategy.position() > 0.0 {
        strategy.close("Long");
    }
}
"#;
