//! Registry rows — volatility (volatility.py).
use super::*;

#[rustfmt::skip]
pub(super) fn rows() -> Vec<IndicatorMeta> {
    use Category::*;
    use RenderKind::*;
    vec![
        // ---- volatility (volatility.py) ----
        ind!("true_range", "True Range", Volatility, Oscillator, false, out! {"true_range":Line}, &[], &[], TrueRange),
        ind!(
            "natr", "Normalized ATR", Volatility, Oscillator, false, out! {"natr":Line}, &[],
            params!(("period", 14.0, 2.0, 100.0, 1.0)), Natr
        ),
        ind!(
            "stddev", "Std Deviation", Volatility, Oscillator, false, out! {"stddev":Line}, &[],
            params!(("period", 20.0, 2.0, 200.0, 1.0)), Stddev
        ),
        ind!(
            "hvol", "Historical Volatility", Volatility, Oscillator, false, out! {"hvol":Line}, &[],
            params!(("period", 20.0, 2.0, 200.0, 1.0), ("ann", 365.0, 1.0, 365.0, 1.0)), Hvol
        ),
        ind!(
            "bbands_pctb", "Bollinger %B", Volatility, Oscillator, false, out! {"pctb":Line}, &[0.0, 1.0],
            params!(("period", 20.0, 2.0, 200.0, 1.0), ("k", 2.0, 0.5, 5.0, 0.1)), BbandsPctb
        ),
        ind!(
            "bbands_width", "Bollinger Width", Volatility, Oscillator, false, out! {"width":Line}, &[],
            params!(("period", 20.0, 2.0, 200.0, 1.0), ("k", 2.0, 0.5, 5.0, 0.1)), BbandsWidth
        ),
        ind!(
            "donchian_width", "Donchian Width", Volatility, Oscillator, false, out! {"width":Line}, &[],
            params!(("period", 20.0, 2.0, 200.0, 1.0)), DonchianWidth
        ),
        ind!(
            "ulcer", "Ulcer Index", Volatility, Oscillator, false, out! {"ulcer":Line}, &[],
            params!(("period", 14.0, 2.0, 200.0, 1.0)), Ulcer
        ),
        ind!(
            "chop", "Choppiness Index", Volatility, Oscillator, false, out! {"chop":Line}, &[38.2, 61.8],
            params!(("period", 14.0, 2.0, 200.0, 1.0)), Chop
        ),
        ind!(
            "relative_volatility", "Relative Volatility Index", Volatility, Oscillator, false, out! {"rvi":Line},
            &[20.0, 80.0], params!(("period", 14.0, 2.0, 200.0, 1.0)), RelativeVolatility
        ),
        ind!(
            "high_low_52w", "52-Week High/Low", Volatility, Overlay, false, out! {"high_n":Line, "low_n":Line}, &[],
            params!(("period", 252.0, 2.0, 1000.0, 1.0)), HighLow52w
        ),
        ind!(
            "mass", "Mass Index", Volatility, Oscillator, false, out! {"mass":Line}, &[26.5, 27.0],
            params!(("period", 25.0, 2.0, 200.0, 1.0), ("ema_period", 9.0, 2.0, 50.0, 1.0)), Mass
        ),
    ]
}
