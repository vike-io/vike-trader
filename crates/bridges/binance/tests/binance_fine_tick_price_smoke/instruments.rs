//! The instruments the fine-tick smoke judges with, checked offline (no network, no credentials).

use super::decimal::{decompose, shl_exact};
use super::*;

/// The ladder `crates/vike-bridge-core/tests/grid_multiple_survives_the_wire.rs`'s `LADDER`
/// sweeps, in f64 form. The decimal spelling is DERIVED here (`py_f64_str`) rather than paired,
/// because the wire path derives it too.
const RUNGS: [f64; 10] = [1e-8, 1e-7, 1e-6, 1e-5, 1e-4, 1e-3, 1e-2, 1e-1, 5e-1, 1e0];

/// Multiples swept per rung — the recording's own sample count.
const MULTIPLES: i128 = 2000;

/// The selection criterion must agree with the REAL chain, measured here rather than pinned
/// from a remembered table.
///
/// Only the sound direction is asserted. A value landing below the grid point's float REQUIRES
/// the step's own image to sit below its decimal (if `fl(step) >= step` then
/// `n · fl(step) >= n · step` in real arithmetic, and rounding to f64 is monotone, so the
/// result cannot fall below the grid point's float). The converse is NOT a theorem — an
/// exposed step makes a low landing possible, not certain — so this file never asserts that a
/// rung the predicate calls exposed must lose something.
#[test]
fn the_criterion_agrees_with_the_real_chain_on_every_rung() {
    let mut rungs_that_drift = 0usize;
    for step_f in RUNGS {
        let step = parse_dec(&py_f64_str(step_f)).expect("py_f64_str is positional");
        let verdict = f64_image_sits_below_its_decimal(step_f, step);
        assert!(verdict.is_some(), "a ladder rung must be decidable: {step_f}");

        let mut below = 0usize;
        for n in 1..=MULTIPLES {
            let value = round_to_step(n as f64 * step_f, step_f);
            let grid = render_multiple(n, step).expect("a ladder multiple fits an i128");
            let grid_float = grid.parse::<f64>().expect("a plain decimal parses");
            if grid_float.to_bits() > value.to_bits() {
                below += 1;
            }
        }
        if below > 0 {
            rungs_that_drift += 1;
            assert_eq!(
                verdict,
                Some(true),
                "step {step_f} put {below}/{MULTIPLES} multiples below their grid point, so \
                 its f64 image MUST read as sitting below its decimal — the live selection \
                 would otherwise pass over the very rungs that carry the defect"
            );
        }
    }
    assert!(
        rungs_that_drift > 0,
        "no rung on the ladder drifted at all, so this test just proved nothing about the \
         criterion — either the chain changed or the sweep stopped reaching it"
    );
}

/// The worked example from `crates/vike-bridge-core/src/format.rs` — `5 · fl(1e-6)` is
/// `0.0000049999999999999996` — is precisely the shape the live selection hunts for, and the
/// fixed formatter carries it whole.
#[test]
fn the_worked_example_is_the_precondition_the_selection_looks_for() {
    let tick = 1e-6;
    let step = parse_dec(&py_f64_str(tick)).expect("py_f64_str is positional");
    let price = round_to_step(5.0 * tick, tick);
    let grid = render_multiple(5, step).expect("five micro-lots fit an i128");
    assert_eq!(grid, "0.000005");
    let grid_float = grid.parse::<f64>().expect("a plain decimal parses");
    assert!(
        grid_float.to_bits() as i64 - price.to_bits() as i64 >= 1,
        "round_to_step gave {price}, which must sit BELOW the grid point {grid_float}"
    );
    assert_eq!(f64_image_sits_below_its_decimal(tick, step), Some(true));
    assert!(decimal_eq(&format_to_step_f(price, tick), &grid));
}

