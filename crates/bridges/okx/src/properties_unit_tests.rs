use super::*;

/// ⚠ **`to_contracts` and the pre-trade gate measure `qty` in DIFFERENT UNITS**, and this pins
/// the conversion between them.
///
/// `parse_okx_perp_instruments` fills `step_size`/`min_qty`/`max_qty` from `lotSz`/`minSz`/
/// `maxMktSz`, which count CONTRACTS — correct for `to_contracts`, which divides a BASE qty by
/// `ct_val` and floors on exactly that grid. But `OrderRequest.qty` is BASE, so a consumer that
/// hands the raw grid to `vike_exec::RiskLimits::from_properties` floors base quantities on a
/// contracts step. `vike-mount`'s okx arm did precisely that.
///
/// The numbers below are BTC-USDT-SWAP's real shape (`ct_val` 0.01 BTC): the gate was applying
/// a 0.01 BTC step where the venue's true base step is 0.0001 BTC — 100x too coarse — and a
/// 0.01 BTC minimum where the real one is 0.0001 BTC.
#[test]
fn the_gate_grid_is_scaled_from_contracts_to_base() {
    let contracts_grid = SymbolProperties {
        tick_size: 0.1,    // quote per base — must NOT scale
        step_size: 0.01,   // lotSz, CONTRACTS
        min_qty: 0.01,     // minSz, CONTRACTS
        max_qty: 1_000.0,  // maxMktSz, CONTRACTS
        min_notional: 5.0, // quote — must NOT scale
        ..Default::default()
    };
    let base = properties_in_base(&contracts_grid, 0.01);

    assert_eq!(base.step_size, 0.0001, "lotSz 0.01 contracts x 0.01 BTC = 0.0001 BTC");
    assert_eq!(base.min_qty, 0.0001, "minSz 0.01 contracts x 0.01 BTC = 0.0001 BTC");
    assert_eq!(base.max_qty, 10.0, "maxMktSz 1000 contracts x 0.01 BTC = 10 BTC");
    // ⚠ The two fields that must NOT move. Scaling either would be the same bug inverted.
    assert_eq!(base.tick_size, 0.1, "tick_size is quote-per-base, not a quantity");
    assert_eq!(base.min_notional, 5.0, "min_notional is quote, not a quantity");
}

/// The scaled grid must be the EXACT inverse of the wire conversion: `to_contracts` divides a
/// base qty by `ct_val` and floors on the contracts step, so one base step must be exactly one
/// contracts lot. That round trip is the whole law, stated here as an identity.
///
/// ⚠ It is asserted on the ARITHMETIC rather than by calling `to_contracts`, because that method
/// lives on `OkxPerpRest` and needs a signer, a transport and a base URL — constructing one here
/// would test the harness. The identity below is what `to_contracts`'s `raw / ct / step` depends
/// on, and `properties_in_base` is its only other consumer.
///
/// NON-VACUOUS: on the raw grid the gate's step is 0.01 BTC, so a 0.0001 BTC order rounds to
/// zero — the ratio asserted here is exactly the 100x the bug applied.
#[test]
fn one_base_step_is_exactly_one_contracts_lot() {
    let ct_val = 0.01;
    let contracts =
        SymbolProperties { step_size: 0.01, min_qty: 0.01, max_qty: 1_000.0, ..Default::default() };
    let base = properties_in_base(&contracts, ct_val);

    assert_eq!(base.step_size / ct_val, contracts.step_size, "one base step == one lot");
    assert_eq!(base.min_qty / ct_val, contracts.min_qty, "the base minimum == minSz lots");
    assert_eq!(base.max_qty / ct_val, contracts.max_qty, "the base maximum == maxMktSz lots");
    // ...and the scaling is a genuine change, not an identity that would make this vacuous.
    assert!(base.step_size < contracts.step_size, "ct_val < 1 must SHRINK the base step");
}

/// A degenerate `ct_val` returns the grid unchanged — today's behaviour — rather than zeroing
/// the step (which would make every order infinitely divisible) or producing NaN.
#[test]
fn a_degenerate_ct_val_leaves_the_grid_alone() {
    let g = SymbolProperties { step_size: 0.01, min_qty: 0.01, ..Default::default() };
    for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let out = properties_in_base(&g, bad);
        assert_eq!(out.step_size, g.step_size, "ct_val {bad} must not alter the grid");
        assert_eq!(out.min_qty, g.min_qty, "ct_val {bad} must not alter the grid");
    }
}
