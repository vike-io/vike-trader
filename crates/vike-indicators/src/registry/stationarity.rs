//! Registry rows — stationarity and memory (`indicators/stationarity.rs`, net-new; no Python twin).
use super::*;

#[rustfmt::skip]
pub(super) fn rows() -> Vec<IndicatorMeta> {
    use Category::*;
    use RenderKind::*;
    vec![
        // The band is the 5% critical value of Kwiatkowski et al. (1992), Table 1: above it, the log close is not level-stationary.
        indicator!(
            "kpss", "KPSS Stationarity", Statistics, Oscillator, false, out! {"kpss":Line}, &[0.463],
            params!(("period", 100.0, 20.0, 400.0, 1.0), ("lags", 4.0, 0.0, 32.0, 1.0)), Kpss
        ),
        indicator!(
            "fractional_difference", "Fractional Difference (FFD)", Statistics, Oscillator, false,
            out! {"fractional_difference":Line}, &[],
            params!(("d", 0.4, 0.0, 2.0, 0.05), ("window", 100.0, 2.0, 1000.0, 1.0)), FractionalDifference
        ),
        indicator!(
            "shannon_entropy", "Shannon Entropy", Statistics, Oscillator, false, out! {"shannon_entropy":Line}, &[],
            params!(("period", 50.0, 10.0, 1000.0, 1.0), ("bins", 10.0, 2.0, 100.0, 1.0)), ShannonEntropy
        ),
        indicator!(
            "lempel_ziv", "Lempel-Ziv Complexity", Statistics, Oscillator, false, out! {"lempel_ziv":Line}, &[1.0],
            params!(("period", 100.0, 10.0, 1000.0, 1.0)), LempelZiv
        ),
    ]
}
