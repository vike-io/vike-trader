//! Registry rows — additions after the first release; filled in by the indicator additions.
use super::*;

#[rustfmt::skip]
pub(super) fn rows() -> Vec<IndicatorMeta> {
    use Category::*;
    use RenderKind::*;
    vec![
        // ---- directional movement (Wilder; the same smoothing as the `adx` row's directional lines) ----
        indicator!(
            "dx", "Directional Movement Index", Momentum, Oscillator, false, out! {"dx":Line}, &[],
            params!(("period", 14.0, 2.0, 100.0, 1.0)), Dx
        ),
        indicator!(
            "plus_di", "Plus Directional Indicator", Momentum, Oscillator, false, out! {"plus_di":Line}, &[],
            params!(("period", 14.0, 2.0, 100.0, 1.0)), PlusDi
        ),
        indicator!(
            "minus_di", "Minus Directional Indicator", Momentum, Oscillator, false, out! {"minus_di":Line}, &[],
            params!(("period", 14.0, 2.0, 100.0, 1.0)), MinusDi
        ),
        indicator!(
            "plus_dm", "Plus Directional Movement", Momentum, Oscillator, false, out! {"plus_dm":Line}, &[],
            params!(("period", 14.0, 2.0, 100.0, 1.0)), PlusDm
        ),
        indicator!(
            "minus_dm", "Minus Directional Movement", Momentum, Oscillator, false, out! {"minus_dm":Line}, &[],
            params!(("period", 14.0, 2.0, 100.0, 1.0)), MinusDm
        ),
        // ---- momentum / volatility ----
        indicator!(
            "rwi", "Random Walk Index", Momentum, Oscillator, false, out! {"rwi_high":Line, "rwi_low":Line}, &[1.0],
            params!(("period", 14.0, 2.0, 200.0, 1.0)), Rwi
        ),
        indicator!(
            "qstick", "Qstick", Momentum, Oscillator, false, out! {"qstick":Line}, &[0.0],
            params!(("period", 8.0, 2.0, 200.0, 1.0)), Qstick
        ),
        indicator!(
            "chaikin_volatility", "Chaikin Volatility", Volatility, Oscillator, false, out! {"chaikin_volatility":Line}, &[0.0],
            params!(("ema_period", 10.0, 2.0, 100.0, 1.0), ("roc_period", 10.0, 1.0, 100.0, 1.0)), ChaikinVolatility
        ),
        // ---- volume (cumulative, hand-written: path-dependent) ----
        indicator!("williams_ad", "Williams A/D", Volume, Oscillator, false, out! {"williams_ad":Line}, &[], &[], WilliamsAd),
        // `anchor_timestamp` is epoch milliseconds; its step is NOT 1.0 so the integer-period epsilon gate in `tests/parity/params.rs` skips it.
        indicator!(
            "anchored_vwap", "Anchored VWAP", Volume, Overlay, false, out! {"anchored_vwap":Line}, &[],
            params!(("anchor_ts", 0.0, 0.0, 4.0e12, 60000.0)), AnchoredVwap
        ),
        // ---- price (recurrent, hand-written: path-dependent) ----
        indicator!(
            "heikin_ashi", "Heikin-Ashi", Price, Overlay, false,
            out! {"ha_open":Line, "ha_high":Line, "ha_low":Line, "ha_close":Line}, &[], &[], HeikinAshi
        ),
        // ---- statistics ----
        indicator!(
            "r_squared", "R-Squared", Statistics, Oscillator, false, out! {"r_squared":Line}, &[0.0, 1.0],
            params!(("period", 14.0, 2.0, 200.0, 1.0)), RSquared
        ),
        // `period >= lag + 2` or every bar is NaN; the pair is deliberately not narrowed by the ranges.
        indicator!(
            "autocorr", "Autocorrelation", Statistics, Oscillator, false, out! {"autocorr":Line}, &[0.0],
            params!(("period", 30.0, 5.0, 200.0, 1.0), ("lag", 1.0, 1.0, 50.0, 1.0)), Autocorr
        ),
        indicator!(
            "hurst", "Hurst Exponent", Statistics, Oscillator, false, out! {"hurst":Line}, &[0.5],
            params!(("period", 100.0, 17.0, 400.0, 1.0)), Hurst
        ),
        // ---- oscillators the strategy lab computed by hand (`indicators/streams_osc.rs` names each source) ----
        indicator!(
            "wavetrend", "WaveTrend", Momentum, Oscillator, false, out! {"wavetrend":Line, "signal":Line}, &[-60.0, 0.0, 60.0],
            params!(("channel", 10.0, 2.0, 500.0, 1.0), ("average", 21.0, 2.0, 500.0, 1.0)), Wavetrend
        ),
        indicator!(
            "qqe", "Quantitative Qualitative Estimation", Momentum, Oscillator, false, out! {"qqe":Line, "trail":Line}, &[50.0],
            params!(("rsi_period", 14.0, 2.0, 100.0, 1.0), ("smoothing", 5.0, 1.0, 100.0, 1.0), ("factor", 4.236, 0.1, 20.0, 0.001)), Qqe
        ),
        indicator!(
            "stc", "Schaff Trend Cycle", Momentum, Oscillator, false, out! {"stc":Line}, &[25.0, 75.0],
            params!(("fast", 23.0, 2.0, 500.0, 1.0), ("slow", 50.0, 2.0, 1000.0, 1.0), ("cycle", 10.0, 2.0, 500.0, 1.0)), Stc
        ),
        indicator!(
            "percent_rank", "Percent Rank", Statistics, Oscillator, false, out! {"percent_rank":Line}, &[],
            params!(("length", 100.0, 1.0, 5000.0, 1.0)), PercentRank
        ),
        // ---- filters (`indicators/filters.rs`) ----
        indicator!(
            "early_onset_trend", "Early Onset Trend (Ehlers)", Momentum, Oscillator, false, out! {"early_onset_trend":Line}, &[0.0],
            params!(("lp_period", 30.0, 2.0, 400.0, 1.0), ("k", 0.85, -0.99, 0.99, 0.01)), EarlyOnsetTrend
        ),
    ]
}
