use super::*;

/// The order the doc states, and that it is a strict total order over the three variants (a tie
/// would let a profile "loosen" a restored state without the comparison noticing).
#[test]
fn strictness_orders_active_below_reducing_below_halted() {
    assert!(TradingState::Active.strictness() < TradingState::Reducing.strictness());
    assert!(TradingState::Reducing.strictness() < TradingState::Halted.strictness());
}
