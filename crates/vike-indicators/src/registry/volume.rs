//! Registry rows — volume (volume.py).
use super::*;

#[rustfmt::skip]
pub(super) fn rows() -> Vec<IndicatorMeta> {
    use Category::*;
    use RenderKind::*;
    vec![
        // ---- volume (volume.py) ----
        indicator!("ad", "Accumulation/Distribution", Volume, Oscillator, false, out! {"ad":Line}, &[], &[], Ad),
        indicator!(
            "adosc", "Chaikin A/D Oscillator", Volume, Oscillator, false, out! {"adosc":Line}, &[0.0],
            params!(("fast", 3.0, 2.0, 50.0, 1.0), ("slow", 10.0, 2.0, 200.0, 1.0)), Adosc
        ),
        indicator!(
            "cmf", "Chaikin Money Flow", Volume, Oscillator, false, out! {"cmf":Line}, &[0.0],
            params!(("period", 20.0, 2.0, 200.0, 1.0)), Cmf
        ),
        indicator!(
            "efi", "Elder Force Index", Volume, Oscillator, false, out! {"efi":Line}, &[0.0],
            params!(("period", 13.0, 2.0, 200.0, 1.0)), Efi
        ),
        indicator!(
            "eom", "Ease of Movement", Volume, Oscillator, false, out! {"eom":Line}, &[0.0],
            params!(("period", 14.0, 2.0, 200.0, 1.0)), Eom
        ),
        indicator!(
            "kvo", "Klinger Volume Oscillator", Volume, Oscillator, false, out! {"kvo":Line, "signal":Line}, &[0.0],
            params!(
                ("fast", 34.0, 2.0, 200.0, 1.0),
                ("slow", 55.0, 2.0, 500.0, 1.0),
                ("signal", 13.0, 2.0, 100.0, 1.0)
            ),
            Kvo
        ),
        indicator!(
            "mfi", "Money Flow Index", Volume, Oscillator, false, out! {"mfi":Line}, &[20.0, 80.0],
            params!(("period", 14.0, 2.0, 100.0, 1.0)), Mfi
        ),
        indicator!(
            "net_volume", "Net Volume", Volume, Oscillator, false, out! {"net_volume":Histogram}, &[0.0], &[], NetVolume
        ),
        indicator!("nvi", "Negative Volume Index", Volume, Oscillator, false, out! {"nvi":Line}, &[], &[], Nvi),
        indicator!("pvi", "Positive Volume Index", Volume, Oscillator, false, out! {"pvi":Line}, &[], &[], Pvi),
        indicator!("pvt", "Price Volume Trend", Volume, Oscillator, false, out! {"pvt":Line}, &[], &[], Pvt),
        indicator!(
            "volume_osc", "Volume Oscillator", Volume, Oscillator, false, out! {"volume_osc":Line}, &[0.0],
            params!(("short", 5.0, 2.0, 50.0, 1.0), ("long", 10.0, 2.0, 200.0, 1.0)), VolumeOsc
        ),
    ]
}
