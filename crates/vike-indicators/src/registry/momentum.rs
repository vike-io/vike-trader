//! Registry rows — momentum (momentum.py).
use super::*;

#[rustfmt::skip]
pub(super) fn rows() -> Vec<IndicatorMeta> {
    use Category::*;
    use RenderKind::*;
    vec![
        // ---- momentum (momentum.py) ----
        ind!(
            "mom", "Momentum", Momentum, Oscillator, false, out! {"mom":Line}, &[0.0],
            params!(("period", 10.0, 1.0, 400.0, 1.0)), Mom
        ),
        ind!(
            "rocp", "ROC Percentage", Momentum, Oscillator, false, out! {"rocp":Line}, &[0.0],
            params!(("period", 10.0, 1.0, 400.0, 1.0)), Rocp
        ),
        ind!(
            "rocr", "ROC Ratio", Momentum, Oscillator, false, out! {"rocr":Line}, &[],
            params!(("period", 10.0, 1.0, 400.0, 1.0)), Rocr
        ),
        ind!(
            "rocr100", "ROC Ratio ×100", Momentum, Oscillator, false, out! {"rocr100":Line}, &[],
            params!(("period", 10.0, 1.0, 400.0, 1.0)), Rocr100
        ),
        ind!(
            "apo", "Absolute Price Osc", Momentum, Oscillator, false, out! {"apo":Line}, &[0.0],
            params!(("fast", 12.0, 2.0, 200.0, 1.0), ("slow", 26.0, 2.0, 400.0, 1.0)), Apo
        ),
        ind!(
            "ppo", "Percentage Price Osc", Momentum, Oscillator, false, out! {"ppo":Line}, &[0.0],
            params!(("fast", 12.0, 2.0, 200.0, 1.0), ("slow", 26.0, 2.0, 400.0, 1.0)), Ppo
        ),
        ind!(
            "cmo", "Chande Momentum Osc", Momentum, Oscillator, false, out! {"cmo":Line}, &[-50.0, 50.0],
            params!(("period", 14.0, 2.0, 200.0, 1.0)), Cmo
        ),
        ind!("bop", "Balance of Power", Momentum, Oscillator, false, out! {"bop":Line}, &[0.0], &[], Bop),
        ind!(
            "dpo", "Detrended Price Osc", Momentum, Oscillator, false, out! {"dpo":Line}, &[0.0],
            params!(("period", 20.0, 2.0, 400.0, 1.0)), Dpo
        ),
        ind!(
            "trix", "TRIX", Momentum, Oscillator, false, out! {"trix":Line}, &[0.0],
            params!(("period", 18.0, 2.0, 200.0, 1.0)), Trix
        ),
        ind!(
            "tsi", "True Strength Index", Momentum, Oscillator, false, out! {"tsi":Line}, &[0.0],
            params!(("long", 25.0, 2.0, 400.0, 1.0), ("short", 13.0, 2.0, 200.0, 1.0)), Tsi
        ),
        ind!(
            "smi_ergodic", "SMI Ergodic", Momentum, Oscillator, false, out! {"smi":Line, "signal":Line}, &[0.0],
            params!(("long", 20.0, 2.0, 400.0, 1.0), ("short", 5.0, 2.0, 200.0, 1.0), ("signal", 5.0, 2.0, 200.0, 1.0)),
            SmiErgodic
        ),
        ind!(
            "coppock", "Coppock Curve", Momentum, Oscillator, false, out! {"coppock":Line}, &[0.0],
            params!(
                ("wma_p", 10.0, 2.0, 100.0, 1.0),
                ("roc_long", 14.0, 2.0, 200.0, 1.0),
                ("roc_short", 11.0, 2.0, 200.0, 1.0)
            ),
            Coppock
        ),
        ind!(
            "kst", "Know Sure Thing", Momentum, Oscillator, false, out! {"kst":Line, "signal":Line}, &[0.0],
            params!(
                ("roc1", 10.0, 1.0, 200.0, 1.0),
                ("sma1", 10.0, 2.0, 200.0, 1.0),
                ("roc2", 15.0, 1.0, 200.0, 1.0),
                ("sma2", 10.0, 2.0, 200.0, 1.0),
                ("roc3", 20.0, 1.0, 200.0, 1.0),
                ("sma3", 10.0, 2.0, 200.0, 1.0),
                ("roc4", 30.0, 1.0, 200.0, 1.0),
                ("sma4", 15.0, 2.0, 200.0, 1.0),
                ("signal", 9.0, 2.0, 200.0, 1.0)
            ),
            Kst
        ),
        ind!(
            "aroon", "Aroon", Momentum, Oscillator, false, out! {"aroon_up":Line, "aroon_down":Line}, &[],
            params!(("period", 14.0, 2.0, 400.0, 1.0)), Aroon
        ),
        ind!(
            "aroonosc", "Aroon Oscillator", Momentum, Oscillator, false, out! {"aroonosc":Line}, &[0.0],
            params!(("period", 14.0, 2.0, 400.0, 1.0)), Aroonosc
        ),
        ind!(
            "adx", "ADX", Momentum, Oscillator, false, out! {"adx":Line, "plus_di":Line, "minus_di":Line},
            &[20.0, 25.0], params!(("period", 14.0, 2.0, 100.0, 1.0)), Adx
        ),
        ind!(
            "adxr", "ADXR", Momentum, Oscillator, false, out! {"adxr":Line}, &[],
            params!(("period", 14.0, 2.0, 100.0, 1.0)), Adxr
        ),
        ind!(
            "elder_ray", "Elder Ray", Momentum, Oscillator, false, out! {"bull_power":Line, "bear_power":Line}, &[0.0],
            params!(("period", 13.0, 2.0, 200.0, 1.0)), ElderRay
        ),
        ind!(
            "stochf", "Fast Stochastic", Momentum, Oscillator, false, out! {"%K":Line, "%D":Line}, &[20.0, 80.0],
            params!(("k", 14.0, 2.0, 100.0, 1.0), ("d", 3.0, 1.0, 50.0, 1.0)), Stochf
        ),
        ind!(
            "stochrsi", "Stochastic RSI", Momentum, Oscillator, false, out! {"%K":Line, "%D":Line}, &[20.0, 80.0],
            params!(("rsi_p", 14.0, 2.0, 100.0, 1.0), ("k", 14.0, 2.0, 100.0, 1.0), ("d", 3.0, 1.0, 50.0, 1.0)),
            Stochrsi
        ),
        ind!(
            "ultosc", "Ultimate Oscillator", Momentum, Oscillator, false, out! {"ultosc":Line}, &[30.0, 70.0],
            params!(("p1", 7.0, 2.0, 100.0, 1.0), ("p2", 14.0, 2.0, 200.0, 1.0), ("p3", 28.0, 2.0, 400.0, 1.0)), Ultosc
        ),
        ind!(
            "vortex", "Vortex", Momentum, Oscillator, false, out! {"vi_plus":Line, "vi_minus":Line}, &[],
            params!(("period", 14.0, 2.0, 200.0, 1.0)), Vortex
        ),
        ind!(
            "chande_kroll_stop", "Chande Kroll Stop", Momentum, Overlay, false,
            out! {"long_stop":Line, "short_stop":Line}, &[],
            params!(("p", 10.0, 2.0, 200.0, 1.0), ("x", 1.0, 1.0, 10.0, 1.0), ("q", 9.0, 2.0, 200.0, 1.0)),
            ChandeKrollStop
        ),
        ind!(
            "asi", "Accumulative Swing Index", Momentum, Oscillator, false, out! {"asi":Line}, &[0.0],
            params!(("limit", 1.0, 0.1, 10.0, 0.1)), Asi
        ),
        ind!(
            "fisher", "Fisher Transform", Momentum, Oscillator, false, out! {"fisher":Line, "trigger":Line}, &[0.0],
            params!(("period", 9.0, 2.0, 100.0, 1.0)), Fisher
        ),
        ind!(
            "connors_rsi", "Connors RSI", Momentum, Oscillator, false, out! {"crsi":Line}, &[30.0, 70.0],
            params!(
                ("rsi_p", 3.0, 2.0, 100.0, 1.0),
                ("streak_p", 2.0, 2.0, 100.0, 1.0),
                ("rank_p", 100.0, 10.0, 500.0, 1.0)
            ),
            ConnorsRsi
        ),
        ind!(
            "relative_vigor", "Relative Vigor Index", Momentum, Oscillator, false, out! {"rvgi":Line, "signal":Line},
            &[0.0], params!(("period", 10.0, 2.0, 200.0, 1.0)), RelativeVigor
        ),
        ind!("ac", "Accelerator Oscillator", Momentum, Oscillator, false, out! {"ac":Histogram}, &[0.0], &[], Ac),
    ]
}
