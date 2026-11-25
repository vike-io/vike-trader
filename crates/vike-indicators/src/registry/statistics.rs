//! Registry rows — statistics (statistics.py), single-series only.
use super::*;

#[rustfmt::skip]
pub(super) fn rows() -> Vec<IndicatorMeta> {
    use Category::*;
    use RenderKind::*;
    vec![
        // ---- statistics (statistics.py) — single-series only ----
        indicator!(
            "linearreg", "Linear Regression", Statistics, Oscillator, false, out! {"linearreg":Line}, &[],
            params!(("period", 14.0, 2.0, 200.0, 1.0)), Linearreg
        ),
        indicator!(
            "linearreg_slope", "Linear Reg Slope", Statistics, Oscillator, false, out! {"slope":Line}, &[0.0],
            params!(("period", 14.0, 2.0, 200.0, 1.0)), LinearregSlope
        ),
        indicator!(
            "linearreg_angle", "Linear Reg Angle", Statistics, Oscillator, false, out! {"angle":Line}, &[0.0],
            params!(("period", 14.0, 2.0, 200.0, 1.0)), LinearregAngle
        ),
        indicator!(
            "linearreg_intercept", "Linear Reg Intercept", Statistics, Oscillator, false, out! {"intercept":Line}, &[],
            params!(("period", 14.0, 2.0, 200.0, 1.0)), LinearregIntercept
        ),
        indicator!(
            "tsf", "Time Series Forecast", Statistics, Oscillator, false, out! {"tsf":Line}, &[],
            params!(("period", 14.0, 2.0, 200.0, 1.0)), Tsf
        ),
        indicator!(
            "var", "Variance", Statistics, Oscillator, false, out! {"var":Line}, &[],
            params!(("period", 20.0, 2.0, 200.0, 1.0)), Var
        ),
        indicator!(
            "zscore", "Z-Score", Statistics, Oscillator, false, out! {"zscore":Line}, &[-2.0, 0.0, 2.0],
            params!(("period", 20.0, 2.0, 200.0, 1.0)), Zscore
        ),
        indicator!(
            "skew", "Skewness", Statistics, Oscillator, false, out! {"skew":Line}, &[0.0],
            params!(("period", 20.0, 2.0, 200.0, 1.0)), Skew
        ),
        indicator!(
            "kurtosis", "Kurtosis", Statistics, Oscillator, false, out! {"kurtosis":Line}, &[0.0],
            params!(("period", 20.0, 2.0, 200.0, 1.0)), Kurtosis
        ),
        indicator!(
            "mad", "Mean Abs Deviation", Statistics, Oscillator, false, out! {"mad":Line}, &[],
            params!(("period", 20.0, 2.0, 200.0, 1.0)), Mad
        ),
        indicator!(
            "std_error", "Standard Error", Statistics, Oscillator, false, out! {"std_error":Line}, &[],
            params!(("period", 20.0, 2.0, 200.0, 1.0)), StdError
        ),
        indicator!(
            "std_error_bands", "Std Error Bands", Statistics, Oscillator, false,
            out! {"upper":Line, "mid":Line, "lower":Line}, &[],
            params!(("period", 20.0, 2.0, 200.0, 1.0), ("mult", 2.0, 0.1, 10.0, 0.1)), StdErrorBands
        ),
        indicator!(
            "rank_correlation", "Rank Correlation", Statistics, Oscillator, false, out! {"rci":Line}, &[-80.0, 80.0],
            params!(("period", 14.0, 2.0, 200.0, 1.0)), RankCorrelation
        ),
        // ---- market microstructure (`indicators/microstructure.rs`) ----
        indicator!(
            "roll_spread", "Roll Spread", Statistics, Oscillator, false, out! {"roll_spread":Line}, &[],
            params!(("period", 20.0, 2.0, 1000.0, 1.0)), RollSpread
        ),
    ]
}
