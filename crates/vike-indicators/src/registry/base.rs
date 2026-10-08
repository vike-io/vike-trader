//! Registry rows — the base set: 17 rows (interleaved categories) and their ParamSpecs.
use super::*;

// Period-type ParamSpecs share one range: any positive integer count of bars
// up to 1000 (a generous ceiling — no base-set indicator is meaningfully used
// beyond a few hundred), stepping by whole bars.
const SMA_PARAMS: &[ParamSpec] = params!(("length", 20.0, 1.0, 1000.0, 1.0));
const EMA_PARAMS: &[ParamSpec] = params!(("length", 20.0, 1.0, 1000.0, 1.0));
const WMA_PARAMS: &[ParamSpec] = params!(("length", 20.0, 1.0, 1000.0, 1.0));
// Bollinger's field is `m`; Keltner's is `mult` — both display as "mult" here
// (T7a inconsistency reconciled at the display-name layer only).
const BOLLINGER_PARAMS: &[ParamSpec] =
    params!(("length", 20.0, 1.0, 1000.0, 1.0), ("mult", 2.0, 0.1, 10.0, 0.1),);
const DONCHIAN_PARAMS: &[ParamSpec] = params!(("length", 20.0, 1.0, 1000.0, 1.0));
const KELTNER_PARAMS: &[ParamSpec] = params!(
    ("ema_length", 20.0, 1.0, 1000.0, 1.0),
    ("atr_length", 10.0, 1.0, 1000.0, 1.0),
    ("mult", 2.0, 0.1, 10.0, 0.1),
);
const VWAP_PARAMS: &[ParamSpec] = &[];
// PSAR's step/max_af are small positive multipliers, not periods: step ranges
// over the classic Wellesley-Wilder 0.01-0.20 neighbourhood with headroom;
// max_af caps it below 1.0 (an af >= 1 makes the SAR jump straight to the EP).
const PSAR_PARAMS: &[ParamSpec] =
    params!(("step", 0.02, 0.001, 0.5, 0.001), ("max_af", 0.20, 0.05, 1.0, 0.01),);
const RSI_PARAMS: &[ParamSpec] = params!(("length", 14.0, 1.0, 1000.0, 1.0));
const MACD_PARAMS: &[ParamSpec] = params!(
    ("fast_length", 12.0, 1.0, 1000.0, 1.0),
    ("slow_length", 26.0, 1.0, 1000.0, 1.0),
    ("signal_length", 9.0, 1.0, 1000.0, 1.0),
);
const STOCHASTIC_PARAMS: &[ParamSpec] = params!(
    ("length", 14.0, 1.0, 1000.0, 1.0),
    ("smooth_k", 3.0, 1.0, 1000.0, 1.0),
    ("smooth_d", 3.0, 1.0, 1000.0, 1.0),
);
const ATR_PARAMS: &[ParamSpec] = params!(("length", 14.0, 1.0, 1000.0, 1.0));
const CCI_PARAMS: &[ParamSpec] = params!(("length", 20.0, 1.0, 1000.0, 1.0));
const ROC_PARAMS: &[ParamSpec] = params!(("length", 10.0, 1.0, 1000.0, 1.0));
const WILLIAMS_PARAMS: &[ParamSpec] = params!(("length", 14.0, 1.0, 1000.0, 1.0));
const OBV_PARAMS: &[ParamSpec] = &[];
const AWESOME_PARAMS: &[ParamSpec] =
    params!(("fast_length", 5.0, 1.0, 1000.0, 1.0), ("slow_length", 34.0, 1.0, 1000.0, 1.0),);

#[rustfmt::skip]
pub(super) fn rows() -> Vec<IndicatorMeta> {
    use Category::*;
    use RenderKind::*;
    vec![
        ind!("sma", "Simple MA", Overlap, Overlay, false, out! {"sma":Line}, &[], SMA_PARAMS, Sma),
        ind!("ema", "Exponential MA", Overlap, Overlay, false, out! {"ema":Line}, &[], EMA_PARAMS, Ema),
        ind!("wma", "Weighted MA", Overlap, Overlay, false, out! {"wma":Line}, &[], WMA_PARAMS, Wma),
        ind!(
            "bollinger", "Bollinger Bands", Volatility, Overlay, false, out! {"upper":Band,"mid":Line,"lower":Band},
            &[], BOLLINGER_PARAMS, Bollinger
        ),
        ind!(
            "donchian", "Donchian Channel", Volatility, Overlay, false, out! {"upper":Band,"mid":Line,"lower":Band},
            &[], DONCHIAN_PARAMS, Donchian
        ),
        ind!(
            "keltner", "Keltner Channel", Volatility, Overlay, false, out! {"upper":Band,"mid":Line,"lower":Band}, &[],
            KELTNER_PARAMS, Keltner
        ),
        IndicatorMeta {
            name: "vwap",
            pretty: "VWAP (session)",
            category: Volume,
            kind: Overlay,
            outputs: out! {"vwap":Line},
            bands: &[],
            batch_only: false,
            make: mk::<Vwap>,
            params: VWAP_PARAMS,
            make_with: |_| Box::new(Vwap::new()),
            factory: None,
        },
        ind!("psar", "Parabolic SAR", Overlap, Overlay, false, out! {"psar":Dots}, &[], PSAR_PARAMS, Psar),
        ind!("rsi", "RSI", Momentum, Oscillator, false, out! {"rsi":Line}, &[30.0, 50.0, 70.0], RSI_PARAMS, Rsi),
        ind!(
            "macd", "MACD", Momentum, Oscillator, false, out! {"macd":Line,"signal":Line,"hist":Histogram}, &[0.0],
            MACD_PARAMS, Macd
        ),
        ind!(
            "stochastic", "Stochastic", Momentum, Oscillator, false, out! {"%K":Line,"%D":Line}, &[20.0, 80.0],
            STOCHASTIC_PARAMS, Stochastic
        ),
        ind!("atr", "ATR", Volatility, Oscillator, false, out! {"atr":Line}, &[], ATR_PARAMS, Atr),
        ind!("cci", "CCI", Momentum, Oscillator, false, out! {"cci":Line}, &[-100.0, 100.0], CCI_PARAMS, Cci),
        ind!("roc", "Rate of Change", Momentum, Oscillator, false, out! {"roc":Line}, &[0.0], ROC_PARAMS, Roc),
        ind!(
            "williams", "Williams %R", Momentum, Oscillator, false, out! {"%R":Line}, &[-80.0, -20.0], WILLIAMS_PARAMS,
            Williams
        ),
        IndicatorMeta {
            name: "obv",
            pretty: "On-Balance Volume",
            category: Volume,
            kind: Oscillator,
            outputs: out! {"obv":Line},
            bands: &[],
            batch_only: false,
            make: mk::<Obv>,
            params: OBV_PARAMS,
            make_with: |_| Box::new(Obv::new()),
            factory: None,
        },
        ind!(
            "awesome", "Awesome Oscillator", Momentum, Oscillator, false, out! {"ao":Histogram}, &[0.0], AWESOME_PARAMS,
            Awesome
        ),
    ]
}
