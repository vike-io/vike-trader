//! Registry rows — overlap / trend (overlap.py).
use super::*;

#[rustfmt::skip]
pub(super) fn rows() -> Vec<IndicatorMeta> {
    use Category::*;
    use RenderKind::*;
    vec![
        // ---- overlap / trend (overlap.py) ----
        indicator!(
            "dema", "Double EMA", Overlap, Overlay, false, out! {"dema":Line}, &[],
            params!(("period", 20.0, 2.0, 400.0, 1.0)), Dema
        ),
        indicator!(
            "tema", "Triple EMA", Overlap, Overlay, false, out! {"tema":Line}, &[],
            params!(("period", 20.0, 2.0, 400.0, 1.0)), Tema
        ),
        indicator!(
            "trima", "Triangular MA", Overlap, Overlay, false, out! {"trima":Line}, &[],
            params!(("period", 20.0, 2.0, 400.0, 1.0)), Trima
        ),
        indicator!(
            "smma", "Smoothed MA", Overlap, Overlay, false, out! {"smma":Line}, &[],
            params!(("period", 14.0, 2.0, 400.0, 1.0)), Smma
        ),
        indicator!(
            "zlema", "Zero-Lag EMA", Overlap, Overlay, false, out! {"zlema":Line}, &[],
            params!(("period", 20.0, 2.0, 400.0, 1.0)), Zlema
        ),
        indicator!(
            "hma", "Hull MA", Overlap, Overlay, false, out! {"hma":Line}, &[],
            params!(("period", 20.0, 2.0, 1000.0, 1.0)), Hma
        ),
        indicator!(
            "vwma", "Volume-Weighted MA", Overlap, Overlay, false, out! {"vwma":Line}, &[],
            params!(("period", 20.0, 2.0, 400.0, 1.0)), Vwma
        ),
        indicator!(
            "t3", "Tillson T3", Overlap, Overlay, false, out! {"t3":Line}, &[],
            params!(("period", 20.0, 2.0, 400.0, 1.0), ("v", 0.7, 0.0, 1.0, 0.05)), T3
        ),
        indicator!(
            "alma", "Arnaud Legoux MA", Overlap, Overlay, false, out! {"alma":Line}, &[],
            params!(
                ("period", 20.0, 2.0, 400.0, 1.0),
                ("offset", 0.85, 0.0, 1.0, 0.05),
                ("sigma", 6.0, 1.0, 20.0, 0.5)
            ),
            Alma
        ),
        indicator!(
            "midpoint", "Midpoint", Overlap, Overlay, false, out! {"midpoint":Line}, &[],
            params!(("period", 14.0, 2.0, 400.0, 1.0)), Midpoint
        ),
        indicator!(
            "midprice", "Midprice", Overlap, Overlay, false, out! {"midprice":Line}, &[],
            params!(("period", 14.0, 2.0, 400.0, 1.0)), Midprice
        ),
        indicator!(
            "supertrend", "Supertrend", Overlap, Overlay, false, out! {"supertrend":Line, "direction":Line}, &[],
            params!(("period", 10.0, 1.0, 100.0, 1.0), ("mult", 3.0, 0.5, 20.0, 0.5)), Supertrend
        ),
        indicator!(
            "ichimoku", "Ichimoku Cloud", Overlap, Overlay, true,
            out! {"tenkan":Line, "kijun":Line, "senkou_a":Line, "senkou_b":Line, "chikou":Line}, &[],
            params!(
                ("tenkan", 9.0, 2.0, 100.0, 1.0),
                ("kijun", 26.0, 2.0, 100.0, 1.0),
                ("senkou", 52.0, 2.0, 200.0, 1.0)
            ),
            Ichimoku
        ),
        indicator!(
            "mcginley", "McGinley Dynamic", Overlap, Overlay, false, out! {"mcginley":Line}, &[],
            params!(("period", 14.0, 2.0, 200.0, 1.0)), Mcginley
        ),
        indicator!(
            "gmma", "Guppy MMA", Overlap, Overlay, false,
            out! {
                "s3":Line,
                "s5":Line,
                "s8":Line,
                "s10":Line,
                "s12":Line,
                "s15":Line,
                "l30":Line,
                "l35":Line,
                "l40":Line,
                "l45":Line,
                "l50":Line,
                "l60":Line
            },
            &[], &[], Gmma
        ),
        indicator!(
            "envelopes", "Envelopes", Overlap, Overlay, false, out! {"upper":Line, "mid":Line, "lower":Line}, &[],
            params!(("period", 20.0, 2.0, 200.0, 1.0), ("pct", 2.5, 0.1, 20.0, 0.1)), Envelopes
        ),
        indicator!(
            "alligator", "Alligator", Overlap, Overlay, false, out! {"jaw":Line, "teeth":Line, "lips":Line}, &[], &[],
            Alligator
        ),
    ]
}
