//! Registry rows — overlays added after the first release: adaptive moving averages, trailing stops
//! and volatility / regression bands (`indicators/overlays.rs`).
use super::*;

#[rustfmt::skip]
pub(super) fn rows() -> Vec<IndicatorMeta> {
    use Category::*;
    use RenderKind::*;
    vec![
        // ---- adaptive and elastic moving averages ----
        ind!(
            "kama", "Kaufman Adaptive MA", Overlap, Overlay, false, out! {"kama":Line}, &[],
            params!(("period", 10.0, 2.0, 200.0, 1.0), ("fast", 2.0, 1.0, 100.0, 1.0), ("slow", 30.0, 2.0, 400.0, 1.0)), Kama
        ),
        ind!(
            "vidya", "Variable Index Dynamic Average", Overlap, Overlay, false, out! {"vidya":Line}, &[],
            params!(("period", 14.0, 2.0, 200.0, 1.0), ("cmo_period", 9.0, 2.0, 200.0, 1.0)), Vidya
        ),
        ind!(
            "frama", "Fractal Adaptive MA", Overlap, Overlay, false, out! {"frama":Line}, &[],
            params!(("period", 16.0, 2.0, 400.0, 2.0)), Frama
        ),
        ind!(
            "evwma", "Elastic Volume-Weighted MA", Overlap, Overlay, false, out! {"evwma":Line}, &[],
            params!(("period", 20.0, 2.0, 400.0, 1.0)), Evwma
        ),
        ind!(
            "median_ma", "Median MA", Overlap, Overlay, false, out! {"median_ma":Line}, &[],
            params!(("period", 20.0, 2.0, 400.0, 1.0)), MedianMa
        ),
        // ---- trailing stops ----
        ind!(
            "chandelier", "Chandelier Exit", Overlap, Overlay, false, out! {"long_stop":Line, "short_stop":Line}, &[],
            params!(("period", 22.0, 2.0, 200.0, 1.0), ("mult", 3.0, 0.5, 10.0, 0.5)), Chandelier
        ),
        ind!(
            "hilo_activator", "Gann HiLo Activator", Overlap, Overlay, false, out! {"hilo":Line}, &[],
            params!(("period", 3.0, 1.0, 200.0, 1.0)), HiloActivator
        ),
        ind!(
            "atr_trailing_stop", "ATR Trailing Stop", Overlap, Overlay, false, out! {"stop":Line}, &[],
            params!(("period", 14.0, 1.0, 200.0, 1.0), ("mult", 3.0, 0.5, 10.0, 0.5)), AtrTrailingStop
        ),
        // ---- bands and channels ----
        ind!(
            "ttm_squeeze", "TTM Squeeze", Volatility, Oscillator, false, out! {"momentum":Histogram, "squeeze":Dots}, &[],
            params!(("length", 20.0, 2.0, 200.0, 1.0), ("bb_mult", 2.0, 0.5, 5.0, 0.1), ("kc_mult", 1.5, 0.5, 5.0, 0.1)), TtmSqueeze
        ),
        ind!(
            "starc", "STARC Bands", Volatility, Overlay, false, out! {"upper":Band, "mid":Line, "lower":Band}, &[],
            params!(("sma_period", 6.0, 1.0, 200.0, 1.0), ("atr_period", 15.0, 1.0, 200.0, 1.0), ("mult", 2.0, 0.5, 10.0, 0.5)), Starc
        ),
        ind!(
            "acceleration_bands", "Acceleration Bands", Volatility, Overlay, false, out! {"upper":Band, "mid":Line, "lower":Band}, &[],
            params!(("period", 20.0, 2.0, 400.0, 1.0), ("factor", 4.0, 0.5, 10.0, 0.5)), AccelerationBands
        ),
        ind!(
            "linreg_channel", "Linear Regression Channel", Volatility, Overlay, false, out! {"upper":Band, "mid":Line, "lower":Band}, &[],
            params!(("period", 100.0, 2.0, 400.0, 1.0), ("mult", 2.0, 0.5, 10.0, 0.5)), LinregChannel
        ),
    ]
}
