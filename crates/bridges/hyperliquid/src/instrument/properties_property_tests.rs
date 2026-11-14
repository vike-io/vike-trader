//! "Arbitrary input never panics" harness for the PRIVATE grid derivation of this module:
//! [`HyperliquidInstruments::build`] (the loader's tail, `from_symbology` -> `properties_for` ->
//! `pow10_neg`) fed hostile `meta` / `spotMeta` bodies. The public decoders are covered in
//! `crates/bridges/hyperliquid/tests/decoder_never_panics.rs`; the grid derivation is reachable from
//! outside the crate only through a live `/info` fetch, so it gets a sibling unit file in the
//! `instruments_tests.rs` style.
//!
//! The property is TOTALITY: `szDecimals` is a wire `u64` truncated to `u32`, and the derived tick /
//! step must be a finite positive grid for every value of it. A minimized counterexample is a REAL
//! bug: commit the `proptest-regressions` seed beside this file and fix the decoder.

use super::*;
use proptest::prelude::*;
use serde_json::json;

/// `szDecimals` values: the real range, then the edges of the `u64 -> u32 -> i32` casts
/// `pow10_neg` performs (`2^31` is `i32::MIN` once truncated, and negating it overflows).
fn sz_decimals() -> impl Strategy<Value = u64> {
    prop_oneof![
        6 => 0u64..12,
        1 => Just((1u64 << 31) - 1),
        1 => Just(1u64 << 31),
        1 => Just(u64::from(u32::MAX)),
        1 => Just(1u64 << 32),
        1 => any::<u64>(),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Every instrument of a hostile universe gets a usable grid: positive, finite tick and step.
    #[test]
    fn grid_derivation_is_total_over_any_sz_decimals(
        perp_decimals in prop::collection::vec(sz_decimals(), 0..4),
        spot_decimals in prop::collection::vec(sz_decimals(), 0..4),
    ) {
        let universe: Vec<_> = perp_decimals
            .iter()
            .enumerate()
            .map(|(i, d)| json!({"name": format!("P{i}"), "szDecimals": d, "maxLeverage": 10}))
            .collect();
        let tokens: Vec<_> = spot_decimals
            .iter()
            .enumerate()
            .map(|(i, d)| json!({"name": format!("T{i}"), "szDecimals": d, "index": i + 1}))
            .collect();
        let pairs: Vec<_> = (0..spot_decimals.len())
            .map(|i| json!({"name": format!("@{i}"), "index": i, "tokens": [i + 1, 0]}))
            .collect();
        let mut all_tokens = vec![json!({"name": "USDC", "szDecimals": 8, "index": 0})];
        all_tokens.extend(tokens);
        let built = HyperliquidInstruments::build(
            &json!({ "universe": universe }),
            &json!({ "tokens": all_tokens, "universe": pairs }),
            None,
        );
        for (_, p) in built.iter() {
            prop_assert!(p.tick_size >= 0.0 && p.tick_size.is_finite());
            prop_assert!(p.step_size >= 0.0 && p.step_size.is_finite());
        }
    }
}
