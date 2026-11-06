//! Registry rows — overlap / trend (overlap.py).
use super::*;

#[rustfmt::skip]
pub(super) fn rows() -> Vec<IndicatorMeta> {
    use Category::*;
    use RenderKind::*;
    vec![
        // ---- overlap / trend (overlap.py) ----
        ind!(
            "dema", "Double EMA", Overlap, Overlay, false, out! {"dema":Line}, &[],
            params!(("period", 20.0, 2.0, 400.0, 1.0)), Dema
        ),
        ind!(
            "tema", "Triple EMA", Overlap, Overlay, false, out! {"tema":Line}, &[],
            params!(("period", 20.0, 2.0, 400.0, 1.0)), Tema
        ),
        ind!(
            "trima", "Triangular MA", Overlap, Overlay, false, out! {"trima":Line}, &[],
            params!(("period", 20.0, 2.0, 400.0, 1.0)), Trima
        ),
        ind!(
            "smma", "Smoothed MA", Overlap, Overlay, false, out! {"smma":Line}, &[],
            params!(("period", 14.0, 2.0, 400.0, 1.0)), Smma
        ),
        ind!(
            "zlema", "Zero-Lag EMA", Overlap, Overlay, false, out! {"zlema":Line}, &[],
            params!(("period", 20.0, 2.0, 400.0, 1.0)), Zlema
        ),
        ind!(
            "hma", "Hull MA", Overlap, Overlay, false, out! {"hma":Line}, &[],
            params!(("period", 20.0, 2.0, 400.0, 1.0)), Hma
        ),
        ind!(
            "vwma", "Volume-Weighted MA", Overlap, Overlay, false, out! {"vwma":Line}, &[],
            params!(("period", 20.0, 2.0, 400.0, 1.0)), Vwma
        ),
        ind!(
            "t3", "Tillson T3", Overlap, Overlay, false, out! {"t3":Line}, &[],
            params!(("period", 20.0, 2.0, 400.0, 1.0), ("v", 0.7, 0.0, 1.0, 0.05)), T3
        ),
        ind!(
            "alma", "Arnaud Legoux MA", Overlap, Overlay, false, out! {"alma":Line}, &[],
            params!(
                ("period", 20.0, 2.0, 400.0, 1.0),
                ("offset", 0.85, 0.0, 1.0, 0.05),
                ("sigma", 6.0, 1.0, 20.0, 0.5)
            ),
            Alma
        ),
        ind!(
            "midpoint", "Midpoint", Overlap, Overlay, false, out! {"midpoint":Line}, &[],
            params!(("period", 14.0, 2.0, 400.0, 1.0)), Midpoint
        ),
        ind!(
            "midprice", "Midprice", Overlap, Overlay, false, out! {"midprice":Line}, &[],
            params!(("period", 14.0, 2.0, 400.0, 1.0)), Midprice
        ),
        ind!(
            "supertrend", "Supertrend", Overlap, Overlay, false, out! {"supertrend":Line, "direction":Line}, &[],
            params!(("period", 10.0, 1.0, 100.0, 1.0), ("mult", 3.0, 0.5, 10.0, 0.5)), Supertrend
        ),
        ind!(
            "ichimoku", "Ichimoku Cloud", Overlap, Overlay, true,
            out! {"tenkan":Line, "kijun":Line, "senkou_a":Line, "senkou_b":Line, "chikou":Line}, &[],
            params!(
                ("tenkan", 9.0, 2.0, 100.0, 1.0),
                ("kijun", 26.0, 2.0, 100.0, 1.0),
                ("senkou", 52.0, 2.0, 200.0, 1.0)
            ),
            Ichimoku
        ),
        ind!(
            "mcginley", "McGinley Dynamic", Overlap, Overlay, false, out! {"mcginley":Line}, &[],
            params!(("period", 14.0, 2.0, 200.0, 1.0)), Mcginley
        ),
        ind!(
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
        ind!(
            "envelopes", "Envelopes", Overlap, Overlay, false, out! {"upper":Line, "mid":Line, "lower":Line}, &[],
            params!(("period", 20.0, 2.0, 200.0, 1.0), ("pct", 2.5, 0.1, 20.0, 0.1)), Envelopes
        ),
        ind!(
            "alligator", "Alligator", Overlap, Overlay, false, out! {"jaw":Line, "teeth":Line, "lips":Line}, &[], &[],
            Alligator
        ),
    ]
}
