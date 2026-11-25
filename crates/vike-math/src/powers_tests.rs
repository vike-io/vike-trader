use super::*;

/// **Every power of ten in range is the decimal literal AND `10f64.powi`, bit for bit**: the parse of
/// `1e{k}` (the nearest `f64` to the decimal value, what oanda's `pow10` reads) and the `powi` the
/// bridges, the order ticket and the strategies spell today, which this function replaces without
/// moving a bit.
#[test]
fn a_power_of_ten_is_the_literal_and_powi_bit_for_bit() {
    for k in -22..=22 {
        let got = power_of_ten(k).unwrap_or_else(|| panic!("10^{k} is in range"));
        let parsed: f64 = format!("1e{k}").parse().expect("a decimal literal parses");
        assert_eq!(
            got.to_bits(),
            parsed.to_bits(),
            "10^{k}: {got:e} against the literal {parsed:e}"
        );
        let raised = 10f64.powi(k);
        assert_eq!(got.to_bits(), raised.to_bits(), "10^{k}: {got:e} against powi's {raised:e}");
    }
}

/// Past ±22 a power of ten is not exact in `f64`, so there is no answer, at the first step out and at
/// the extremes an `i32` reaches (`i32::MIN` has no positive twin).
#[test]
fn a_power_of_ten_out_of_range_has_no_answer() {
    for k in [23, -23, 308, -308, i32::MAX, i32::MIN] {
        assert_eq!(power_of_ten(k), None, "10^{k}");
    }
}
