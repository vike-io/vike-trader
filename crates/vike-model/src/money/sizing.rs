//! The two pure position-size unit converters — exact port of `core/sizing.py`'s
//! `units_from_percent` / `units_from_value`. MOVED here from `vike-backtest::sizing`
//! (accounting-upgrade Phase C) so the live `order_target_*` verbs in vike-core can share
//! the one sizing law with the backtest engines (vike-core must not depend on
//! vike-backtest); the sizer framework (`PositionSizer` et al.) stays in vike-backtest.

/// Convert a target equity fraction to contract units: `pct * equity / (price * multiplier)`.
/// 0.0 if the denominator is non-positive (guards div-by-zero).
pub fn units_from_percent(pct: f64, equity: f64, price: f64, multiplier: f64) -> f64 {
    let denom = price * multiplier;
    if denom > 0.0 { (pct * equity) / denom } else { 0.0 }
}

/// Convert a target notional value to contract units: `value / (price * multiplier)`.
/// 0.0 if the denominator is non-positive.
pub fn units_from_value(value: f64, price: f64, multiplier: f64) -> f64 {
    let denom = price * multiplier;
    if denom > 0.0 { value / denom } else { 0.0 }
}
