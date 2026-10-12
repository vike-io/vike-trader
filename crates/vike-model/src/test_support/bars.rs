//! Comparing bars bit for bit.

use crate::Bar;

/// Bit-exact bar comparison: the count, then per bar `ts`, the OHLCV floats and `funding` through
/// `f64::to_bits`, so `-0.0` differs from `0.0`, equal NaN bits compare equal, and a one-ulp drift
/// is a failure naming the field and the bar's index.
///
/// ⚠ `bid`, `ask` and `symbol` are NOT compared. That is the scope of the five copies this replaced
/// (a venue kline parser's, the local store's and the datahub wire's tests), and widening it would
/// change what every one of those assertions means.
pub fn assert_bars_bit_eq(a: &[Bar], b: &[Bar]) {
    assert_eq!(a.len(), b.len(), "bar count");
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        assert_eq!(x.ts, y.ts, "ts[{i}]");
        for (name, xv, yv) in [
            ("open", x.open, y.open),
            ("high", x.high, y.high),
            ("low", x.low, y.low),
            ("close", x.close, y.close),
            ("volume", x.volume, y.volume),
        ] {
            assert_eq!(xv.to_bits(), yv.to_bits(), "{name}[{i}] ({xv} vs {yv})");
        }
        assert_eq!(x.funding.map(f64::to_bits), y.funding.map(f64::to_bits), "funding[{i}]");
    }
}
