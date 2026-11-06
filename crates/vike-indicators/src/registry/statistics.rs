//! Registry rows — statistics (statistics.py), single-series only.
use super::*;

#[rustfmt::skip]
pub(super) fn rows() -> Vec<IndicatorMeta> {
    use Category::*;
    use RenderKind::*;
    vec![
        // ---- statistics (statistics.py) — single-series only ----
        ind!(
            "linearreg", "Linear Regression", Statistics, Oscillator, false, out! {"linearreg":Line}, &[],
            params!(("period", 14.0, 2.0, 200.0, 1.0)), Linearreg
        ),
        ind!(
            "linearreg_slope", "Linear Reg Slope", Statistics, Oscillator, false, out! {"slope":Line}, &[0.0],
            params!(("period", 14.0, 2.0, 200.0, 1.0)), LinearregSlope
        ),
        ind!(
            "linearreg_angle", "Linear Reg Angle", Statistics, Oscillator, false, out! {"angle":Line}, &[0.0],
            params!(("period", 14.0, 2.0, 200.0, 1.0)), LinearregAngle
        ),
        ind!(
            "linearreg_intercept", "Linear Reg Intercept", Statistics, Oscillator, false, out! {"intercept":Line}, &[],
            params!(("period", 14.0, 2.0, 200.0, 1.0)), LinearregIntercept
        ),
        ind!(
            "tsf", "Time Series Forecast", Statistics, Oscillator, false, out! {"tsf":Line}, &[],
            params!(("period", 14.0, 2.0, 200.0, 1.0)), Tsf
        ),
        ind!(
            "var", "Variance", Statistics, Oscillator, false, out! {"var":Line}, &[],
            params!(("period", 20.0, 2.0, 200.0, 1.0)), Var
        ),
        ind!(
            "zscore", "Z-Score", Statistics, Oscillator, false, out! {"zscore":Line}, &[-2.0, 0.0, 2.0],
            params!(("period", 20.0, 2.0, 200.0, 1.0)), Zscore
        ),
        ind!(
            "skew", "Skewness", Statistics, Oscillator, false, out! {"skew":Line}, &[0.0],
            params!(("period", 20.0, 2.0, 200.0, 1.0)), Skew
        ),
        ind!(
            "kurtosis", "Kurtosis", Statistics, Oscillator, false, out! {"kurtosis":Line}, &[0.0],
            params!(("period", 20.0, 2.0, 200.0, 1.0)), Kurtosis
        ),
        ind!(
            "mad", "Mean Abs Deviation", Statistics, Oscillator, false, out! {"mad":Line}, &[],
            params!(("period", 20.0, 2.0, 200.0, 1.0)), Mad
        ),
        ind!(
            "std_error", "Standard Error", Statistics, Oscillator, false, out! {"std_error":Line}, &[],
            params!(("period", 20.0, 2.0, 200.0, 1.0)), StdError
        ),
        ind!(
            "std_error_bands", "Std Error Bands", Statistics, Oscillator, false,
            out! {"upper":Line, "mid":Line, "lower":Line}, &[],
            params!(("period", 20.0, 2.0, 200.0, 1.0), ("mult", 2.0, 0.1, 10.0, 0.1)), StdErrorBands
        ),
        ind!(
            "rank_correlation", "Rank Correlation", Statistics, Oscillator, false, out! {"rci":Line}, &[-80.0, 80.0],
            params!(("period", 14.0, 2.0, 200.0, 1.0)), RankCorrelation
        ),
    ]
}
