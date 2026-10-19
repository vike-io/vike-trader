//! Registry rows — additions after the first release; filled in by the indicator additions.
use super::*;

#[rustfmt::skip]
pub(super) fn rows() -> Vec<IndicatorMeta> {
    use Category::*;
    use RenderKind::*;
    vec![
        // ---- directional movement (Wilder; the same smoothing as the `adx` row's directional lines) ----
        ind!(
            "dx", "Directional Movement Index", Momentum, Oscillator, false, out! {"dx":Line}, &[],
            params!(("period", 14.0, 2.0, 100.0, 1.0)), Dx
        ),
        ind!(
            "plus_di", "Plus Directional Indicator", Momentum, Oscillator, false, out! {"plus_di":Line}, &[],
            params!(("period", 14.0, 2.0, 100.0, 1.0)), PlusDi
        ),
        ind!(
            "minus_di", "Minus Directional Indicator", Momentum, Oscillator, false, out! {"minus_di":Line}, &[],
            params!(("period", 14.0, 2.0, 100.0, 1.0)), MinusDi
        ),
        ind!(
            "plus_dm", "Plus Directional Movement", Momentum, Oscillator, false, out! {"plus_dm":Line}, &[],
            params!(("period", 14.0, 2.0, 100.0, 1.0)), PlusDm
        ),
        ind!(
            "minus_dm", "Minus Directional Movement", Momentum, Oscillator, false, out! {"minus_dm":Line}, &[],
            params!(("period", 14.0, 2.0, 100.0, 1.0)), MinusDm
        ),
        // ---- momentum / volatility ----
        ind!(
            "rwi", "Random Walk Index", Momentum, Oscillator, false, out! {"rwi_high":Line, "rwi_low":Line}, &[1.0],
            params!(("period", 14.0, 2.0, 200.0, 1.0)), Rwi
        ),
        ind!(
            "qstick", "Qstick", Momentum, Oscillator, false, out! {"qstick":Line}, &[0.0],
            params!(("period", 8.0, 2.0, 200.0, 1.0)), Qstick
        ),
        ind!(
            "chaikin_volatility", "Chaikin Volatility", Volatility, Oscillator, false, out! {"chaikin_volatility":Line}, &[0.0],
            params!(("ema_period", 10.0, 2.0, 100.0, 1.0), ("roc_period", 10.0, 1.0, 100.0, 1.0)), ChaikinVolatility
        ),
        // ---- volume (cumulative, hand-written: path-dependent) ----
        ind!("williams_ad", "Williams A/D", Volume, Oscillator, false, out! {"williams_ad":Line}, &[], &[], WilliamsAd),
        // `anchor_ts` is epoch milliseconds; its step is NOT 1.0 so the integer-period epsilon gate in `tests/params.rs` skips it.
        ind!(
            "anchored_vwap", "Anchored VWAP", Volume, Overlay, false, out! {"anchored_vwap":Line}, &[],
            params!(("anchor_ts", 0.0, 0.0, 4.0e12, 60000.0)), AnchoredVwap
        ),
        // ---- price (recurrent, hand-written: path-dependent) ----
        ind!(
            "heikin_ashi", "Heikin-Ashi", Price, Overlay, false,
            out! {"ha_open":Line, "ha_high":Line, "ha_low":Line, "ha_close":Line}, &[], &[], HeikinAshi
        ),
        // ---- statistics ----
        ind!(
            "r_squared", "R-Squared", Statistics, Oscillator, false, out! {"r_squared":Line}, &[0.0, 1.0],
            params!(("period", 14.0, 2.0, 200.0, 1.0)), RSquared
        ),
        // `period >= lag + 2` or every bar is NaN; the pair is deliberately not narrowed by the ranges.
        ind!(
            "autocorr", "Autocorrelation", Statistics, Oscillator, false, out! {"autocorr":Line}, &[0.0],
            params!(("period", 30.0, 5.0, 200.0, 1.0), ("lag", 1.0, 1.0, 50.0, 1.0)), Autocorr
        ),
        ind!(
            "hurst", "Hurst Exponent", Statistics, Oscillator, false, out! {"hurst":Line}, &[0.5],
            params!(("period", 100.0, 17.0, 400.0, 1.0)), Hurst
        ),
    ]
}
