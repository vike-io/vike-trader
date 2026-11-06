//! The property suites' shared generators (`tests/event_roundtrip.rs`, `tests/trade_event_roundtrip.rs`).
//! Their weights are fixed: a saved failure seed reproduces only under the same strategy.

use proptest::prelude::Strategy;

/// Every finite `f64` class: normal, subnormal, zero (both signs), either sign.
pub fn finite() -> impl Strategy<Value = f64> {
    proptest::num::f64::POSITIVE
        | proptest::num::f64::NEGATIVE
        | proptest::num::f64::NORMAL
        | proptest::num::f64::SUBNORMAL
        | proptest::num::f64::ZERO
}