#[test]
fn a_padded_venue_echo_is_not_a_different_price() {
    assert!(decimal_eq("0.00012300", "0.000123"));
    assert!(decimal_eq("5", "5.000"));
    assert!(!decimal_eq("0.000122", "0.000123"));
    // ...and a malformed or exponent-form string is never quietly called equal.
    assert!(!decimal_eq("1e-4", "0.0001"));
    assert!(!decimal_eq("", "0.0001"));
    assert!(!decimal_eq("1.2.3", "1.2"));
}

#[test]
fn one_step_low_reads_as_one_step_low() {
    let step = parse_dec("0.000001").expect("a plain decimal");
    assert_eq!(steps_off("0.000004", "0.000005", step), Some(-1));
    assert_eq!(steps_off("0.000005", "0.000005", step), Some(0));
    assert_eq!(steps_off("0.0000055", "0.000005", step), None);
}

#[test]
fn a_multiple_renders_at_the_steps_own_scale() {
    let micro = parse_dec("0.000001").expect("a plain decimal");
    let unit = parse_dec("1.0").expect("a plain decimal");
    let quarter = parse_dec("0.25").expect("a plain decimal");
    assert_eq!(render_multiple(5, micro).as_deref(), Some("0.000005"));
    assert_eq!(render_multiple(123, unit).as_deref(), Some("123.0"));
    assert_eq!(render_multiple(4, quarter).as_deref(), Some("1.00"));
}

/// The size floors are checked against the size the VENUE parses back, not the one computed
/// here — the wire quantizes quantity DOWN against `stepSize`, and a size that clears
/// `minQty`/`minNotional` before that truncation can fail both after it. A rejected order is
/// not a proof of anything, so this is the arithmetic that keeps a live run from spending its
/// one submit on a `-1013`.
#[test]
fn an_order_size_clears_the_venue_floors_after_the_wire_has_truncated_it() {
    let price = 0.000_123_f64;
    let properties = SymbolProperties {
        tick_size: 1e-6,
        step_size: 0.1,
        min_qty: 1.0,
        min_notional: 10.0,
        ..Default::default()
    };
    let OrderSize::Ok(qty) = order_qty(&properties, price) else {
        panic!("a symbol with ordinary floors must be sizeable");
    };
    let on_wire: f64 = format_to_step_f(qty, properties.step_size)
        .parse()
        .expect("the wire size is a plain decimal");
    assert!(
        on_wire >= properties.min_qty,
        "the wire size {on_wire} fell under minQty {}",
        properties.min_qty
    );
    assert!(
        on_wire * price >= properties.min_notional,
        "the wire notional {} fell under minNotional {}",
        on_wire * price,
        properties.min_notional
    );
}

/// A symbol whose CHEAPEST legal order is bigger than the venue will accept is passed over,
/// never submitted — and an ABSENT ceiling (`SymbolProperties`' absent-is-`0.0` convention) is
/// not a ceiling of zero, which would have refused every symbol on the grid.
#[test]
fn a_size_the_venue_would_refuse_is_passed_over_rather_than_submitted() {
    let capped = SymbolProperties {
        step_size: 0.1,
        min_qty: 1.0,
        max_qty: 5.0,
        min_notional: 10.0,
        ..Default::default()
    };
    assert_matches!(order_qty(&capped, 0.001), OrderSize::OverMaxQty);
    let uncapped = SymbolProperties { max_qty: 0.0, ..capped };
    assert_matches!(order_qty(&uncapped, 0.001), OrderSize::Ok(_));
}

/// An undecidable step is UNKNOWN, never "clean" — otherwise an overflow would silently look
/// like a symbol that had been examined and cleared.
#[test]
fn the_criterion_declines_rather_than_guessing() {
    let too_deep = Dec { digits: 1, scale: 40 };
    assert_eq!(f64_image_sits_below_its_decimal(1e-40, too_deep), None);
    assert_eq!(decompose(0.0), None);
    assert_eq!(decompose(f64::NAN), None);
    assert_eq!(shl_exact(u128::MAX, 1), None);
    assert_eq!(parse_dec("nonsense"), None);
}
