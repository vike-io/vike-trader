//! Registry rows — features read off a model of the series (`indicators/models.rs`): GARCH(1,1)
//! volatility, a two-state Gaussian HMM's regime probability, the dominant cycle, the Haar wavelet
//! detail energy and the rolling augmented Dickey-Fuller statistic. The GARCH and HMM parameters are
//! inputs; nothing here fits those models.
use super::*;

#[rustfmt::skip]
pub(super) fn rows() -> Vec<IndicatorMeta> {
    use Category::*;
    use RenderKind::*;
    vec![
        // `alpha + beta` may exceed 1 (an explosive model); the row's doc says what the start is then.
        indicator!(
            "garch_volatility", "GARCH(1,1) Volatility", Volatility, Oscillator, false, out! {"garch_volatility":Line}, &[],
            params!(("omega", 0.000001, 0.0, 1.0, 0.000001), ("alpha", 0.1, 0.0, 1.0, 0.001), ("beta", 0.85, 0.0, 1.0, 0.001)), GarchVolatility
        ),
        // A `sigma` stays above 0: the state's density divides by it.
        indicator!(
            "hmm_regime", "HMM Regime Probability", Statistics, Oscillator, false, out! {"hmm_regime":Line}, &[0.5],
            params!(
                ("mean_0", 0.0, -1.0, 1.0, 0.0001), ("sigma_0", 0.01, 0.000001, 1.0, 0.000001), ("stay_0", 0.98, 0.0, 1.0, 0.001),
                ("mean_1", 0.0, -1.0, 1.0, 0.0001), ("sigma_1", 0.03, 0.000001, 1.0, 0.000001), ("stay_1", 0.95, 0.0, 1.0, 0.001)
            ), HmmRegime
        ),
        indicator!(
            "dominant_cycle", "Dominant Cycle Period", Statistics, Oscillator, false, out! {"dominant_cycle":Line}, &[],
            params!(("length", 64.0, 4.0, 256.0, 1.0), ("min_period", 8.0, 2.0, 256.0, 1.0), ("max_period", 48.0, 2.0, 256.0, 1.0)), DominantCycle
        ),
        indicator!(
            "haar_detail_energy", "Haar Wavelet Detail Energy", Volatility, Oscillator, false, out! {"haar_detail_energy":Line}, &[],
            params!(("length", 64.0, 2.0, 1000.0, 1.0), ("level", 1.0, 1.0, 8.0, 1.0)), HaarDetailEnergy
        ),
        // `lags` stops at 8 so the shortest window (20 closes) keeps a residual degree of freedom
        // (`vike_math::stationarity_tests`'s `augmented_dickey_fuller` needs `period > 2 lags + 3`); the band is the 5% critical value.
        indicator!(
            "adf", "Augmented Dickey-Fuller", Statistics, Oscillator, false, out! {"adf":Line}, &[-2.86],
            params!(("period", 100.0, 20.0, 400.0, 1.0), ("lags", 1.0, 0.0, 8.0, 1.0)), Adf
        ),
    ]
}
